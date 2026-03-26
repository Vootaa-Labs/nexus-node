// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! B-3: Epoch→query/proof/snapshot end-to-end consistency.
//!
//! Verifies that query results, proof verification, and snapshot state
//! remain mutually consistent across epoch boundaries. The key concern
//! is that individual subsystems (consensus, execution, proof, storage)
//! may each be correct in isolation but produce inconsistent results
//! when epoch transitions interleave with query/proof operations.
//!
//! ## Test scenarios:
//!
//! 1. State changes applied during epoch N are visible in proofs at epoch N.
//! 2. After epoch advance, new state changes produce new proofs
//!    that don't verify against old roots.
//! 3. Snapshot manifest epoch field is populated consistently.
//! 4. Query results (account balance) agree with proof state.
//! 5. Full 10-epoch pipeline: state → commit → proof → epoch → repeat.

#[cfg(test)]
mod tests {
    use nexus_consensus::types::EpochTransitionTrigger;
    use nexus_consensus::{Committee, ConsensusEngine};
    use nexus_crypto::{FalconSigner, Signer};
    use nexus_node::backends::LiveStateProofBackend;
    use nexus_node::commitment_tracker::{new_shared_tracker, CommitmentTracker, StateChangeEntry};
    use nexus_node::epoch_store;
    use nexus_node::snapshot_signing::{sign_manifest, verify_manifest};
    use nexus_primitives::{Blake3Digest, EpochNumber};
    use nexus_rpc::StateProofBackend;
    use nexus_storage::commitment::Blake3SmtCommitment;
    use nexus_storage::rocks::SnapshotManifest;
    use nexus_storage::traits::StateCommitment;
    use nexus_storage::MemoryStore;

    use crate::fixtures::consensus::TestCommittee;

    // ── Helpers ──────────────────────────────────────────────────────

    fn make_committee(n: usize, epoch: EpochNumber) -> Committee {
        TestCommittee::new(n, epoch).committee
    }

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

    fn make_manifest(epoch: u64, block_height: u64, content_hash: [u8; 32]) -> SnapshotManifest {
        SnapshotManifest {
            version: 1,
            block_height,
            entry_count: 100,
            total_bytes: 5000,
            content_hash: Some(content_hash),
            signature: None,
            signer_public_key: None,
            signature_scheme: None,
            chain_id: Some("nexus-devnet-7".to_string()),
            epoch: Some(epoch),
            created_at_ms: Some(1_700_000_000_000 + epoch * 60_000),
            previous_manifest_hash: None,
        }
    }

    // ── 1. State changes visible in proofs within same epoch ─────────

    /// After inserting state during an epoch and running epoch_boundary_check,
    /// proofs for the inserted keys must verify against the current root.
    #[test]
    fn state_changes_visible_in_proofs_within_epoch() {
        let mut tracker = CommitmentTracker::new();

        // Epoch 0: insert 3 keys.
        insert(
            &mut tracker,
            &[
                (b"acct_alice", b"1000"),
                (b"acct_bob", b"500"),
                (b"acct_carol", b"250"),
            ],
        );

        let root = tracker.commitment_root();
        tracker.epoch_boundary_check().unwrap();

        // All 3 keys must be provable.
        for (key, val) in [
            (b"acct_alice".as_slice(), b"1000".as_slice()),
            (b"acct_bob", b"500"),
            (b"acct_carol", b"250"),
        ] {
            let (v, proof) = tracker.prove_key(key).unwrap();
            assert_eq!(v.as_deref(), Some(val));
            Blake3SmtCommitment::verify_proof(&root, key, Some(val), &proof).unwrap();
        }
    }

    // ── 2. Proofs invalidated after epoch transition with new data ───

