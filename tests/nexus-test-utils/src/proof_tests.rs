// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Proof roundtrip integration tests (v0.1.5 — proof surface hardening).
//!
//! Verifies the full lifecycle of state proofs:
//!
//! - Commitment tracker produces non-zero roots after inserts.
//! - Single key and batch proofs can be verified against the root.
//! - Absence proofs are valid for keys that do not exist.
//! - [`LiveStateProofBackend`] correctly delegates to the tracker.
//! - Epoch boundary consistency check passes after state changes.
//! - Snapshot-signed manifests round-trip (sign → verify).

use nexus_node::backends::LiveStateProofBackend;
use nexus_node::commitment_tracker::{
    new_shared_tracker, CommitmentTracker, PersistentCommitmentBackend, StateChangeEntry,
};
use nexus_primitives::Blake3Digest;
use nexus_rpc::StateProofBackend;
use nexus_storage::commitment::Blake3SmtCommitment;
use nexus_storage::traits::StateCommitment;
use nexus_storage::MemoryStore;

// ── Helpers ─────────────────────────────────────────────────────────────

fn insert_entries(tracker: &mut CommitmentTracker, entries: &[(&[u8], &[u8])]) {
    let changes: Vec<StateChangeEntry<'_>> = entries
        .iter()
        .map(|(k, v)| StateChangeEntry {
            key: k,
            value: Some(v),
        })
        .collect();
    tracker.apply_state_changes(&changes);
}

// ── Tests ───────────────────────────────────────────────────────────────

#[test]
fn proof_roundtrip_single_key() {
    let mut tracker = CommitmentTracker::new();
    insert_entries(
        &mut tracker,
        &[(b"alpha", b"one"), (b"beta", b"two"), (b"gamma", b"three")],
    );

    let root = tracker.commitment_root();
    assert_ne!(
        root,
        Blake3Digest::ZERO,
        "root must be non-zero after inserts"
    );

    // Prove and verify each key.
    for (key, expected_val) in [
        (b"alpha".as_slice(), b"one".as_slice()),
        (b"beta", b"two"),
        (b"gamma", b"three"),
    ] {
        let (val, proof) = tracker.prove_key(key).expect("prove_key should not fail");
        assert_eq!(val.as_deref(), Some(expected_val));

        Blake3SmtCommitment::verify_proof(&root, key, Some(expected_val), &proof)
            .expect("proof verification must succeed");
    }
}

#[test]
fn proof_roundtrip_absence() {
    let mut tracker = CommitmentTracker::new();
    insert_entries(&mut tracker, &[(b"exists", b"yes")]);

    let root = tracker.commitment_root();

    let (val, proof) = tracker
        .prove_key(b"missing")
        .expect("prove_key for absent key");
    assert!(val.is_none(), "absent key should have no value");

    Blake3SmtCommitment::verify_proof(&root, b"missing", None, &proof)
        .expect("absence proof must verify");
}

#[test]
fn proof_roundtrip_batch_keys() {
    let mut tracker = CommitmentTracker::new();
    insert_entries(
        &mut tracker,
        &[
            (b"k1", b"v1"),
            (b"k2", b"v2"),
            (b"k3", b"v3"),
            (b"k4", b"v4"),
        ],
    );

    let root = tracker.commitment_root();
    let keys: Vec<&[u8]> = vec![b"k1", b"k2", b"k4", b"missing"];
    let proofs = tracker.prove_keys(&keys).expect("batch prove_keys");

    assert_eq!(proofs.len(), 4);

    // k1 → present
    assert_eq!(proofs[0].0.as_deref(), Some(b"v1".as_slice()));
    Blake3SmtCommitment::verify_proof(&root, b"k1", Some(b"v1"), &proofs[0].1).unwrap();

    // k2 → present
    assert_eq!(proofs[1].0.as_deref(), Some(b"v2".as_slice()));
    Blake3SmtCommitment::verify_proof(&root, b"k2", Some(b"v2"), &proofs[1].1).unwrap();

    // k4 → present
    assert_eq!(proofs[2].0.as_deref(), Some(b"v4".as_slice()));
    Blake3SmtCommitment::verify_proof(&root, b"k4", Some(b"v4"), &proofs[2].1).unwrap();

    // missing → absent
    assert!(proofs[3].0.is_none());
    Blake3SmtCommitment::verify_proof(&root, b"missing", None, &proofs[3].1).unwrap();
}

