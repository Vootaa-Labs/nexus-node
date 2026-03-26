// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! B-2: Governance interface consistency after recovery.
//!
//! Verifies that governance actions (slash, advance, history) maintain
//! semantic consistency across cold restarts. Exercises the round-trip:
//!
//! 1. **Slash persistence gap**: Slash modifies in-memory committee.
//!    Without explicit re-persistence, the slash is lost on restart.
//!    Test verifies re-persist+reload recovers slashed state.
//!
//! 2. **advance_epoch via EpochManager**: Tests that manual advance
//!    via EpochManager is correctly persisted and recovered.
//!
//! 3. **epoch_history consistency**: Verifies the full transition
//!    history survives recovery and re-indexing.
//!
//! 4. **Combined governance scenario**: Multi-epoch governance actions
//!    with slashes, advances, and history queries across restarts.

#[cfg(test)]
mod tests {
    use nexus_consensus::types::{EpochConfig, EpochTransitionTrigger};
    use nexus_consensus::{Committee, ConsensusEngine, EpochManager, ValidatorRegistry};
    use nexus_node::epoch_store;
    use nexus_primitives::{Amount, EpochNumber, ValidatorIndex};
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

    // ── 1. Slash persistence: round-trip through to_persistent/from_persistent ──

    /// Verifies that a slashed committee survives persistence round-trip
    /// when the committee is explicitly re-persisted after slash.
    #[test]
    fn slash_survives_persist_reload_when_committee_re_persisted() {
        let store = MemoryStore::new();

        let c0 = make_committee(7, EpochNumber(0));
        epoch_store::persist_initial_epoch(&store, &c0).unwrap();

        // Slash validator 2.
        let mut c0_slashed = c0;
        c0_slashed.slash(ValidatorIndex(2)).unwrap();
        assert_eq!(c0_slashed.active_validators().len(), 6);
        assert!(
            c0_slashed.all_validators()[2].is_slashed,
            "validator 2 should be slashed in memory"
        );

        // Re-persist the committee after slash — this is the fix pattern.
        epoch_store::persist_initial_epoch(&store, &c0_slashed).unwrap();

        // Cold restart.
        let state = epoch_store::load_epoch_state(&store).unwrap().unwrap();
        assert_eq!(state.committee.active_validators().len(), 6);
        assert!(
            state.committee.all_validators()[2].is_slashed,
            "slash must survive persist+reload"
        );
    }

    /// Documents the gap: if slash is NOT re-persisted, it is lost on restart.
    #[test]
    fn slash_lost_on_restart_without_re_persist() {
        let store = MemoryStore::new();

        let c0 = make_committee(5, EpochNumber(0));
        epoch_store::persist_initial_epoch(&store, &c0).unwrap();

        // Slash in memory only (no re-persist).
        let mut c0_local = c0;
        c0_local.slash(ValidatorIndex(1)).unwrap();
        assert_eq!(c0_local.active_validators().len(), 4);

        // Cold restart — loads original un-slashed committee.
        let state = epoch_store::load_epoch_state(&store).unwrap().unwrap();
        assert_eq!(
            state.committee.active_validators().len(),
            5,
            "without re-persist, slash is lost — all 5 validators restored"
        );
    }

    // ── 2. Slash + epoch advance: slashed state carries into new epoch ──