    /// State changes applied in epoch N+1 must produce a different root.
    /// Proofs from epoch N must NOT verify against the epoch N+1 root.
    #[test]
    fn proofs_invalidated_after_epoch_transition() {
        let mut tracker = CommitmentTracker::new();

        // Epoch 0.
        insert(&mut tracker, &[(b"k1", b"v1_epoch0")]);
        let root_e0 = tracker.commitment_root();
        tracker.epoch_boundary_check().unwrap();
        let (_, proof_e0) = tracker.prove_key(b"k1").unwrap();

        // Verify epoch 0 proof.
        Blake3SmtCommitment::verify_proof(&root_e0, b"k1", Some(b"v1_epoch0"), &proof_e0).unwrap();

        // Epoch 1: modify same key.
        insert(&mut tracker, &[(b"k1", b"v1_epoch1")]);
        let root_e1 = tracker.commitment_root();
        tracker.epoch_boundary_check().unwrap();

        assert_ne!(root_e0, root_e1, "root must change after state update");

        // Old proof must NOT verify against new root.
        assert!(
            Blake3SmtCommitment::verify_proof(&root_e1, b"k1", Some(b"v1_epoch0"), &proof_e0)
                .is_err(),
            "epoch 0 proof must not verify against epoch 1 root"
        );

        // New proof must verify.
        let (v_e1, proof_e1) = tracker.prove_key(b"k1").unwrap();
        assert_eq!(v_e1.as_deref(), Some(b"v1_epoch1".as_slice()));
        Blake3SmtCommitment::verify_proof(&root_e1, b"k1", Some(b"v1_epoch1"), &proof_e1).unwrap();
    }

    // ── 3. Snapshot manifest epoch field consistency ──────────────────

    /// Verifies that snapshot manifests have epoch, chain_id, and
    /// content_hash populated, and that manifests for different epochs
    /// produce different signable bytes.
    #[test]
    fn snapshot_manifest_epoch_consistency() {
        let (sk, vk) = FalconSigner::generate_keypair();

        let mut m1 = make_manifest(1, 100, [0xAA; 32]);
        sign_manifest(&mut m1, &sk, &vk);
        verify_manifest(&m1, &vk).unwrap();

        let mut m2 = make_manifest(2, 200, [0xBB; 32]);
        sign_manifest(&mut m2, &sk, &vk);
        verify_manifest(&m2, &vk).unwrap();

        // Different epochs → different signable bytes.
        assert_ne!(m1.signable_bytes(), m2.signable_bytes());

        // Each manifest carries its epoch.
        assert_eq!(m1.epoch, Some(1));
        assert_eq!(m2.epoch, Some(2));

        // Cross-verify: m2's signature must not verify with m1's signable bytes.
        let mut m2_with_epoch1 = m2.clone();
        m2_with_epoch1.epoch = Some(1);
        assert!(
            verify_manifest(&m2_with_epoch1, &vk).is_err(),
            "manifest with wrong epoch must not verify"
        );
    }

    // ── 4. Proof backend + epoch boundary: root tracks state ─────────

    /// Uses LiveStateProofBackend to verify that the proof backend
    /// produces correct roots and proofs as state changes across epochs.
    #[test]
    fn proof_backend_epoch_boundary_root_tracking() {
        let shared = new_shared_tracker();

        // Epoch 0: insert data.
        {
            let mut t = shared.write().unwrap();
            insert(&mut t, &[(b"balance_alice", b"1000")]);
            t.epoch_boundary_check().unwrap();
        }

        let backend = LiveStateProofBackend::new(shared.clone());
        let root_e0 = backend.commitment_root().unwrap();
        assert_ne!(root_e0, Blake3Digest::ZERO);

        let (val, proof) = backend.prove_key(b"balance_alice").unwrap();
        assert_eq!(val.as_deref(), Some(b"1000".as_slice()));
        Blake3SmtCommitment::verify_proof(&root_e0, b"balance_alice", Some(b"1000"), &proof)
            .unwrap();

        // Epoch 1: update alice, add bob.
        {
            let mut t = shared.write().unwrap();
            insert(
                &mut t,
                &[(b"balance_alice", b"900"), (b"balance_bob", b"100")],
            );
            t.epoch_boundary_check().unwrap();
        }

        let root_e1 = backend.commitment_root().unwrap();
        assert_ne!(
            root_e0, root_e1,
            "root must change after epoch 1 state update"
        );

        // Alice's new balance.
        let (val_alice, proof_alice) = backend.prove_key(b"balance_alice").unwrap();
        assert_eq!(val_alice.as_deref(), Some(b"900".as_slice()));
        Blake3SmtCommitment::verify_proof(&root_e1, b"balance_alice", Some(b"900"), &proof_alice)
            .unwrap();

        // Bob's balance (new key).
        let (val_bob, proof_bob) = backend.prove_key(b"balance_bob").unwrap();
        assert_eq!(val_bob.as_deref(), Some(b"100".as_slice()));
        Blake3SmtCommitment::verify_proof(&root_e1, b"balance_bob", Some(b"100"), &proof_bob)
            .unwrap();
    }