#[test]
fn proof_after_update_reflects_new_value() {
    let mut tracker = CommitmentTracker::new();
    insert_entries(&mut tracker, &[(b"key", b"old_value")]);
    let root_v1 = tracker.commitment_root();

    // Update the key.
    insert_entries(&mut tracker, &[(b"key", b"new_value")]);
    let root_v2 = tracker.commitment_root();
    assert_ne!(root_v1, root_v2, "root must change after update");

    // Proof against the new root should have the new value.
    let (val, proof) = tracker.prove_key(b"key").unwrap();
    assert_eq!(val.as_deref(), Some(b"new_value".as_slice()));
    Blake3SmtCommitment::verify_proof(&root_v2, b"key", Some(b"new_value"), &proof).unwrap();
}

#[test]
fn proof_after_delete_reflects_absence() {
    let mut tracker = CommitmentTracker::new();
    insert_entries(&mut tracker, &[(b"a", b"1"), (b"b", b"2")]);

    // Delete key "a".
    tracker.apply_state_changes(&[StateChangeEntry {
        key: b"a",
        value: None,
    }]);

    let root = tracker.commitment_root();
    let (val, proof) = tracker.prove_key(b"a").unwrap();
    assert!(val.is_none(), "deleted key should be absent");
    Blake3SmtCommitment::verify_proof(&root, b"a", None, &proof).unwrap();

    // "b" should still be provable.
    let (val_b, proof_b) = tracker.prove_key(b"b").unwrap();
    assert_eq!(val_b.as_deref(), Some(b"2".as_slice()));
    Blake3SmtCommitment::verify_proof(&root, b"b", Some(b"2"), &proof_b).unwrap();
}

#[test]
fn epoch_boundary_check_passes_after_insertions() {
    let mut tracker = CommitmentTracker::new();
    insert_entries(&mut tracker, &[(b"x", b"y")]);
    tracker
        .epoch_boundary_check()
        .expect("epoch check should pass");
    assert_eq!(tracker.epoch_checks_passed(), 1);
}

#[test]
fn epoch_boundary_check_passes_on_empty_tree() {
    let mut tracker = CommitmentTracker::new();
    tracker
        .epoch_boundary_check()
        .expect("epoch check on empty tree should pass");
}

#[test]
fn live_backend_commitment_info() {
    let shared = new_shared_tracker();
    {
        let mut tracker = shared.write().unwrap();
        insert_entries(&mut tracker, &[(b"k1", b"v1"), (b"k2", b"v2")]);
    }

    let backend = LiveStateProofBackend::new(shared);

    let info = backend
        .commitment_info()
        .expect("commitment_info should succeed");
    assert_eq!(info.entry_count, 2);
    assert_eq!(info.updates_applied, 2);

    // Root should be non-zero hex string.
    let root_bytes = hex::decode(&info.commitment_root).expect("valid hex root");
    assert_ne!(root_bytes, vec![0u8; 32]);
}

#[test]
fn live_backend_prove_and_verify() {
    let shared = new_shared_tracker();
    {
        let mut tracker = shared.write().unwrap();
        insert_entries(&mut tracker, &[(b"foo", b"bar"), (b"baz", b"qux")]);
    }

    let backend = LiveStateProofBackend::new(shared);

    let root = backend.commitment_root().expect("commitment_root");
    let (val, proof) = backend.prove_key(b"foo").expect("prove_key");
    assert_eq!(val.as_deref(), Some(b"bar".as_slice()));

    Blake3SmtCommitment::verify_proof(&root, b"foo", Some(b"bar"), &proof)
        .expect("proof from backend must verify");
}

