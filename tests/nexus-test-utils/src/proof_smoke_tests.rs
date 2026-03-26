// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! C-2: Proof surface smoke tests for release rehearsal.
//!
//! This module provides a self-contained drill that validates the proof
//! surface is ready for release. It exercises the full lifecycle:
//!
//! 1. Proof endpoint availability (commitment tracker → backend → DTOs)
//! 2. State proof roundtrip under churn (inserts, updates, deletes)
//! 3. Snapshot manifest sign → verify → tamper-reject cycle
//! 4. Epoch boundary consistency after state mutations
//! 5. Batch proof verification with mixed present/absent keys
//!
//! All tests are deterministic and self-contained (no external services).
//! Intended to run in CI as part of the Proof-Surface gate (Gate 3e).

use nexus_crypto::{FalconSigner, Signer};
use nexus_node::backends::LiveStateProofBackend;
use nexus_node::commitment_tracker::{new_shared_tracker, CommitmentTracker, StateChangeEntry};
use nexus_node::snapshot_signing::{sign_manifest, verify_manifest};
use nexus_primitives::Blake3Digest;
use nexus_rpc::StateProofBackend;
use nexus_storage::commitment::Blake3SmtCommitment;
use nexus_storage::rocks::SnapshotManifest;
use nexus_storage::traits::StateCommitment;

// ── Helpers ─────────────────────────────────────────────────────────────

fn insert(tracker: &mut CommitmentTracker, entries: &[(&[u8], &[u8])]) {
    let changes: Vec<StateChangeEntry<'_>> = entries
        .iter()
        .map(|(k, v)| StateChangeEntry {
            key: k,
            value: Some(v),
        })
        .collect();
    tracker.apply_state_changes(&changes);
}

fn delete(tracker: &mut CommitmentTracker, key: &[u8]) {
    tracker.apply_state_changes(&[StateChangeEntry { key, value: None }]);
}

fn make_manifest() -> SnapshotManifest {
    SnapshotManifest {
        version: 1,
        block_height: 100,
        entry_count: 500,
        total_bytes: 12345,
        content_hash: Some([0xAB; 32]),
        signature: None,
        signer_public_key: None,
        signature_scheme: None,
        chain_id: Some("nexus-devnet-7".to_string()),
        epoch: Some(5),
        created_at_ms: Some(1_700_000_000_000),
        previous_manifest_hash: None,
    }
}

// ── Smoke 1: Proof endpoint availability ────────────────────────────────

/// Verifies the LiveStateProofBackend is wired correctly and returns
/// non-zero commitment info after state inserts.
#[test]
fn smoke_proof_endpoint_available() {
    let shared = new_shared_tracker();
    {
        let mut t = shared.write().unwrap();
        insert(&mut t, &[(b"k1", b"v1"), (b"k2", b"v2"), (b"k3", b"v3")]);
    }

    let backend = LiveStateProofBackend::new(shared);

    // commitment_info should succeed and report 3 entries.
    let info = backend.commitment_info().expect("commitment_info failed");
    assert_eq!(info.entry_count, 3);
    assert_eq!(info.updates_applied, 3);
    assert!(!info.commitment_root.is_empty());
    let root_bytes = hex::decode(&info.commitment_root).expect("root should be valid hex");
    assert_ne!(root_bytes, vec![0u8; 32], "root must be non-zero");

    // commitment_root direct call.
    let root = backend.commitment_root().expect("commitment_root failed");
    assert_ne!(root, Blake3Digest::ZERO);

    // prove_key should work.
    let (val, proof) = backend.prove_key(b"k1").expect("prove_key failed");
    assert_eq!(val.as_deref(), Some(b"v1".as_slice()));

    // verify proof against root.
    Blake3SmtCommitment::verify_proof(&root, b"k1", Some(b"v1"), &proof)
        .expect("proof should verify");
}

// ── Smoke 2: State churn proof roundtrip ────────────────────────────────