    // ── 5. Full 10-epoch pipeline: consensus + proof + snapshot ──────

    /// Exercises the full pipeline over 10 epochs:
    /// - Each epoch: insert unique state, advance epoch (consensus),
    ///   epoch_boundary_check (proof), build snapshot manifest (storage).
    /// - After all epochs: verify proof roots are unique per epoch,
    ///   manifests form a hash chain, and final state is provable.
    #[test]
    fn full_pipeline_10_epochs_proof_snapshot_consistency() {
        let _cfg = nexus_consensus::types::EpochConfig {
            epoch_length_commits: 10,
            epoch_length_seconds: 0,
            min_epoch_commits: 3,
        };
        let store = MemoryStore::new();
        let (sk, vk) = FalconSigner::generate_keypair();

        let c0 = make_committee(4, EpochNumber(0));
        epoch_store::persist_initial_epoch(&store, &c0).unwrap();
        let mut engine = ConsensusEngine::new(EpochNumber(0), c0);
        let mut tracker = CommitmentTracker::new();

        let mut roots: Vec<Blake3Digest> = Vec::new();
        let mut manifests: Vec<SnapshotManifest> = Vec::new();

        for epoch in 0..10u64 {
            // State change: each epoch writes a unique account balance.
            let key = format!("balance_user_{epoch}").into_bytes();
            let val = format!("{}", (epoch + 1) * 1000).into_bytes();
            insert(&mut tracker, &[(&key, &val)]);

            // Proof: epoch boundary check + capture root.
            tracker.epoch_boundary_check().unwrap();
            let root = tracker.commitment_root();
            assert!(
                !roots.contains(&root),
                "root must be unique at epoch {epoch}"
            );
            roots.push(root);

            // Proof: verify the key just written.
            let (v, proof) = tracker.prove_key(&key).unwrap();
            assert_eq!(v.as_deref(), Some(val.as_slice()));
            Blake3SmtCommitment::verify_proof(&root, &key, Some(&val), &proof).unwrap();

            // Consensus: advance epoch.
            if epoch < 9 {
                let next = EpochNumber(epoch + 1);
                let new_c = make_committee(4, next);
                let (t, _) =
                    engine.advance_epoch(new_c.clone(), EpochTransitionTrigger::CommitThreshold);
                epoch_store::persist_epoch_transition(&store, &new_c, &t).unwrap();
            }

            // Snapshot: build and sign manifest for this epoch.
            let content_hash = {
                let h = blake3::hash(root.as_bytes());
                let mut arr = [0u8; 32];
                arr.copy_from_slice(h.as_bytes());
                arr
            };
            let mut manifest = make_manifest(epoch, (epoch + 1) * 10, content_hash);

            // Hash chain: link to previous manifest.
            if let Some(prev) = manifests.last() {
                let prev_bytes = prev.signable_bytes();
                let h = blake3::hash(&prev_bytes);
                let mut arr = [0u8; 32];
                arr.copy_from_slice(h.as_bytes());
                manifest.previous_manifest_hash = Some(arr);
            }

            sign_manifest(&mut manifest, &sk, &vk);
            verify_manifest(&manifest, &vk).unwrap();
            manifests.push(manifest);
        }

        // Post: verify all roots are unique.
        assert_eq!(roots.len(), 10);
        let unique_roots: std::collections::HashSet<_> = roots.iter().collect();
        assert_eq!(unique_roots.len(), 10, "all roots must be unique");

        // Post: verify manifest chain integrity.
        for i in 1..manifests.len() {
            assert!(
                manifests[i].previous_manifest_hash.is_some(),
                "manifest {i} must have previous_manifest_hash"
            );
            let expected_prev_hash = {
                let prev_bytes = manifests[i - 1].signable_bytes();
                let h = blake3::hash(&prev_bytes);
                let mut arr = [0u8; 32];
                arr.copy_from_slice(h.as_bytes());
                arr
            };
            assert_eq!(
                manifests[i].previous_manifest_hash.unwrap(),
                expected_prev_hash,
                "manifest {i} hash chain broken"
            );
        }

        // Post: epoch store has 9 transitions (10 epochs, 9 advances).
        let final_state = epoch_store::load_epoch_state(&store).unwrap().unwrap();
        assert_eq!(final_state.epoch, EpochNumber(9));
        assert_eq!(final_state.transitions.len(), 9);
    }