    /// Slashes a validator, then advances epoch with the slashed committee.
    /// Verifies the new epoch inherits the slash state after restart.
    #[test]
    fn slash_carries_into_next_epoch_and_survives_restart() {
        let store = MemoryStore::new();
        let c0 = make_committee(7, EpochNumber(0));
        epoch_store::persist_initial_epoch(&store, &c0).unwrap();
        let mut engine = ConsensusEngine::new(EpochNumber(0), c0);

        // Slash validator 0 during epoch 0.
        engine.committee_mut().slash(ValidatorIndex(0)).unwrap();
        assert_eq!(engine.committee().active_validators().len(), 6);

        // Advance to epoch 1 — pass the current (slashed) committee state
        // into the new committee so the slash is carried forward.
        let mut c1 = make_committee(7, EpochNumber(1));
        // Simulate carrying forward the slash: mark validator 0 as slashed in new committee.
        c1.slash(ValidatorIndex(0)).unwrap();

        let (t, _) = engine.advance_epoch(c1.clone(), EpochTransitionTrigger::Manual);
        epoch_store::persist_epoch_transition(&store, &c1, &t).unwrap();

        // Cold restart.
        let state = epoch_store::load_epoch_state(&store).unwrap().unwrap();
        assert_eq!(state.epoch, EpochNumber(1));
        assert_eq!(state.committee.active_validators().len(), 6);
        assert!(
            state.committee.all_validators()[0].is_slashed,
            "slash must carry into epoch 1 after restart"
        );
    }

    // ── 3. Multiple slashes across epochs ────────────────────────────

    /// Slashes different validators in different epochs, verifying
    /// cumulative slash state after each restart.
    #[test]
    fn cumulative_slashes_across_epochs() {
        let store = MemoryStore::new();
        let c0 = make_committee(10, EpochNumber(0));
        epoch_store::persist_initial_epoch(&store, &c0).unwrap();
        let mut engine = ConsensusEngine::new(EpochNumber(0), c0);

        // Epoch 0: slash validator 0.
        engine.committee_mut().slash(ValidatorIndex(0)).unwrap();

        // Epoch 1: carry forward slash + slash validator 3.
        let mut c1 = make_committee(10, EpochNumber(1));
        c1.slash(ValidatorIndex(0)).unwrap();
        c1.slash(ValidatorIndex(3)).unwrap();
        let (t1, _) = engine.advance_epoch(c1.clone(), EpochTransitionTrigger::CommitThreshold);
        epoch_store::persist_epoch_transition(&store, &c1, &t1).unwrap();

        // Verify: 2 slashed → 8 active.
        let state1 = epoch_store::load_epoch_state(&store).unwrap().unwrap();
        assert_eq!(state1.committee.active_validators().len(), 8);

        // Epoch 2: carry forward + slash validator 7.
        let engine2 = ConsensusEngine::new(state1.epoch, state1.committee);
        let mut c2 = make_committee(10, EpochNumber(2));
        c2.slash(ValidatorIndex(0)).unwrap();
        c2.slash(ValidatorIndex(3)).unwrap();
        c2.slash(ValidatorIndex(7)).unwrap();
        let (t2, _) = ConsensusEngine::new(EpochNumber(1), engine2.committee().clone())
            .advance_epoch(c2.clone(), EpochTransitionTrigger::CommitThreshold);
        epoch_store::persist_epoch_transition(&store, &c2, &t2).unwrap();

        // Cold restart.
        let state2 = epoch_store::load_epoch_state(&store).unwrap().unwrap();
        assert_eq!(state2.epoch, EpochNumber(2));
        assert_eq!(state2.committee.active_validators().len(), 7);

        // Specific validators slashed.
        let all = state2.committee.all_validators();
        assert!(all[0].is_slashed, "validator 0 must be slashed");
        assert!(all[3].is_slashed, "validator 3 must be slashed");
        assert!(all[7].is_slashed, "validator 7 must be slashed");
        assert!(!all[1].is_slashed, "validator 1 must NOT be slashed");
    }

    // ── 4. epoch_history recovered after restart ─────────────────────

