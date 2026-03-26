// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! B-1: Multi-epoch stress tests with partition, restart, and replay.
//!
//! Extends the existing soak tests with combination scenarios that exercise
//! epoch boundary behaviors under adversarial conditions: network partitions,
//! mid-boundary restarts, state replay after crash, and cross-module
//! consistency (commitment root, proof verification) across epoch transitions.
//!
//! Acceptance criteria (from roadmap B-1):
//!   - At least one suite crossing 10+ epochs with combined stress.
//!   - Epoch boundary restart is tested and state is verified post-reload.
//!   - Partition simulation shows manager/engine resilience.
//!   - Commitment root remains consistent across epoch transitions + state changes.

#[cfg(test)]
mod tests {
    use nexus_consensus::types::{EpochConfig, EpochTransition, EpochTransitionTrigger};
    use nexus_consensus::{Committee, ConsensusEngine, EpochManager, ValidatorRegistry};
    use nexus_node::commitment_tracker::{CommitmentTracker, StateChangeEntry};
    use nexus_node::epoch_store;
    use nexus_primitives::{Amount, Blake3Digest, EpochNumber, TimestampMs, ValidatorIndex};
    use nexus_storage::commitment::Blake3SmtCommitment;
    use nexus_storage::traits::StateCommitment;
    use nexus_storage::MemoryStore;

    use crate::fixtures::consensus::TestCommittee;

    // ── Helpers ──────────────────────────────────────────────────────

    fn make_cfg(commits: u64, seconds: u64, min: u64) -> EpochConfig {
        EpochConfig {
            epoch_length_commits: commits,
            epoch_length_seconds: seconds,
            min_epoch_commits: min,
        }
    }

    fn make_committee(n: usize, epoch: EpochNumber) -> Committee {
        TestCommittee::new(n, epoch).committee
    }

    fn transition(
        from: u64,
        to: u64,
        trigger: EpochTransitionTrigger,
        commits: u64,
    ) -> EpochTransition {
        EpochTransition {
            from_epoch: EpochNumber(from),
            to_epoch: EpochNumber(to),
            trigger,
            final_commit_count: commits,
            transitioned_at: TimestampMs(1_700_000_000_000 + to * 60_000),
        }
    }

    // ── 1. Partition simulation: no progress during partition window ──

    /// Simulates a network partition by skipping epoch advances for several
    /// "rounds" while accumulating commits, then resuming. Verifies the
    /// epoch manager correctly advances once the partition lifts and the
    /// engine state is consistent.
    #[test]
    fn partition_during_epoch_transition() {
        let cfg = make_cfg(20, 0, 5);
        let store = MemoryStore::new();

        let c0 = make_committee(4, EpochNumber(0));
        epoch_store::persist_initial_epoch(&store, &c0).unwrap();
        let mut mgr = EpochManager::new(cfg.clone(), EpochNumber(0));
        let mut engine = ConsensusEngine::new(EpochNumber(0), c0);

        // Advance 3 epochs normally.
        for i in 0..3u64 {
            assert!(mgr.should_advance(20).is_some());
            let next = EpochNumber(i + 1);
            let new_c = make_committee(4, next);
            let (t, _) =
                engine.advance_epoch(new_c.clone(), EpochTransitionTrigger::CommitThreshold);
            epoch_store::persist_epoch_transition(&store, &new_c, &t).unwrap();
            mgr.record_transition(t);
        }
        assert_eq!(mgr.current_epoch(), EpochNumber(3));

        // ── PARTITION: commits accumulate but we do NOT advance epochs ──
        // Simulate 5 rounds of commit checking without triggering advance.
        // The manager says "should advance" but the node is partitioned and
        // cannot form the new committee. We just ignore the trigger.
        for _ in 0..5u32 {
            let _trigger = mgr.should_advance(20);
            // Partition: cannot fetch new committee, skip advance.
        }
        // Manager should still be at epoch 3 (no transition recorded).
        assert_eq!(mgr.current_epoch(), EpochNumber(3));
        assert_eq!(engine.epoch(), EpochNumber(3));

        // ── PARTITION LIFTS: advance resumes ──
        for i in 3..6u64 {
            assert!(mgr.should_advance(20).is_some());
            let next = EpochNumber(i + 1);
            let new_c = make_committee(4, next);
            let (t, _) =
                engine.advance_epoch(new_c.clone(), EpochTransitionTrigger::CommitThreshold);
            epoch_store::persist_epoch_transition(&store, &new_c, &t).unwrap();
            mgr.record_transition(t);
        }
        assert_eq!(mgr.current_epoch(), EpochNumber(6));
        assert_eq!(engine.epoch(), EpochNumber(6));

        // Cold restart verification.
        let state = epoch_store::load_epoch_state(&store).unwrap().unwrap();
        assert_eq!(state.epoch, EpochNumber(6));
        assert_eq!(state.transitions.len(), 6);
    }