/// Exercises the full proof lifecycle under state churn: insert → update
/// → delete → verify at each step.
#[test]
fn smoke_proof_under_state_churn() {
    let mut tracker = CommitmentTracker::new();

    // Step 1: Insert 5 entries.
    insert(
        &mut tracker,
        &[
            (b"alpha", b"1"),
            (b"beta", b"2"),
            (b"gamma", b"3"),
            (b"delta", b"4"),
            (b"epsilon", b"5"),
        ],
    );
    let root_v1 = tracker.commitment_root();

    for (key, val) in [
        (b"alpha".as_slice(), b"1".as_slice()),
        (b"beta", b"2"),
        (b"gamma", b"3"),
        (b"delta", b"4"),
        (b"epsilon", b"5"),
    ] {
        let (v, p) = tracker.prove_key(key).unwrap();
        assert_eq!(v.as_deref(), Some(val));
        Blake3SmtCommitment::verify_proof(&root_v1, key, Some(val), &p).unwrap();
    }

    // Step 2: Update two entries.
    insert(&mut tracker, &[(b"alpha", b"one"), (b"gamma", b"three")]);
    let root_v2 = tracker.commitment_root();
    assert_ne!(root_v1, root_v2);

    let (v, p) = tracker.prove_key(b"alpha").unwrap();
    assert_eq!(v.as_deref(), Some(b"one".as_slice()));
    Blake3SmtCommitment::verify_proof(&root_v2, b"alpha", Some(b"one"), &p).unwrap();

    // Step 3: Delete one entry.
    delete(&mut tracker, b"delta");
    let root_v3 = tracker.commitment_root();
    assert_ne!(root_v2, root_v3);

    let (v, p) = tracker.prove_key(b"delta").unwrap();
    assert!(v.is_none(), "deleted key should be absent");
    Blake3SmtCommitment::verify_proof(&root_v3, b"delta", None, &p).unwrap();

    // Remaining entries still provable.
    let (v, _) = tracker.prove_key(b"beta").unwrap();
    assert_eq!(v.as_deref(), Some(b"2".as_slice()));
}

// ── Smoke 3: Snapshot sign → verify → tamper drill ──────────────────────

/// The standard snapshot sign/verify/tamper-reject drill for release rehearsal.
#[test]
fn smoke_snapshot_sign_verify_tamper_drill() {
    let (sk, vk) = FalconSigner::generate_keypair();

    // Sign a manifest.
    let mut manifest = make_manifest();
    sign_manifest(&mut manifest, &sk, &vk);

    assert!(manifest.signature.is_some(), "signature must be present");
    assert_eq!(manifest.signature_scheme.as_deref(), Some("falcon-512"));
    assert!(
        manifest.signer_public_key.is_some(),
        "pubkey must be present"
    );

    // Verify succeeds with correct key.
    verify_manifest(&manifest, &vk).expect("valid manifest must verify");

    // ── Tamper: content_hash ──
    let mut tampered_content = manifest.clone();
    tampered_content.content_hash = Some([0xFF; 32]);
    assert!(
        verify_manifest(&tampered_content, &vk).is_err(),
        "tampered content_hash must be rejected"
    );

    // ── Tamper: block_height ──
    let mut tampered_height = manifest.clone();
    tampered_height.block_height = 999;
    assert!(
        verify_manifest(&tampered_height, &vk).is_err(),
        "tampered block_height must be rejected"
    );

    // ── Tamper: entry_count ──
    let mut tampered_count = manifest.clone();
    tampered_count.entry_count = 1;
    assert!(
        verify_manifest(&tampered_count, &vk).is_err(),
        "tampered entry_count must be rejected"
    );

    // ── Tamper: chain_id ──
    let mut tampered_chain = manifest.clone();
    tampered_chain.chain_id = Some("evil-chain".to_string());
    assert!(
        verify_manifest(&tampered_chain, &vk).is_err(),
        "tampered chain_id must be rejected"
    );

    // ── Tamper: epoch ──
    let mut tampered_epoch = manifest.clone();
    tampered_epoch.epoch = Some(999);
    assert!(
        verify_manifest(&tampered_epoch, &vk).is_err(),
        "tampered epoch must be rejected"
    );

    // ── Wrong key ──
    let (_, wrong_vk) = FalconSigner::generate_keypair();
    assert!(
        verify_manifest(&manifest, &wrong_vk).is_err(),
        "wrong verification key must be rejected"
    );
}

// ── Smoke 4: Epoch boundary consistency drill ───────────────────────────