    /// Advances 8 epochs with varied triggers and governance actions.
    /// After cold restart, verifies EpochManager::recover() provides
    /// the full, correct transition history via transitions().
    #[test]
    fn epoch_history_recovered_correctly() {
        let cfg = make_cfg(10, 0, 3);
        let store = MemoryStore::new();

        let c0 = make_committee(4, EpochNumber(0));
        epoch_store::persist_initial_epoch(&store, &c0).unwrap();
        let mut mgr = EpochManager::new(cfg.clone(), EpochNumber(0));
        let mut engine = ConsensusEngine::new(EpochNumber(0), c0);

        let triggers = [
            EpochTransitionTrigger::CommitThreshold,
            EpochTransitionTrigger::Manual,
            EpochTransitionTrigger::TimeElapsed,
            EpochTransitionTrigger::CommitThreshold,
            EpochTransitionTrigger::Manual,
            EpochTransitionTrigger::CommitThreshold,
            EpochTransitionTrigger::TimeElapsed,
            EpochTransitionTrigger::Manual,
        ];

        for (i, trigger) in triggers.iter().enumerate() {
            let next = EpochNumber(i as u64 + 1);
            let new_c = make_committee(4, next);
            let (t, _) = engine.advance_epoch(new_c.clone(), *trigger);
            epoch_store::persist_epoch_transition(&store, &new_c, &t).unwrap();
            mgr.record_transition(t);
        }

        assert_eq!(engine.epoch(), EpochNumber(8));
        assert_eq!(mgr.transitions().len(), 8);

        // Cold restart.
        let state = epoch_store::load_epoch_state(&store).unwrap().unwrap();
        let mgr2 = EpochManager::recover(
            cfg,
            state.epoch,
            state.epoch_started_at,
            state.transitions.clone(),
        );

        // Verify full history.
        assert_eq!(mgr2.current_epoch(), EpochNumber(8));
        assert_eq!(mgr2.transitions().len(), 8);

        for (i, t) in mgr2.transitions().iter().enumerate() {
            assert_eq!(t.from_epoch, EpochNumber(i as u64));
            assert_eq!(t.to_epoch, EpochNumber(i as u64 + 1));
            assert_eq!(t.trigger, triggers[i], "trigger mismatch at index {i}");
        }
    }

    // ── 5. Manual epoch advance (simulating admin API) ───────────────

    /// Simulates the admin advance_epoch flow:
    /// 1. EpochManager decides not to advance (commit threshold not met)
    /// 2. Operator triggers manual advance
    /// 3. Advance is persisted
    /// 4. Cold restart recovers with the manual advance in history
    #[test]
    fn manual_advance_epoch_persists_and_recovers() {
        let cfg = make_cfg(100, 0, 5); // High threshold — won't auto-advance
        let store = MemoryStore::new();

        let c0 = make_committee(4, EpochNumber(0));
        epoch_store::persist_initial_epoch(&store, &c0).unwrap();
        let mut mgr = EpochManager::new(cfg.clone(), EpochNumber(0));
        let mut engine = ConsensusEngine::new(EpochNumber(0), c0);

        // Threshold not met — should_advance returns None.
        assert!(mgr.should_advance(10).is_none());

        // Manual advance (operator action via admin API).
        let c1 = make_committee(4, EpochNumber(1));
        let (t, _) = engine.advance_epoch(c1.clone(), EpochTransitionTrigger::Manual);
        epoch_store::persist_epoch_transition(&store, &c1, &t).unwrap();
        mgr.record_transition(t);

        assert_eq!(mgr.current_epoch(), EpochNumber(1));
        assert_eq!(mgr.transitions()[0].trigger, EpochTransitionTrigger::Manual);

        // Cold restart.
        let state = epoch_store::load_epoch_state(&store).unwrap().unwrap();
        let mgr2 = EpochManager::recover(
            cfg,
            state.epoch,
            state.epoch_started_at,
            state.transitions.clone(),
        );
        assert_eq!(mgr2.current_epoch(), EpochNumber(1));
        assert_eq!(mgr2.transitions().len(), 1);
        assert_eq!(
            mgr2.transitions()[0].trigger,
            EpochTransitionTrigger::Manual
        );
    }

    // ── 6. Slash + advance + history combined stress ─────────────────