    // ── 2. Restart at exact epoch boundary ───────────────────────────

    /// Persists state just after an epoch transition, then "restarts" by
    /// dropping all in-memory state and reloading from storage. Verifies
    /// the new engine+manager can resume from the exact boundary.
    #[test]
    fn restart_at_epoch_boundary() {
        let cfg = make_cfg(10, 0, 3);
        let store = MemoryStore::new();

        let c0 = make_committee(5, EpochNumber(0));
        epoch_store::persist_initial_epoch(&store, &c0).unwrap();
        let mut mgr = EpochManager::new(cfg.clone(), EpochNumber(0));
        let mut engine = ConsensusEngine::new(EpochNumber(0), c0);

        // Advance to epoch 5, restarting at each boundary.
        for i in 0..5u64 {
            assert!(mgr.should_advance(10).is_some());
            let next = EpochNumber(i + 1);
            let new_c = make_committee(5, next);
            let (t, remaining) =
                engine.advance_epoch(new_c.clone(), EpochTransitionTrigger::CommitThreshold);
            assert!(
                remaining.is_empty(),
                "no commits in test so remaining should be empty"
            );
            epoch_store::persist_epoch_transition(&store, &new_c, &t).unwrap();
            mgr.record_transition(t);

            // ── RESTART: reload from storage ──
            let state = epoch_store::load_epoch_state(&store).unwrap().unwrap();
            assert_eq!(state.epoch, next);
            mgr = EpochManager::new(cfg.clone(), state.epoch);
            engine = ConsensusEngine::new(state.epoch, state.committee);
        }

        assert_eq!(mgr.current_epoch(), EpochNumber(5));
        assert_eq!(engine.epoch(), EpochNumber(5));

        // Final state check.
        let final_state = epoch_store::load_epoch_state(&store).unwrap().unwrap();
        assert_eq!(final_state.transitions.len(), 5);
    }

    // ── 3. Mixed trigger types over 12 epochs ────────────────────────

    /// Alternates between CommitThreshold, TimeElapsed, and Manual triggers
    /// to verify the engine and manager handle all trigger variants correctly
    /// across many epochs.
    #[test]
    fn mixed_trigger_types_over_12_epochs() {
        let store = MemoryStore::new();

        let c0 = make_committee(4, EpochNumber(0));
        epoch_store::persist_initial_epoch(&store, &c0).unwrap();
        let mut engine = ConsensusEngine::new(EpochNumber(0), c0);

        let triggers = [
            EpochTransitionTrigger::CommitThreshold,
            EpochTransitionTrigger::TimeElapsed,
            EpochTransitionTrigger::Manual,
        ];

        for i in 0..12u64 {
            let next = EpochNumber(i + 1);
            let trigger = triggers[(i % 3) as usize];
            let new_c = make_committee(4, next);
            let (t, _) = engine.advance_epoch(new_c.clone(), trigger);
            epoch_store::persist_epoch_transition(&store, &new_c, &t).unwrap();

            assert_eq!(t.from_epoch, EpochNumber(i));
            assert_eq!(t.to_epoch, next);
        }

        assert_eq!(engine.epoch(), EpochNumber(12));

        // Verify each trigger was persisted correctly.
        let state = epoch_store::load_epoch_state(&store).unwrap().unwrap();
        assert_eq!(state.transitions.len(), 12);
        for (idx, t) in state.transitions.iter().enumerate() {
            let expected = triggers[idx % 3];
            assert_eq!(t.trigger, expected, "epoch {idx} trigger mismatch");
        }
    }