#[test]
fn live_backend_batch_prove() {
    let shared = new_shared_tracker();
    {
        let mut tracker = shared.write().unwrap();
        insert_entries(&mut tracker, &[(b"a", b"1"), (b"b", b"2"), (b"c", b"3")]);
    }

    let backend = LiveStateProofBackend::new(shared);

    let keys = vec![b"a".to_vec(), b"b".to_vec(), b"nonexistent".to_vec()];
    let proofs = backend.prove_keys(&keys).expect("batch prove_keys");
    assert_eq!(proofs.len(), 3);

    let root = backend.commitment_root().unwrap();

    // a → present
    assert_eq!(proofs[0].0.as_deref(), Some(b"1".as_slice()));
    Blake3SmtCommitment::verify_proof(&root, b"a", Some(b"1"), &proofs[0].1).unwrap();

    // b → present
    assert_eq!(proofs[1].0.as_deref(), Some(b"2".as_slice()));
    Blake3SmtCommitment::verify_proof(&root, b"b", Some(b"2"), &proofs[1].1).unwrap();

    // nonexistent → absent
    assert!(proofs[2].0.is_none());
    Blake3SmtCommitment::verify_proof(&root, b"nonexistent", None, &proofs[2].1).unwrap();
}

#[test]
fn snapshot_manifest_sign_verify_roundtrip() {
    use nexus_crypto::{FalconSigner, Signer};
    use nexus_node::snapshot_signing::{sign_manifest, verify_manifest};
    use nexus_storage::rocks::SnapshotManifest;

    let (sk, vk) = FalconSigner::generate_keypair();

    let mut manifest = SnapshotManifest {
        version: 1,
        block_height: 100,
        entry_count: 500,
        total_bytes: 12345,
        content_hash: Some([0xAB; 32]),
        signature: None,
        signer_public_key: None,
        signature_scheme: None,
        chain_id: None,
        epoch: None,
        created_at_ms: None,
        previous_manifest_hash: None,
    };

    sign_manifest(&mut manifest, &sk, &vk);

    assert!(manifest.signature.is_some());
    assert_eq!(manifest.signature_scheme.as_deref(), Some("falcon-512"));
    assert!(manifest.signer_public_key.is_some());

    verify_manifest(&manifest, &vk).expect("roundtrip verification must succeed");
}

#[test]
fn snapshot_manifest_verify_rejects_tampered_content() {
    use nexus_crypto::{FalconSigner, Signer};
    use nexus_node::snapshot_signing::{sign_manifest, verify_manifest};
    use nexus_storage::rocks::SnapshotManifest;

    let (sk, vk) = FalconSigner::generate_keypair();

    let mut manifest = SnapshotManifest {
        version: 1,
        block_height: 100,
        entry_count: 500,
        total_bytes: 12345,
        content_hash: Some([0xAB; 32]),
        signature: None,
        signer_public_key: None,
        signature_scheme: None,
        chain_id: None,
        epoch: None,
        created_at_ms: None,
        previous_manifest_hash: None,
    };

    sign_manifest(&mut manifest, &sk, &vk);

    // Tamper with the content hash.
    manifest.content_hash = Some([0xFF; 32]);

    let result = verify_manifest(&manifest, &vk);
    assert!(result.is_err(), "tampered manifest must fail verification");
}

#[test]
fn persistent_tracker_restores_after_restart() {
    let store = MemoryStore::new();
    let expected_root = {
        let mut tracker = CommitmentTracker::with_persistence(Box::new(
            PersistentCommitmentBackend::new(store.clone(), 8),
        ))
        .expect("persistent tracker init");
        insert_entries(
            &mut tracker,
            &[(b"alpha", b"one"), (b"beta", b"two"), (b"gamma", b"three")],
        );
        tracker.commitment_root()
    };

    let restored =
        CommitmentTracker::with_persistence(Box::new(PersistentCommitmentBackend::new(store, 8)))
            .expect("restore persistent tracker");

    assert_eq!(restored.commitment_root(), expected_root);
    assert_eq!(restored.entry_count(), 3);

    let (value, proof) = restored.prove_key(b"beta").expect("proof after restore");
    assert_eq!(value.as_deref(), Some(b"two".as_slice()));
    Blake3SmtCommitment::verify_proof(&expected_root, b"beta", Some(b"two"), &proof).unwrap();
}