    /// 10-epoch scenario combining slash, advance (auto + manual), and
    /// full history verification after two cold restarts.
    #[test]
    fn slash_advance_history_combined_10_epochs() {
        let cfg = make_cfg(10, 0, 3);
        let store = MemoryStore::new();

        let c0 = make_committee(10, EpochNumber(0));
        epoch_store::persist_initial_epoch(&store, &c0).unwrap();
        let mut mgr = EpochManager::new(cfg.clone(), EpochNumber(0));
        let mut engine = ConsensusEngine::new(EpochNumber(0), c0);

        // Phase 1: epochs 0→3, auto advance, slash validators 1 and 5.
        for i in 0..3u64 {
            let next = EpochNumber(i + 1);
            let mut new_c = make_committee(10, next);
            // Carry forward previous slashes.
            if i >= 1 {
                new_c.slash(ValidatorIndex(1)).unwrap();
            }
            if i >= 2 {
                new_c.slash(ValidatorIndex(5)).unwrap();
            }

            // Slash at appropriate epoch.
            if i == 0 {
                engine.committee_mut().slash(ValidatorIndex(1)).unwrap();
                new_c.slash(ValidatorIndex(1)).unwrap();
            }
            if i == 1 {
                // Slash validator 5 during epoch 1→2.
                engine.committee_mut().slash(ValidatorIndex(5)).unwrap();
                new_c.slash(ValidatorIndex(5)).unwrap();
            }

            let (t, _) =
                engine.advance_epoch(new_c.clone(), EpochTransitionTrigger::CommitThreshold);
            epoch_store::persist_epoch_transition(&store, &new_c, &t).unwrap();
            mgr.record_transition(t);
        }

        assert_eq!(engine.epoch(), EpochNumber(3));
        assert_eq!(engine.committee().active_validators().len(), 8);

        // ── Cold restart 1 ──
        let state1 = epoch_store::load_epoch_state(&store).unwrap().unwrap();
        mgr = EpochManager::recover(
            cfg.clone(),
            state1.epoch,
            state1.epoch_started_at,
            state1.transitions.clone(),
        );
        engine = ConsensusEngine::new(state1.epoch, state1.committee);

        assert_eq!(engine.committee().active_validators().len(), 8);

        // Phase 2: epochs 3→6, manual advance at epoch 5.
        for i in 3..6u64 {
            let next = EpochNumber(i + 1);
            let mut new_c = make_committee(10, next);
            new_c.slash(ValidatorIndex(1)).unwrap();
            new_c.slash(ValidatorIndex(5)).unwrap();

            let trigger = if i == 4 {
                EpochTransitionTrigger::Manual
            } else {
                EpochTransitionTrigger::CommitThreshold
            };

            let (t, _) = engine.advance_epoch(new_c.clone(), trigger);
            epoch_store::persist_epoch_transition(&store, &new_c, &t).unwrap();
            mgr.record_transition(t);
        }

        assert_eq!(engine.epoch(), EpochNumber(6));

        // Phase 3: slash validator 9 at epoch 6, advance to epoch 10.
        for i in 6..10u64 {
            let next = EpochNumber(i + 1);
            let mut new_c = make_committee(10, next);
            new_c.slash(ValidatorIndex(1)).unwrap();
            new_c.slash(ValidatorIndex(5)).unwrap();
            new_c.slash(ValidatorIndex(9)).unwrap();

            if i == 6 {
                engine.committee_mut().slash(ValidatorIndex(9)).unwrap();
            }

            let (t, _) =
                engine.advance_epoch(new_c.clone(), EpochTransitionTrigger::CommitThreshold);
            epoch_store::persist_epoch_transition(&store, &new_c, &t).unwrap();
            mgr.record_transition(t);
        }

        // ── Cold restart 2 ──
        let state2 = epoch_store::load_epoch_state(&store).unwrap().unwrap();
        let mgr_final = EpochManager::recover(
            cfg,
            state2.epoch,
            state2.epoch_started_at,
            state2.transitions.clone(),
        );

        assert_eq!(mgr_final.current_epoch(), EpochNumber(10));
        assert_eq!(mgr_final.transitions().len(), 10);
        assert_eq!(state2.committee.active_validators().len(), 7);

        // Verify the manual advance is at index 4 (epoch 4→5).
        assert_eq!(
            mgr_final.transitions()[4].trigger,
            EpochTransitionTrigger::Manual
        );

        // Verify all others are CommitThreshold.
        for (i, t) in mgr_final.transitions().iter().enumerate() {
            if i != 4 {
                assert_eq!(
                    t.trigger,
                    EpochTransitionTrigger::CommitThreshold,
                    "transition {i} should be CommitThreshold"
                );
            }
        }

        // Verify slashed validators.
        let all = state2.committee.all_validators();
        assert!(all[1].is_slashed, "validator 1 slashed at epoch 1");
        assert!(all[5].is_slashed, "validator 5 slashed at epoch 2");
        assert!(all[9].is_slashed, "validator 9 slashed at epoch 7");
    }