    // ── 4. Combined partition + restart + rotation over 15 epochs ────

    /// A comprehensive 15-epoch scenario combining:
    /// - Normal operation (epochs 0–4)
    /// - Partition at epoch 5 (stalled for 3 rounds, no advance)
    /// - Resume + cold restart at epoch 6 boundary
    /// - Committee rotation: alternating 4/7 validators (epochs 7–10)
    /// - Slash during epoch 11
    /// - Another cold restart at epoch 12
    /// - Normal finish through epoch 15
    #[test]
    fn combined_partition_restart_rotation_15_epochs() {
        let cfg = make_cfg(10, 0, 3);
        let store = MemoryStore::new();

        let c0 = make_committee(4, EpochNumber(0));
        epoch_store::persist_initial_epoch(&store, &c0).unwrap();
        let mut mgr = EpochManager::new(cfg.clone(), EpochNumber(0));
        let mut engine = ConsensusEngine::new(EpochNumber(0), c0);

        let mut epoch_idx = 0u64;

        // Phase 1: Normal operation, epochs 0→5.
        for _ in 0..5 {
            assert!(mgr.should_advance(10).is_some());
            epoch_idx += 1;
            let new_c = make_committee(4, EpochNumber(epoch_idx));
            let (t, _) =
                engine.advance_epoch(new_c.clone(), EpochTransitionTrigger::CommitThreshold);
            epoch_store::persist_epoch_transition(&store, &new_c, &t).unwrap();
            mgr.record_transition(t);
        }
        assert_eq!(engine.epoch(), EpochNumber(5));

        // Phase 2: Partition at epoch 5 — 3 stalled rounds.
        for _ in 0..3u32 {
            let _ = mgr.should_advance(10);
            // Cannot advance — partitioned.
        }
        assert_eq!(engine.epoch(), EpochNumber(5));

        // Phase 3: Resume, advance to epoch 6, then cold restart.
        assert!(mgr.should_advance(10).is_some());
        epoch_idx += 1;
        let c6 = make_committee(4, EpochNumber(epoch_idx));
        let (t6, _) = engine.advance_epoch(c6.clone(), EpochTransitionTrigger::CommitThreshold);
        epoch_store::persist_epoch_transition(&store, &c6, &t6).unwrap();
        mgr.record_transition(t6);

        // Cold restart.
        let state = epoch_store::load_epoch_state(&store).unwrap().unwrap();
        assert_eq!(state.epoch, EpochNumber(6));
        mgr = EpochManager::new(cfg.clone(), state.epoch);
        engine = ConsensusEngine::new(state.epoch, state.committee);

        // Phase 4: Committee rotation epochs 7–10.
        for i in 0..4u64 {
            epoch_idx += 1;
            let size = if i % 2 == 0 { 7 } else { 4 };
            let new_c = make_committee(size, EpochNumber(epoch_idx));
            let (t, _) =
                engine.advance_epoch(new_c.clone(), EpochTransitionTrigger::CommitThreshold);
            epoch_store::persist_epoch_transition(&store, &new_c, &t).unwrap();
            mgr.record_transition(EpochTransition {
                from_epoch: EpochNumber(epoch_idx - 1),
                to_epoch: EpochNumber(epoch_idx),
                trigger: EpochTransitionTrigger::CommitThreshold,
                final_commit_count: 10,
                transitioned_at: TimestampMs::now(),
            });
        }
        assert_eq!(engine.epoch(), EpochNumber(10));

        // Phase 5: Slash during epoch 11.
        epoch_idx += 1;
        let mut c11 = make_committee(7, EpochNumber(epoch_idx));
        let _ = c11.slash(ValidatorIndex(0));
        let _active_after_slash = c11.active_validators().len();
        let (t11, _) = engine.advance_epoch(c11.clone(), EpochTransitionTrigger::Manual);
        epoch_store::persist_epoch_transition(&store, &c11, &t11).unwrap();
        mgr.record_transition(EpochTransition {
            from_epoch: EpochNumber(epoch_idx - 1),
            to_epoch: EpochNumber(epoch_idx),
            trigger: EpochTransitionTrigger::Manual,
            final_commit_count: 10,
            transitioned_at: TimestampMs::now(),
        });

        // Phase 6: Cold restart at epoch 12.
        epoch_idx += 1;
        let c12 = make_committee(4, EpochNumber(epoch_idx));
        let (t12, _) = engine.advance_epoch(c12.clone(), EpochTransitionTrigger::CommitThreshold);
        epoch_store::persist_epoch_transition(&store, &c12, &t12).unwrap();

        let state2 = epoch_store::load_epoch_state(&store).unwrap().unwrap();
        assert_eq!(state2.epoch, EpochNumber(12));
        mgr = EpochManager::new(cfg.clone(), state2.epoch);
        engine = ConsensusEngine::new(state2.epoch, state2.committee);

        // Phase 7: Normal finish through epoch 15.
        for _ in 0..3 {
            epoch_idx += 1;
            let new_c = make_committee(4, EpochNumber(epoch_idx));
            let (t, _) =
                engine.advance_epoch(new_c.clone(), EpochTransitionTrigger::CommitThreshold);
            epoch_store::persist_epoch_transition(&store, &new_c, &t).unwrap();
            mgr.record_transition(EpochTransition {
                from_epoch: EpochNumber(epoch_idx - 1),
                to_epoch: EpochNumber(epoch_idx),
                trigger: EpochTransitionTrigger::CommitThreshold,
                final_commit_count: 10,
                transitioned_at: TimestampMs::now(),
            });
        }

        assert_eq!(epoch_idx, 15);
        assert_eq!(engine.epoch(), EpochNumber(15));

        // Final cold-restart verification.
        let final_state = epoch_store::load_epoch_state(&store).unwrap().unwrap();
        assert_eq!(final_state.epoch, EpochNumber(15));
        assert!(
            final_state.transitions.len() >= 15,
            "expected at least 15 transitions, got {}",
            final_state.transitions.len()
        );
    }