/// Runs 5 epochs of state changes, performing epoch_boundary_check at each.
/// Verifies commitment root grows monotonically (non-repeating) and
/// proofs remain valid against their epoch's root.
#[test]
fn smoke_epoch_boundary_consistency() {
    let mut tracker = CommitmentTracker::new();
    let mut roots: Vec<Blake3Digest> = Vec::new();

    for epoch in 0..5u64 {
        // Insert epoch-specific data.
        let key = format!("epoch_{epoch}").into_bytes();
        let val = format!("data_{epoch}").into_bytes();
        insert(&mut tracker, &[(&key, &val)]);

        let root = tracker.commitment_root();
        tracker
            .epoch_boundary_check()
            .expect("epoch check should pass");

        // Root must be unique per epoch.
        assert!(
            !roots.contains(&root),
            "root must be unique at epoch {epoch}"
        );
        roots.push(root);

        // Proof for this epoch's key.
        let (v, p) = tracker.prove_key(&key).unwrap();
        assert_eq!(v.as_deref(), Some(val.as_slice()));
        Blake3SmtCommitment::verify_proof(&root, &key, Some(&val), &p).unwrap();
    }

    assert_eq!(tracker.epoch_checks_passed(), 5);
    assert_eq!(roots.len(), 5);
}

// ── Smoke 5: Batch proof with mixed present/absent keys ─────────────────

/// Verifies batch proof returns correct results for a mix of existing
/// and non-existing keys, and all proofs verify individually.
#[test]
fn smoke_batch_proof_mixed_keys() {
    let shared = new_shared_tracker();
    {
        let mut t = shared.write().unwrap();
        insert(
            &mut t,
            &[
                (b"a", b"1"),
                (b"b", b"2"),
                (b"c", b"3"),
                (b"d", b"4"),
                (b"e", b"5"),
            ],
        );
    }

    let backend = LiveStateProofBackend::new(shared);
    let root = backend.commitment_root().unwrap();

    // Mix of present and absent keys.
    let keys = vec![
        b"a".to_vec(),
        b"missing1".to_vec(),
        b"c".to_vec(),
        b"missing2".to_vec(),
        b"e".to_vec(),
    ];
    let proofs = backend.prove_keys(&keys).expect("batch prove_keys");
    assert_eq!(proofs.len(), 5);

    // a → present.
    assert_eq!(proofs[0].0.as_deref(), Some(b"1".as_slice()));
    Blake3SmtCommitment::verify_proof(&root, b"a", Some(b"1"), &proofs[0].1).unwrap();

    // missing1 → absent.
    assert!(proofs[1].0.is_none());
    Blake3SmtCommitment::verify_proof(&root, b"missing1", None, &proofs[1].1).unwrap();

    // c → present.
    assert_eq!(proofs[2].0.as_deref(), Some(b"3".as_slice()));
    Blake3SmtCommitment::verify_proof(&root, b"c", Some(b"3"), &proofs[2].1).unwrap();

    // missing2 → absent.
    assert!(proofs[3].0.is_none());
    Blake3SmtCommitment::verify_proof(&root, b"missing2", None, &proofs[3].1).unwrap();

    // e → present.
    assert_eq!(proofs[4].0.as_deref(), Some(b"5".as_slice()));
    Blake3SmtCommitment::verify_proof(&root, b"e", Some(b"5"), &proofs[4].1).unwrap();
}

// ── Smoke 6: Snapshot manifest with provenance chain ────────────────────

/// Verifies that provenance fields (chain_id, epoch, timestamp,
/// previous_manifest_hash) are included in the signed content.
#[test]
fn smoke_snapshot_provenance_chain() {
    let (sk, vk) = FalconSigner::generate_keypair();

    // First manifest.
    let mut m1 = make_manifest();
    m1.epoch = Some(1);
    m1.previous_manifest_hash = None;
    sign_manifest(&mut m1, &sk, &vk);
    verify_manifest(&m1, &vk).unwrap();

    // Second manifest references the first.
    let m1_hash = {
        let bytes = m1.signable_bytes();
        let h = blake3::hash(&bytes);
        let mut arr = [0u8; 32];
        arr.copy_from_slice(h.as_bytes());
        arr
    };

    let mut m2 = make_manifest();
    m2.epoch = Some(2);
    m2.block_height = 200;
    m2.previous_manifest_hash = Some(m1_hash);
    sign_manifest(&mut m2, &sk, &vk);
    verify_manifest(&m2, &vk).unwrap();

    // The two manifests have different signable bytes.
    assert_ne!(m1.signable_bytes(), m2.signable_bytes());
}