    // ── 7. Quorum recalculation after slash + recovery ───────────────

    /// Verifies quorum thresholds are correctly recalculated after
    /// slashing and then recovering from persistence.
    #[test]
    fn quorum_correct_after_slash_and_recovery() {
        let store = MemoryStore::new();

        // 7 validators, stake=1000 each: total=7000, quorum = 7000*2/3+1 = 4667.
        let mut c = make_committee(7, EpochNumber(0));
        assert_eq!(c.quorum_threshold(), Amount(4667));

        // Slash 1: active=6, total=6000, quorum = 6000*2/3+1 = 4001.
        c.slash(ValidatorIndex(0)).unwrap();
        assert_eq!(c.quorum_threshold(), Amount(4001));

        // Persist and reload to verify quorum recalculation.
        epoch_store::persist_initial_epoch(&store, &c).unwrap();
        let state = epoch_store::load_epoch_state(&store).unwrap().unwrap();

        // from_persistent calls compute_stats, so quorum should match.
        assert_eq!(
            state.committee.quorum_threshold(),
            c.quorum_threshold(),
            "quorum must match after recovery"
        );
        assert_eq!(state.committee.active_validators().len(), 6);

        // Slash another.
        let mut c2 = state.committee;
        c2.slash(ValidatorIndex(3)).unwrap();
        // 5 active: total=5000, quorum = 5000*2/3+1 = 3334.
        assert_eq!(c2.active_validators().len(), 5);
        let expected_quorum = c2.quorum_threshold();

        epoch_store::persist_initial_epoch(&store, &c2).unwrap();
        let state2 = epoch_store::load_epoch_state(&store).unwrap().unwrap();
        assert_eq!(state2.committee.quorum_threshold(), expected_quorum);
    }

    // ── 8. Double slash (idempotency) ────────────────────────────────

    /// Verifies that slashing an already-slashed validator returns an
    /// error and does not corrupt state, even across persist/reload.
    #[test]
    fn double_slash_error_survives_restart() {
        let store = MemoryStore::new();
        let mut c = make_committee(5, EpochNumber(0));
        c.slash(ValidatorIndex(2)).unwrap();

        // Double slash should error.
        let err = c.slash(ValidatorIndex(2));
        assert!(err.is_err(), "double slash must error");

        // Persist and reload.
        epoch_store::persist_initial_epoch(&store, &c).unwrap();
        let state = epoch_store::load_epoch_state(&store).unwrap().unwrap();

        // Double slash on recovered committee should also error.
        let mut recovered = state.committee;
        let err2 = recovered.slash(ValidatorIndex(2));
        assert!(err2.is_err(), "double slash must error after recovery");

        // State is not corrupted.
        assert_eq!(recovered.active_validators().len(), 4);
    }

    // ── 9. EpochManager recover with empty history ───────────────────