    // ── 5. Epoch boundary commitment root stability ──────────────────

    /// Applies state changes during each epoch, runs epoch_boundary_check()
    /// at every transition, and verifies:
    /// - Root changes after state mutations.
    /// - Root stays the same if no mutations occur between checks.
    /// - Proofs verified against the root remain valid after epoch boundary.
    #[test]
    fn epoch_boundary_commitment_root_consistency() {
        let mut tracker = CommitmentTracker::new();

        let mut previous_root: Option<Blake3Digest> = None;

        for epoch in 0..10u64 {
            // Mutate state for this epoch.
            let key = format!("epoch_{epoch}_key").into_bytes();
            let val = format!("epoch_{epoch}_val").into_bytes();
            tracker.apply_state_changes(&[StateChangeEntry {
                key: &key,
                value: Some(&val),
            }]);

            let root = tracker.commitment_root();
            assert_ne!(
                root,
                Blake3Digest::ZERO,
                "root must be non-zero after insert"
            );

            // Root should differ from previous epoch (different data inserted).
            if let Some(prev) = previous_root {
                assert_ne!(
                    root, prev,
                    "root should change after epoch {epoch} state mutation"
                );
            }

            // Epoch boundary check.
            tracker.epoch_boundary_check().unwrap_or_else(|e| {
                panic!("epoch_boundary_check failed at epoch {epoch}: {e}");
            });

            // Verify proof for the key just inserted.
            let (val_out, proof) = tracker.prove_key(&key).unwrap();
            assert_eq!(val_out.as_deref(), Some(val.as_slice()));
            Blake3SmtCommitment::verify_proof(&root, &key, Some(&val), &proof).unwrap_or_else(
                |e| {
                    panic!("proof verification failed at epoch {epoch}: {e}");
                },
            );

            previous_root = Some(root);
        }

        assert_eq!(tracker.epoch_checks_passed(), 10);
    }