#[test]
fn persistent_tracker_incremental_root_matches_full_recompute() {
    let store = MemoryStore::new();
    let mut tracker =
        CommitmentTracker::with_persistence(Box::new(PersistentCommitmentBackend::new(store, 2)))
            .expect("persistent tracker init");

    tracker
        .try_apply_state_changes(&[
            StateChangeEntry {
                key: b"a",
                value: Some(b"1"),
            },
            StateChangeEntry {
                key: b"c",
                value: Some(b"3"),
            },
            StateChangeEntry {
                key: b"b",
                value: Some(b"2"),
            },
        ])
        .unwrap();
    tracker
        .try_apply_state_changes(&[
            StateChangeEntry {
                key: b"b",
                value: Some(b"20"),
            },
            StateChangeEntry {
                key: b"c",
                value: None,
            },
            StateChangeEntry {
                key: b"d",
                value: Some(b"4"),
            },
        ])
        .unwrap();

    let mut recomputed = Blake3SmtCommitment::new();
    recomputed.update(&[
        (b"a".as_slice(), b"1".as_slice()),
        (b"b", b"20"),
        (b"d", b"4"),
    ]);

    assert_eq!(tracker.commitment_root(), recomputed.root_commitment());
    assert_eq!(tracker.entry_count(), 3);
}

#[test]
fn persistent_tracker_delete_then_reinsert_restores_same_root() {
    let store = MemoryStore::new();
    let mut tracker =
        CommitmentTracker::with_persistence(Box::new(PersistentCommitmentBackend::new(store, 16)))
            .expect("persistent tracker init");

    tracker
        .try_apply_state_changes(&[StateChangeEntry {
            key: b"k",
            value: Some(b"v"),
        }])
        .unwrap();
    let original_root = tracker.commitment_root();

    tracker
        .try_apply_state_changes(&[StateChangeEntry {
            key: b"k",
            value: None,
        }])
        .unwrap();
    tracker
        .try_apply_state_changes(&[StateChangeEntry {
            key: b"k",
            value: Some(b"v"),
        }])
        .unwrap();

    assert_eq!(tracker.commitment_root(), original_root);
}

// ── D-4: Provenance metadata & hash-chain tests ─────────────────────

#[test]
fn manifest_provenance_fields_included_in_signable_bytes() {
    use nexus_storage::rocks::SnapshotManifest;

    let base = SnapshotManifest {
        version: 1,
        block_height: 50,
        entry_count: 10,
        total_bytes: 1000,
        content_hash: Some([0xAA; 32]),
        signature: None,
        signer_public_key: None,
        signature_scheme: None,
        chain_id: None,
        epoch: None,
        created_at_ms: None,
        previous_manifest_hash: None,
    };

    let with_provenance = SnapshotManifest {
        chain_id: Some("nexus-devnet-7".to_string()),
        epoch: Some(3),
        created_at_ms: Some(1_700_000_000_000),
        previous_manifest_hash: Some([0xBB; 32]),
        ..base.clone()
    };

    // Provenance fields must change the signable bytes.
    assert_ne!(
        base.signable_bytes(),
        with_provenance.signable_bytes(),
        "provenance fields must affect signable_bytes()"
    );
}

#[test]
fn manifest_hash_deterministic() {
    use nexus_storage::rocks::SnapshotManifest;

    let manifest = SnapshotManifest {
        version: 1,
        block_height: 100,
        entry_count: 42,
        total_bytes: 9999,
        content_hash: Some([0xCC; 32]),
        signature: None,
        signer_public_key: None,
        signature_scheme: None,
        chain_id: Some("test-chain".to_string()),
        epoch: Some(5),
        created_at_ms: Some(1_700_000_000_000),
        previous_manifest_hash: None,
    };

    let h1 = manifest.manifest_hash();
    let h2 = manifest.manifest_hash();
    assert_eq!(h1, h2, "manifest_hash must be deterministic");
    assert_ne!(h1, [0u8; 32], "hash must not be zero");
}