    /// Simulates recovery when no transitions have been recorded yet
    /// (e.g., the node was just started and no epoch advances occurred).
    #[test]
    fn recover_with_empty_history() {
        let cfg = make_cfg(10, 0, 3);
        let store = MemoryStore::new();

        let c0 = make_committee(4, EpochNumber(0));
        epoch_store::persist_initial_epoch(&store, &c0).unwrap();

        // Immediate restart — no transitions recorded.
        let state = epoch_store::load_epoch_state(&store).unwrap().unwrap();
        let mgr = EpochManager::recover(
            cfg,
            state.epoch,
            state.epoch_started_at,
            state.transitions.clone(),
        );

        assert_eq!(mgr.current_epoch(), EpochNumber(0));
        assert!(mgr.transitions().is_empty());
        assert!(
            mgr.should_advance(10).is_some(),
            "should still be able to advance from epoch 0"
        );
    }

    // ── 10. History integrity: no gaps after mixed gov actions ────────

    /// Performs 15 governance operations (slash, advance, manual advance)
    /// in mixed order, then verifies transition history is gap-free and
    /// monotonically sequenced after two cold restarts.
    #[test]
    fn history_integrity_after_mixed_governance_15_actions() {
        let cfg = make_cfg(10, 0, 3);
        let store = MemoryStore::new();

        let c0 = make_committee(10, EpochNumber(0));
        epoch_store::persist_initial_epoch(&store, &c0).unwrap();
        let mut mgr = EpochManager::new(cfg.clone(), EpochNumber(0));
        let mut engine = ConsensusEngine::new(EpochNumber(0), c0);

        let mut slashed_validators: Vec<ValidatorIndex> = Vec::new();

        for i in 0..15u64 {
            let next = EpochNumber(i + 1);
            let mut new_c = make_committee(10, next);

            // Carry forward all previous slashes.
            for &idx in &slashed_validators {
                new_c.slash(idx).unwrap();
            }

            // Slash a new validator every 5 epochs.
            if i % 5 == 0 && (i / 5) < 10 {
                let slash_idx = ValidatorIndex((i / 5) as u32);
                engine.committee_mut().slash(slash_idx).ok(); // May already be slashed
                if new_c.slash(slash_idx).is_ok() {
                    slashed_validators.push(slash_idx);
                }
            }

            let trigger = if i % 4 == 3 {
                EpochTransitionTrigger::Manual
            } else {
                EpochTransitionTrigger::CommitThreshold
            };

            let (t, _) = engine.advance_epoch(new_c.clone(), trigger);
            epoch_store::persist_epoch_transition(&store, &new_c, &t).unwrap();
            mgr.record_transition(t);

            // Cold restart every 5 epochs.
            if (i + 1) % 5 == 0 {
                let state = epoch_store::load_epoch_state(&store).unwrap().unwrap();
                mgr = EpochManager::recover(
                    cfg.clone(),
                    state.epoch,
                    state.epoch_started_at,
                    state.transitions.clone(),
                );
                engine = ConsensusEngine::new(state.epoch, state.committee);
            }
        }

        // Final verification.
        let final_state = epoch_store::load_epoch_state(&store).unwrap().unwrap();
        let final_mgr = EpochManager::recover(
            cfg,
            final_state.epoch,
            final_state.epoch_started_at,
            final_state.transitions.clone(),
        );

        assert_eq!(final_mgr.current_epoch(), EpochNumber(15));
        assert_eq!(final_mgr.transitions().len(), 15);

        // Verify gap-free, monotonic sequencing.
        for (i, t) in final_mgr.transitions().iter().enumerate() {
            assert_eq!(t.from_epoch, EpochNumber(i as u64), "gap at transition {i}");
            assert_eq!(
                t.to_epoch,
                EpochNumber(i as u64 + 1),
                "gap at transition {i}"
            );
        }

        // Verify manual triggers at indices 3, 7, 11.
        for idx in [3, 7, 11] {
            assert_eq!(
                final_mgr.transitions()[idx].trigger,
                EpochTransitionTrigger::Manual,
                "transition {idx} should be Manual"
            );
        }

        // Verify slash count: epochs 0, 5, 10 → 3 validators slashed.
        assert_eq!(final_state.committee.active_validators().len(), 7);
    }
}