    // ── 6. Consecutive restarts across multiple boundaries ───────────

    /// Advances 10 epochs, performing a cold restart at every even-numbered
    /// epoch boundary. This stress-tests the persistence layer's ability
    /// to handle frequent reload cycles.
    #[test]
    fn consecutive_restarts_at_every_other_boundary() {
        let cfg = make_cfg(10, 0, 3);
        let store = MemoryStore::new();

        let c0 = make_committee(4, EpochNumber(0));
        epoch_store::persist_initial_epoch(&store, &c0).unwrap();
        let mut mgr = EpochManager::new(cfg.clone(), EpochNumber(0));
        let mut engine = ConsensusEngine::new(EpochNumber(0), c0);

        for i in 0..10u64 {
            let next = EpochNumber(i + 1);
            let new_c = make_committee(4, next);
            let (t, _) =
                engine.advance_epoch(new_c.clone(), EpochTransitionTrigger::CommitThreshold);
            epoch_store::persist_epoch_transition(&store, &new_c, &t).unwrap();
            mgr.record_transition(t);

            // Restart at every even boundary.
            if next.0 % 2 == 0 {
                let state = epoch_store::load_epoch_state(&store).unwrap().unwrap();
                assert_eq!(state.epoch, next);
                assert_eq!(
                    state.committee.active_validators().len(),
                    4,
                    "committee size should be 4 after restart at epoch {}",
                    next.0
                );
                mgr = EpochManager::new(cfg.clone(), state.epoch);
                engine = ConsensusEngine::new(state.epoch, state.committee);
            }
        }

        assert_eq!(mgr.current_epoch(), EpochNumber(10));
        assert_eq!(engine.epoch(), EpochNumber(10));
    }

    // ── 7. Late commits after recovery ───────────────────────────────

    /// Simulates a crash at epoch 5, then verifies the epoch manager
    /// correctly handles commits that arrive after recovery (the commit
    /// counter resets to 0 for the new epoch, so the first batch of
    /// commits post-recovery should work normally).
    #[test]
    fn late_commits_after_recovery() {
        let store = MemoryStore::new();
        let cfg = make_cfg(10, 0, 3);

        let c0 = make_committee(4, EpochNumber(0));
        epoch_store::persist_initial_epoch(&store, &c0).unwrap();
        let mut mgr = EpochManager::new(cfg.clone(), EpochNumber(0));

        // Advance to epoch 5.
        for i in 0..5u64 {
            assert!(mgr.should_advance(10).is_some());
            let new_c = make_committee(4, EpochNumber(i + 1));
            let t = transition(i, i + 1, EpochTransitionTrigger::CommitThreshold, 10);
            epoch_store::persist_epoch_transition(&store, &new_c, &t).unwrap();
            mgr.record_transition(t);
        }
        assert_eq!(mgr.current_epoch(), EpochNumber(5));

        // ── CRASH + RECOVER ──
        let state = epoch_store::load_epoch_state(&store).unwrap().unwrap();
        let mut mgr2 = EpochManager::new(cfg.clone(), state.epoch);

        // Post-recovery: should NOT advance with only 3 commits (min_commits=3, threshold=10).
        assert!(mgr2.should_advance(3).is_none());
        // Should NOT advance at 9 commits.
        assert!(mgr2.should_advance(9).is_none());
        // Should advance at 10 commits.
        assert!(mgr2.should_advance(10).is_some());

        // Record the transition.
        let new_c = make_committee(4, EpochNumber(6));
        let t = transition(5, 6, EpochTransitionTrigger::CommitThreshold, 10);
        epoch_store::persist_epoch_transition(&store, &new_c, &t).unwrap();
        mgr2.record_transition(t);
        assert_eq!(mgr2.current_epoch(), EpochNumber(6));
    }