#[test]
fn manifest_hash_chain_links_consecutive_snapshots() {
    use nexus_storage::rocks::SnapshotManifest;

    let first = SnapshotManifest {
        version: 1,
        block_height: 10,
        entry_count: 5,
        total_bytes: 500,
        content_hash: Some([0x01; 32]),
        signature: None,
        signer_public_key: None,
        signature_scheme: None,
        chain_id: Some("nexus-devnet-7".to_string()),
        epoch: Some(1),
        created_at_ms: Some(1_000),
        previous_manifest_hash: None,
    };

    let first_hash = first.manifest_hash();

    let second = SnapshotManifest {
        version: 1,
        block_height: 20,
        entry_count: 8,
        total_bytes: 800,
        content_hash: Some([0x02; 32]),
        signature: None,
        signer_public_key: None,
        signature_scheme: None,
        chain_id: Some("nexus-devnet-7".to_string()),
        epoch: Some(2),
        created_at_ms: Some(2_000),
        previous_manifest_hash: Some(first_hash),
    };

    // The second manifest's hash must differ from the first.
    assert_ne!(first_hash, second.manifest_hash());

    // The chain link must be verifiable: second.previous_manifest_hash == first.manifest_hash().
    assert_eq!(
        second.previous_manifest_hash.unwrap(),
        first_hash,
        "hash-chain link must reference the previous manifest"
    );
}

#[test]
fn signed_manifest_with_provenance_roundtrip() {
    use nexus_crypto::{FalconSigner, Signer};
    use nexus_node::snapshot_signing::{sign_manifest, verify_manifest};
    use nexus_storage::rocks::SnapshotManifest;

    let (sk, vk) = FalconSigner::generate_keypair();

    let mut manifest = SnapshotManifest {
        version: 1,
        block_height: 200,
        entry_count: 1000,
        total_bytes: 50_000,
        content_hash: Some([0xDD; 32]),
        signature: None,
        signer_public_key: None,
        signature_scheme: None,
        chain_id: Some("nexus-devnet-7".to_string()),
        epoch: Some(10),
        created_at_ms: Some(1_700_000_000_000),
        previous_manifest_hash: Some([0xEE; 32]),
    };

    sign_manifest(&mut manifest, &sk, &vk);

    // Signature covers provenance fields.
    verify_manifest(&manifest, &vk).expect("signed manifest with provenance must verify");

    // Tampering with provenance must break the signature.
    let mut tampered = manifest.clone();
    tampered.epoch = Some(999);
    let err = verify_manifest(&tampered, &vk);
    assert!(err.is_err(), "tampered provenance must fail verification");
}

#[test]
fn export_with_provenance_roundtrip() {
    use nexus_crypto::{FalconSigner, Signer};
    use nexus_node::snapshot_signing::sign_manifest;
    use nexus_storage::rocks::{RocksStore, SnapshotProvenance};
    use nexus_storage::{StateStorage, StorageConfig, WriteBatchOps};

    let tmp = tempfile::tempdir().expect("create temp dir");
    let db_path = tmp.path().join("db");
    let snap_path = tmp.path().join("snap");

    let config = StorageConfig::for_testing(db_path.clone());
    let store = RocksStore::open_at(&db_path, &config).expect("open store");

    // Insert some state.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut batch = store.new_batch();
        batch.put_cf("cf_state", b"key_a".to_vec(), b"val_a".to_vec());
        batch.put_cf("cf_state", b"key_b".to_vec(), b"val_b".to_vec());
        store.write_batch(batch).await.unwrap();
    });

    let provenance = SnapshotProvenance {
        chain_id: "nexus-devnet-7".to_string(),
        epoch: 5,
        previous_manifest_hash: None,
    };

    let mut manifest = store
        .export_state_snapshot_with_provenance(&snap_path, 42, Some(tmp.path()), Some(&provenance))
        .expect("export with provenance");

    // Verify provenance fields are populated.
    assert_eq!(manifest.chain_id.as_deref(), Some("nexus-devnet-7"));
    assert_eq!(manifest.epoch, Some(5));
    assert!(manifest.created_at_ms.is_some());
    assert!(manifest.previous_manifest_hash.is_none());
    // entry_count includes the 2 inserted keys plus any schema metadata.
    assert!(
        manifest.entry_count >= 2,
        "expected at least 2 entries, got {}",
        manifest.entry_count
    );

    // Sign and verify via the file-level helper.
    let (sk, vk) = FalconSigner::generate_keypair();
    sign_manifest(&mut manifest, &sk, &vk);

    // Write the signed manifest back for file-level verification.
    // The export wrote the unsigned manifest; re-read to verify the
    // signature covers provenance.
    // We verify the in-memory manifest here.
    nexus_node::snapshot_signing::verify_manifest(&manifest, &vk)
        .expect("signed export manifest must verify");

    // Verify manifest_hash is non-zero.
    let mh = manifest.manifest_hash();
    assert_ne!(mh, [0u8; 32]);
}

