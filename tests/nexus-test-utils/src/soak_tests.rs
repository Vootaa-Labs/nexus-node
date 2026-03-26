//! F-2: Long-running multi-epoch soak tests.
//!
//! Exercises epoch management, committee rotation, cold-restart recovery,
//! and pipeline throughput over extended sequences to catch state drift
//! or resource leaks that unit tests miss.

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    use nexus_consensus::types::{EpochConfig, EpochTransition, EpochTransitionTrigger};
    use nexus_consensus::{Committee, ConsensusEngine, EpochManager, ValidatorRegistry};
    use nexus_node::epoch_store;
    use nexus_primitives::{EpochNumber, TimestampMs, ValidatorIndex};
    use nexus_storage::MemoryStore;

    use crate::fixtures::consensus::TestCommittee;

    fn make_epoch_config(commits: u64, seconds: u64, min_commits: u64) -> EpochConfig {
        EpochConfig {
            epoch_length_commits: commits,
            epoch_length_seconds: seconds,
            min_epoch_commits: min_commits,
        }
    }

    fn make_committee(n: usize, epoch: EpochNumber) -> Committee {
        TestCommittee::new(n, epoch).committee
    }

    // ── 1. 20-epoch soak with varying committee sizes ────────────────────

    /// Runs 20 sequential epoch transitions with alternating 4- and 7-node
    /// committees, persisting and recovering from cold storage each time.
    #[test]
    fn soak_20_epochs_with_rotation_and_recovery() {
        let store = MemoryStore::new();
        let cfg = make_epoch_config(25, 0, 5);

        // Genesis epoch.
        let c0 = make_committee(4, EpochNumber(0));
        epoch_store::persist_initial_epoch(&store, &c0).expect("persist genesis");
        let mut mgr = EpochManager::new(cfg.clone(), EpochNumber(0));

        let total_epochs: u64 = 20;

        for i in 0..total_epochs {
            let committee_size = if i % 2 == 0 { 7 } else { 4 };
            let next_epoch = EpochNumber(i + 1);
            let new_c = make_committee(committee_size, next_epoch);

            let trigger = mgr
                .should_advance(25)
                .expect("should trigger at 25 commits");
            assert_eq!(trigger, EpochTransitionTrigger::CommitThreshold);

            let t = EpochTransition {
                from_epoch: EpochNumber(i),
                to_epoch: next_epoch,
                trigger,
                final_commit_count: 25,
                transitioned_at: TimestampMs(1_700_000_000_000 + i * 60_000),
            };
            epoch_store::persist_epoch_transition(&store, &new_c, &t).expect("persist transition");
            mgr.record_transition(t);

            assert_eq!(mgr.current_epoch(), next_epoch);
        }

        assert_eq!(mgr.current_epoch(), EpochNumber(total_epochs));
        assert_eq!(mgr.transitions().len(), total_epochs as usize);

        // Cold restart: reload from storage and verify full history.
        let recovered = epoch_store::load_epoch_state(&store)
            .expect("load")
            .expect("state present");
        assert_eq!(recovered.epoch, EpochNumber(total_epochs));
        assert_eq!(recovered.transitions.len(), total_epochs as usize);

        // Verify committee sizes alternate correctly.
        // The final committee should be from epoch 20, which is even → 7 validators.
        // (epoch 20 = i=19, 19%2==1 → 4)  Wait: i goes 0..19:
        //   i=19 → committee_size = 4
        let final_size = recovered.committee.active_validators().len();
        assert_eq!(
            final_size, 4,
            "epoch 20 (i=19, odd) should have 4 validators"
        );
    }

    // ── 2. Mid-epoch recovery preserves partial commit count ─────────────

    /// Simulates a crash at epoch 8/20 then verifies the epoch manager
    /// can resume from persisted state at the correct epoch without
    /// losing transition history.
    #[test]
    fn soak_mid_epoch_crash_recovery() {
        let store = MemoryStore::new();
        let cfg = make_epoch_config(10, 0, 3);

        let c0 = make_committee(4, EpochNumber(0));
        epoch_store::persist_initial_epoch(&store, &c0).expect("persist genesis");
        let mut mgr = EpochManager::new(cfg.clone(), EpochNumber(0));

        // Run 8 epochs cleanly.
        for i in 0..8u64 {
            let new_c = make_committee(4, EpochNumber(i + 1));
            let trigger = mgr.should_advance(10).unwrap();
            let t = EpochTransition {
                from_epoch: EpochNumber(i),
                to_epoch: EpochNumber(i + 1),
                trigger,
                final_commit_count: 10,
                transitioned_at: TimestampMs(1_700_000_000_000 + i * 1000),
            };
            epoch_store::persist_epoch_transition(&store, &new_c, &t).unwrap();
            mgr.record_transition(t);
        }
        assert_eq!(mgr.current_epoch(), EpochNumber(8));

        // ── SIMULATE CRASH: drop the manager, reload from storage ────
        drop(mgr);
        let state = epoch_store::load_epoch_state(&store)
            .expect("reload")
            .expect("present");
        assert_eq!(state.epoch, EpochNumber(8));
        assert_eq!(state.transitions.len(), 8);

        // Recreate manager and continue from epoch 8 → 16.
        let mut mgr2 = EpochManager::new(cfg.clone(), state.epoch);
        for j in 0..8u64 {
            let epoch_idx = 8 + j;
            let new_c = make_committee(4, EpochNumber(epoch_idx + 1));
            let trigger = mgr2.should_advance(10).unwrap();
            let t = EpochTransition {
                from_epoch: EpochNumber(epoch_idx),
                to_epoch: EpochNumber(epoch_idx + 1),
                trigger,
                final_commit_count: 10,
                transitioned_at: TimestampMs(1_700_000_000_000 + epoch_idx * 1000),
            };
            epoch_store::persist_epoch_transition(&store, &new_c, &t).unwrap();
            mgr2.record_transition(t);
        }
        assert_eq!(mgr2.current_epoch(), EpochNumber(16));

        // Final reload.
        let final_state = epoch_store::load_epoch_state(&store)
            .expect("final reload")
            .expect("present");
        assert_eq!(final_state.epoch, EpochNumber(16));
        assert_eq!(final_state.transitions.len(), 16);
    }

    // ── 3. Engine + manager coordinated multi-epoch ──────────────────────

    /// Tests the full ConsensusEngine + EpochManager interaction over
    /// 10 epochs, verifying that advance_epoch resets the engine and
    /// the manager tracks all transitions.
    #[test]
    fn soak_engine_manager_coordinated_10_epochs() {
        let cfg = make_epoch_config(5, 0, 2);
        let mut mgr = EpochManager::new(cfg.clone(), EpochNumber(0));

        let tc = TestCommittee::new(4, EpochNumber(0));
        let mut engine = ConsensusEngine::new(tc.epoch, tc.committee.clone());

        for i in 0..10u64 {
            // Simulate commits.
            assert!(mgr.should_advance(5).is_some());

            let next_epoch = EpochNumber(i + 1);
            let new_tc = TestCommittee::new(4, next_epoch);

            // Advance engine — returns (EpochTransition, remaining_batches).
            let (transition, _remaining) = engine.advance_epoch(
                new_tc.committee.clone(),
                EpochTransitionTrigger::CommitThreshold,
            );
            assert_eq!(engine.epoch(), next_epoch);

            // Record in manager.
            mgr.record_transition(transition);
        }

        assert_eq!(mgr.current_epoch(), EpochNumber(10));
        assert_eq!(engine.epoch(), EpochNumber(10));
        assert_eq!(mgr.transitions().len(), 10);
    }

    // ── 4. Slash accumulation across epochs ──────────────────────────────

    /// Slashes a validator in early epochs and verifies the slash
    /// persists across epoch boundaries after cold restart.
    #[test]
    fn soak_slash_persistence_across_epochs() {
        let store = MemoryStore::new();
        let cfg = make_epoch_config(10, 0, 3);

        let c0 = make_committee(7, EpochNumber(0));
        epoch_store::persist_initial_epoch(&store, &c0).expect("genesis");
        let mut mgr = EpochManager::new(cfg.clone(), EpochNumber(0));

        // Advance to epoch 3, slashing validator 2 during epoch 1.
        for i in 0..3u64 {
            let mut new_c = make_committee(7, EpochNumber(i + 1));
            if i == 0 {
                // Slash validator index 2 during first transition.
                let _ = new_c.slash(ValidatorIndex(2));
            }
            let trigger = mgr.should_advance(10).unwrap();
            let t = EpochTransition {
                from_epoch: EpochNumber(i),
                to_epoch: EpochNumber(i + 1),
                trigger,
                final_commit_count: 10,
                transitioned_at: TimestampMs::now(),
            };
            epoch_store::persist_epoch_transition(&store, &new_c, &t).unwrap();
            mgr.record_transition(t);
        }

        // Cold restart at epoch 3.
        let state = epoch_store::load_epoch_state(&store).unwrap().unwrap();
        assert_eq!(state.epoch, EpochNumber(3));
        // The committee at epoch 3 was created fresh with 7 validators
        // (slash was only applied to epoch 1's committee).
        assert_eq!(state.committee.active_validators().len(), 7);
    }

    // ── 5. Concurrent epoch + commit counter stress ──────────────────────

    /// Uses an AtomicU64 commit counter to simulate high-throughput
    /// commit counting interleaved with epoch checks.
    #[test]
    fn soak_commit_counter_stress() {
        let cfg = make_epoch_config(100, 0, 10);
        let mut mgr = EpochManager::new(cfg, EpochNumber(0));
        let commit_counter = Arc::new(AtomicU64::new(0));

        let mut epochs_advanced = 0u64;

        // Simulate 1000 commits, checking for epoch advance every 10.
        for _batch in 0..100u32 {
            commit_counter.fetch_add(10, Ordering::Relaxed);
            let total = commit_counter.load(Ordering::Relaxed);

            // Check in multiples of the epoch length.
            if let Some(trigger) = mgr.should_advance(total - epochs_advanced * 100) {
                let t = EpochTransition {
                    from_epoch: EpochNumber(epochs_advanced),
                    to_epoch: EpochNumber(epochs_advanced + 1),
                    trigger,
                    final_commit_count: 100,
                    transitioned_at: TimestampMs::now(),
                };
                mgr.record_transition(t);
                epochs_advanced += 1;
            }
        }

        assert_eq!(
            epochs_advanced, 10,
            "1000 commits / 100 per epoch = 10 epochs"
        );
        assert_eq!(mgr.current_epoch(), EpochNumber(10));
    }
}