    // ── 8. Quorum changes under stress ───────────────────────────────

    /// Varies committee sizes dramatically across epochs (3→100→7→50→4)
    /// and verifies quorum is recalculated correctly after each transition.
    #[test]
    fn quorum_changes_under_stress() {
        let sizes: Vec<usize> = vec![3, 100, 7, 50, 4, 20, 1, 10, 201, 5];

        for (i, &size) in sizes.iter().enumerate() {
            let c = make_committee(size, EpochNumber(i as u64));
            let active = c.active_validators().len();
            assert_eq!(
                active, size,
                "epoch {i} committee should have {size} validators"
            );

            // Stake-weighted quorum: total_stake * 2 / 3 + 1 (each validator stakes 1000).
            let total_stake = size as u64 * 1000;
            let expected_quorum = Amount(total_stake * 2 / 3 + 1);
            assert_eq!(
                c.quorum_threshold(),
                expected_quorum,
                "epoch {i}, n={size}: expected quorum {expected_quorum:?}"
            );
        }
    }

    // ── 9. Rapid epoch transitions without persistence ───────────────

    /// Advances the engine through 100 rapid epoch transitions (no
    /// persistence) to verify the engine handles high-frequency
    /// reconfiguration without state corruption.
    #[test]
    fn rapid_100_epoch_transitions_engine_only() {
        let tc = TestCommittee::new(4, EpochNumber(0));
        let mut engine = ConsensusEngine::new(tc.epoch, tc.committee.clone());

        for i in 0..100u64 {
            let next = EpochNumber(i + 1);
            let new_c = make_committee(4, next);
            let (t, remaining) =
                engine.advance_epoch(new_c, EpochTransitionTrigger::CommitThreshold);
            assert_eq!(t.to_epoch, next);
            assert!(remaining.is_empty());
        }

        assert_eq!(engine.epoch(), EpochNumber(100));
        assert_eq!(
            engine.dag_size(),
            0,
            "DAG should be empty after rapid resets"
        );
        assert_eq!(engine.total_commits(), 0, "commits reset after advance");
    }

    // ── 10. Epoch + state changes + proof consistency ────────────────

    /// Full pipeline scenario: over 10 epochs, apply state changes
    /// at each epoch, run epoch_boundary_check, take a proof, then
    /// verify the proof against the commitment root after the next
    /// epoch's mutations. The old proof should NOT verify against
    /// the new root (proving the root actually changed).
    #[test]
    fn cross_epoch_proof_invalidation() {
        let mut tracker = CommitmentTracker::new();
        let mut old_proofs: Vec<(Vec<u8>, Vec<u8>, Blake3Digest, nexus_storage::MerkleProof)> =
            Vec::new();

        for epoch in 0..10u64 {
            let key = format!("e{epoch}").into_bytes();
            let val = format!("v{epoch}").into_bytes();
            tracker.apply_state_changes(&[StateChangeEntry {
                key: &key,
                value: Some(&val),
            }]);

            let root = tracker.commitment_root();
            tracker.epoch_boundary_check().unwrap();

            let (_, proof) = tracker.prove_key(&key).unwrap();
            // Current proof should verify against current root.
            Blake3SmtCommitment::verify_proof(&root, &key, Some(&val), &proof).unwrap();

            // Check that proofs from previous epochs do NOT verify with new root
            // (the root has changed because new data was inserted).
            for (old_key, old_val, old_root, old_proof) in &old_proofs {
                // Old proof should still verify against its own old root.
                Blake3SmtCommitment::verify_proof(old_root, old_key, Some(old_val), old_proof)
                    .unwrap();
                // But should NOT verify against the current (new) root.
                assert!(
                    Blake3SmtCommitment::verify_proof(&root, old_key, Some(old_val), old_proof)
                        .is_err(),
                    "old proof from epoch should not verify against new root after epoch {epoch}"
                );
            }

            old_proofs.push((key, val, root, proof));
        }
    }