#[test]
fn export_provenance_hash_chain_across_snapshots() {
    use nexus_storage::rocks::{RocksStore, SnapshotProvenance};
    use nexus_storage::{StateStorage, StorageConfig, WriteBatchOps};

    let tmp = tempfile::tempdir().expect("create temp dir");
    let db_path = tmp.path().join("db");
    let snap1_path = tmp.path().join("snap1");
    let snap2_path = tmp.path().join("snap2");

    let config = StorageConfig::for_testing(db_path.clone());
    let store = RocksStore::open_at(&db_path, &config).expect("open store");

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut batch = store.new_batch();
        batch.put_cf("cf_state", b"k1".to_vec(), b"v1".to_vec());
        store.write_batch(batch).await.unwrap();
    });

    // First snapshot (no predecessor).
    let prov1 = SnapshotProvenance {
        chain_id: "test-chain".to_string(),
        epoch: 1,
        previous_manifest_hash: None,
    };
    let m1 = store
        .export_state_snapshot_with_provenance(&snap1_path, 10, Some(tmp.path()), Some(&prov1))
        .expect("export snap1");
    assert!(m1.previous_manifest_hash.is_none());

    // Second snapshot linked to first.
    let prov2 = SnapshotProvenance {
        chain_id: "test-chain".to_string(),
        epoch: 2,
        previous_manifest_hash: Some(m1.manifest_hash()),
    };
    let m2 = store
        .export_state_snapshot_with_provenance(&snap2_path, 20, Some(tmp.path()), Some(&prov2))
        .expect("export snap2");

    // Verify hash-chain link.
    assert_eq!(
        m2.previous_manifest_hash.unwrap(),
        m1.manifest_hash(),
        "second snapshot must link to first via hash chain"
    );

    // The two manifest hashes must differ.
    assert_ne!(m1.manifest_hash(), m2.manifest_hash());
}

#[test]
fn read_snapshot_manifest_roundtrip() {
    use nexus_storage::rocks::{RocksStore, SnapshotProvenance};
    use nexus_storage::{StateStorage, StorageConfig, WriteBatchOps};

    let tmp = tempfile::tempdir().expect("create temp dir");
    let db_path = tmp.path().join("db");
    let snap_path = tmp.path().join("snap");

    let config = StorageConfig::for_testing(db_path.clone());
    let store = RocksStore::open_at(&db_path, &config).expect("open store");

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut batch = store.new_batch();
        batch.put_cf("cf_state", b"x".to_vec(), b"y".to_vec());
        store.write_batch(batch).await.unwrap();
    });

    let prov = SnapshotProvenance {
        chain_id: "read-test".to_string(),
        epoch: 7,
        previous_manifest_hash: Some([0x77; 32]),
    };

    let exported = store
        .export_state_snapshot_with_provenance(&snap_path, 99, Some(tmp.path()), Some(&prov))
        .expect("export");

    // Read back without importing.
    let read_back = RocksStore::read_snapshot_manifest(&snap_path).expect("read manifest");

    assert_eq!(read_back.version, exported.version);
    assert_eq!(read_back.block_height, exported.block_height);
    assert_eq!(read_back.entry_count, exported.entry_count);
    assert_eq!(read_back.content_hash, exported.content_hash);
    assert_eq!(read_back.chain_id, exported.chain_id);
    assert_eq!(read_back.epoch, exported.epoch);
    assert_eq!(
        read_back.previous_manifest_hash,
        exported.previous_manifest_hash
    );
}