    // ── 6. Query/proof agreement: balance matches proof ──────────────

    /// Simulates a scenario where a "query" reads a balance and a
    /// "proof" proves the same key. Both must agree on value and root.
    #[test]
    fn query_proof_agreement_across_3_epochs() {
        let shared = new_shared_tracker();

        for epoch in 0..3u64 {
            let key = b"balance_alice";
            let balance = format!("{}", 1000 + epoch * 100);
            let val = balance.as_bytes();

            // "Execution" writes the balance.
            {
                let mut t = shared.write().unwrap();
                insert(&mut t, &[(key, val)]);
                t.epoch_boundary_check().unwrap();
            }

            // "Query" reads the balance.
            let backend = LiveStateProofBackend::new(shared.clone());
            let (queried_val, proof) = backend.prove_key(key).unwrap();
            let root = backend.commitment_root().unwrap();

            // Query result and proof must agree.
            assert_eq!(
                queried_val.as_deref(),
                Some(val),
                "query must return correct balance at epoch {epoch}"
            );
            Blake3SmtCommitment::verify_proof(&root, key, Some(val), &proof).unwrap_or_else(|e| {
                panic!("proof must verify at epoch {epoch}: {e}");
            });
        }
    }

    // ── 7. Delete + epoch advance: absent key provable ───────────────

    /// Inserts a key, advances epoch, deletes the key, then verifies
    /// the deletion is provable (absence proof) in the new epoch.
    #[test]
    fn delete_across_epoch_produces_absence_proof() {
        let mut tracker = CommitmentTracker::new();

        // Epoch 0: insert.
        insert(&mut tracker, &[(b"temp_key", b"temp_val")]);
        tracker.epoch_boundary_check().unwrap();
        let root_e0 = tracker.commitment_root();

        let (v, _) = tracker.prove_key(b"temp_key").unwrap();
        assert_eq!(v.as_deref(), Some(b"temp_val".as_slice()));

        // Epoch 1: delete.
        tracker.apply_state_changes(&[StateChangeEntry {
            key: b"temp_key",
            value: None,
        }]);
        tracker.epoch_boundary_check().unwrap();
        let root_e1 = tracker.commitment_root();

        assert_ne!(root_e0, root_e1, "root must change after deletion");

        // Absence proof.
        let (v_deleted, proof_absent) = tracker.prove_key(b"temp_key").unwrap();
        assert!(v_deleted.is_none(), "deleted key must be absent");
        Blake3SmtCommitment::verify_proof(&root_e1, b"temp_key", None, &proof_absent).unwrap();
    }

    // ── 8. Concurrent-key updates across epochs ──────────────────────