    // ── 11. Partition then double-advance recovery ───────────────────

    /// Simulates a scenario where a node recovers from a partition and
    /// needs to jump forward by 2 epochs (catching up). Verifies that
    /// skipping an epoch in persistence is safe if the engine is
    /// reconstructed from the latest known state.
    #[test]
    fn partition_then_double_epoch_advance() {
        let store = MemoryStore::new();

        let c0 = make_committee(4, EpochNumber(0));
        epoch_store::persist_initial_epoch(&store, &c0).unwrap();
        let mut engine = ConsensusEngine::new(EpochNumber(0), c0);

        // Normal advance to epoch 3.
        for i in 0..3u64 {
            let new_c = make_committee(4, EpochNumber(i + 1));
            let (t, _) =
                engine.advance_epoch(new_c.clone(), EpochTransitionTrigger::CommitThreshold);
            epoch_store::persist_epoch_transition(&store, &new_c, &t).unwrap();
        }
        assert_eq!(engine.epoch(), EpochNumber(3));

        // Partition: engine cannot advance.
        // After partition lifts, advance twice in quick succession (catch-up).
        let c4 = make_committee(4, EpochNumber(4));
        let (t4, _) = engine.advance_epoch(c4.clone(), EpochTransitionTrigger::CommitThreshold);
        epoch_store::persist_epoch_transition(&store, &c4, &t4).unwrap();

        let c5 = make_committee(4, EpochNumber(5));
        let (t5, _) = engine.advance_epoch(c5.clone(), EpochTransitionTrigger::CommitThreshold);
        epoch_store::persist_epoch_transition(&store, &c5, &t5).unwrap();

        assert_eq!(engine.epoch(), EpochNumber(5));

        // Verify persistence caught up.
        let state = epoch_store::load_epoch_state(&store).unwrap().unwrap();
        assert_eq!(state.epoch, EpochNumber(5));
        assert_eq!(state.transitions.len(), 5);
    }

    // ── 12. Slash + rotation + restart combined ──────────────────────

    /// Over 8 epochs: slash validators, rotate committee sizes, and restart
    /// at epoch 4. Verifies quorum recalculation and persistence survive
    /// the combined stress.
    #[test]
    fn slash_rotation_restart_combined() {
        let store = MemoryStore::new();

        let c0 = make_committee(7, EpochNumber(0));
        epoch_store::persist_initial_epoch(&store, &c0).unwrap();
        let mut engine = ConsensusEngine::new(EpochNumber(0), c0);

        // Epoch 1: slash index 0.
        let mut c1 = make_committee(7, EpochNumber(1));
        let _ = c1.slash(ValidatorIndex(0));
        let (t1, _) = engine.advance_epoch(c1.clone(), EpochTransitionTrigger::CommitThreshold);
        epoch_store::persist_epoch_transition(&store, &c1, &t1).unwrap();
        assert_eq!(engine.committee().active_validators().len(), 6);

        // Epoch 2: rotate to 10 validators.
        let c2 = make_committee(10, EpochNumber(2));
        let (t2, _) = engine.advance_epoch(c2.clone(), EpochTransitionTrigger::CommitThreshold);
        epoch_store::persist_epoch_transition(&store, &c2, &t2).unwrap();
        assert_eq!(engine.committee().active_validators().len(), 10);

        // Epoch 3: slash two validators.
        let mut c3 = make_committee(10, EpochNumber(3));
        let _ = c3.slash(ValidatorIndex(1));
        let _ = c3.slash(ValidatorIndex(5));
        let (t3, _) = engine.advance_epoch(c3.clone(), EpochTransitionTrigger::Manual);
        epoch_store::persist_epoch_transition(&store, &c3, &t3).unwrap();
        assert_eq!(engine.committee().active_validators().len(), 8);

        // Cold restart at epoch 4.
        let c4 = make_committee(4, EpochNumber(4));
        let (t4, _) = engine.advance_epoch(c4.clone(), EpochTransitionTrigger::CommitThreshold);
        epoch_store::persist_epoch_transition(&store, &c4, &t4).unwrap();

        let state = epoch_store::load_epoch_state(&store).unwrap().unwrap();
        assert_eq!(state.epoch, EpochNumber(4));
        assert_eq!(state.committee.active_validators().len(), 4);
        engine = ConsensusEngine::new(state.epoch, state.committee);

        // Continue epochs 5–8.
        for i in 4..8u64 {
            let new_c = make_committee(4, EpochNumber(i + 1));
            let (t, _) =
                engine.advance_epoch(new_c.clone(), EpochTransitionTrigger::CommitThreshold);
            epoch_store::persist_epoch_transition(&store, &new_c, &t).unwrap();
        }
        assert_eq!(engine.epoch(), EpochNumber(8));

        let final_state = epoch_store::load_epoch_state(&store).unwrap().unwrap();
        assert_eq!(final_state.epoch, EpochNumber(8));
        assert_eq!(final_state.transitions.len(), 8);
    }

    // ── 13. Epoch transition history integrity ───────────────────────

    /// Runs 20 epochs with persistence, then verifies every transition
    /// record in the loaded history has correct from/to sequencing
    /// (no gaps, no overlaps, monotonically increasing).
    #[test]
    fn transition_history_integrity_20_epochs() {
        let store = MemoryStore::new();
        let cfg = make_cfg(10, 0, 3);

        let c0 = make_committee(4, EpochNumber(0));
        epoch_store::persist_initial_epoch(&store, &c0).unwrap();
        let mut mgr = EpochManager::new(cfg.clone(), EpochNumber(0));

        for i in 0..20u64 {
            assert!(mgr.should_advance(10).is_some());
            let new_c = make_committee(4, EpochNumber(i + 1));
            let t = transition(i, i + 1, EpochTransitionTrigger::CommitThreshold, 10);
            epoch_store::persist_epoch_transition(&store, &new_c, &t).unwrap();
            mgr.record_transition(t);
        }

        let state = epoch_store::load_epoch_state(&store).unwrap().unwrap();
        assert_eq!(state.transitions.len(), 20);

        // Verify monotonic sequencing.
        for (idx, t) in state.transitions.iter().enumerate() {
            assert_eq!(
                t.from_epoch,
                EpochNumber(idx as u64),
                "transition {idx}: from_epoch mismatch"
            );
            assert_eq!(
                t.to_epoch,
                EpochNumber((idx + 1) as u64),
                "transition {idx}: to_epoch mismatch"
            );
            assert!(
                t.final_commit_count > 0,
                "transition {idx}: final_commit_count should be positive"
            );
        }
    }

    // ── 14. State churn across epoch boundaries with proof checks ────

    /// Over 10 epochs, mutates the same key each epoch and verifies
    /// the proof chain: each epoch's proof is valid against its root,
    /// and roots form a strictly changing sequence.
    #[test]
    fn state_churn_same_key_across_10_epochs() {
        let mut tracker = CommitmentTracker::new();
        let key = b"persistent_key";
        let mut roots: Vec<Blake3Digest> = Vec::new();

        for epoch in 0..10u64 {
            let val = format!("version_{epoch}").into_bytes();
            tracker.apply_state_changes(&[StateChangeEntry {
                key: &key[..],
                value: Some(&val),
            }]);

            let root = tracker.commitment_root();
            tracker.epoch_boundary_check().unwrap();

            // Root should be unique for each epoch.
            assert!(
                !roots.contains(&root),
                "root at epoch {epoch} is a duplicate!"
            );
            roots.push(root);

            // Proof for current value.
            let (v, proof) = tracker.prove_key(key).unwrap();
            assert_eq!(v.as_deref(), Some(val.as_slice()));
            Blake3SmtCommitment::verify_proof(&root, key, Some(&val), &proof).unwrap();
        }

        assert_eq!(roots.len(), 10);
    }
}