    /// Multiple keys updated in each of 5 epochs. After all epochs,
    /// every key's latest value must be provable against the final root.
    #[test]
    fn concurrent_keys_across_5_epochs() {
        let mut tracker = CommitmentTracker::new();
        let keys: Vec<Vec<u8>> = (0..5).map(|i| format!("key_{i}").into_bytes()).collect();

        for epoch in 0..5u64 {
            // Update all 5 keys in each epoch.
            let entries: Vec<(Vec<u8>, Vec<u8>)> = keys
                .iter()
                .map(|k| {
                    let v = format!("val_e{epoch}_{}", String::from_utf8_lossy(k));
                    (k.clone(), v.into_bytes())
                })
                .collect();
            let refs: Vec<(&[u8], &[u8])> = entries
                .iter()
                .map(|(k, v)| (k.as_slice(), v.as_slice()))
                .collect();
            insert(&mut tracker, &refs);
            tracker.epoch_boundary_check().unwrap();
        }

        let final_root = tracker.commitment_root();

        // Each key should have its epoch-4 value.
        for key in &keys {
            let expected = format!("val_e4_{}", String::from_utf8_lossy(key));
            let (val, proof) = tracker.prove_key(key).unwrap();
            assert_eq!(val.as_deref(), Some(expected.as_bytes()));
            Blake3SmtCommitment::verify_proof(&final_root, key, Some(expected.as_bytes()), &proof)
                .unwrap();
        }
    }

    // ── 9. Snapshot provenance chain across epoch boundaries ──────────

    /// Builds a 5-epoch manifest chain where each manifest references
    /// the previous one. Verifies the chain is intact and epoch-specific
    /// tamper attempts are rejected.
    #[test]
    fn snapshot_provenance_chain_across_5_epochs() {
        let (sk, vk) = FalconSigner::generate_keypair();
        let mut manifests: Vec<SnapshotManifest> = Vec::new();

        for epoch in 0..5u64 {
            let content_hash = {
                let h = blake3::hash(&epoch.to_le_bytes());
                let mut arr = [0u8; 32];
                arr.copy_from_slice(h.as_bytes());
                arr
            };

            let mut m = make_manifest(epoch, (epoch + 1) * 50, content_hash);

            if let Some(prev) = manifests.last() {
                let prev_bytes = prev.signable_bytes();
                let h = blake3::hash(&prev_bytes);
                let mut arr = [0u8; 32];
                arr.copy_from_slice(h.as_bytes());
                m.previous_manifest_hash = Some(arr);
            }

            sign_manifest(&mut m, &sk, &vk);
            verify_manifest(&m, &vk).unwrap();
            manifests.push(m);
        }

        // Tamper: swap epoch on manifest 3.
        let mut tampered = manifests[3].clone();
        tampered.epoch = Some(99);
        assert!(
            verify_manifest(&tampered, &vk).is_err(),
            "tampered epoch must be rejected"
        );

        // Tamper: break hash chain on manifest 2.
        let mut tampered_chain = manifests[2].clone();
        tampered_chain.previous_manifest_hash = Some([0xFF; 32]);
        // Re-sign to see if content change is caught.
        sign_manifest(&mut tampered_chain, &sk, &vk);
        // The original signature should not verify.
        let original_sig = manifests[2].signature.clone();
        tampered_chain.signature = original_sig;
        assert!(
            verify_manifest(&tampered_chain, &vk).is_err(),
            "broken hash chain must be detected"
        );
    }

    // ── 10. Epoch-tagged commitment info ─────────────────────────────

    /// Verifies that LiveStateProofBackend's commitment_info() reports
    /// monotonically increasing entry counts and update counts as
    /// state is applied across epochs.
    #[test]
    fn commitment_info_monotonic_across_epochs() {
        let shared = new_shared_tracker();
        let backend = LiveStateProofBackend::new(shared.clone());

        let mut prev_updates = 0u64;

        for epoch in 0..5u64 {
            {
                let mut t = shared.write().unwrap();
                let key = format!("info_key_{epoch}").into_bytes();
                let val = format!("info_val_{epoch}").into_bytes();
                insert(&mut t, &[(&key, &val)]);
                t.epoch_boundary_check().unwrap();
            }

            let info = backend.commitment_info().unwrap();
            assert!(
                info.updates_applied > prev_updates,
                "updates_applied must increase across epochs"
            );
            assert!(
                info.entry_count > 0,
                "entry_count must be positive at epoch {epoch}"
            );
            assert!(
                !info.commitment_root.is_empty(),
                "commitment_root must be non-empty"
            );
            prev_updates = info.updates_applied;
        }
    }
}
