// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! E-3: Multi-epoch integration tests.
//!
//! Tests covering epoch transitions, committee rotation, cold restart
//! recovery, and epoch-aware RPC responses.

use nexus_consensus::types::{EpochConfig, EpochTransition, EpochTransitionTrigger};
use nexus_consensus::{Committee, ConsensusEngine, EpochManager, ValidatorRegistry};
use nexus_node::epoch_store;
use nexus_primitives::{EpochNumber, TimestampMs, ValidatorIndex};
use nexus_storage::MemoryStore;

use crate::fixtures::consensus::TestCommittee;

// ── Helpers ──────────────────────────────────────────────────────────────────

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

fn make_store() -> MemoryStore {
    MemoryStore::new()
}

// ── 1. Epoch Manager transition round-trip ───────────────────────────────────

#[test]
fn epoch_manager_full_lifecycle() {
    let cfg = make_epoch_config(100, 0, 10);
    let mut mgr = EpochManager::new(cfg, EpochNumber(0));

    // Below minimum — no advance.
    assert!(mgr.should_advance(5).is_none());

    // Above minimum but below threshold — no advance.
    assert!(mgr.should_advance(50).is_none());

    // At threshold — advance.
    let trigger = mgr
        .should_advance(100)
        .expect("should trigger at threshold");
    assert_eq!(trigger, EpochTransitionTrigger::CommitThreshold);

    // Simulate transition.
    let transition = EpochTransition {
        from_epoch: EpochNumber(0),
        to_epoch: EpochNumber(1),
        trigger,
        final_commit_count: 100,
        transitioned_at: TimestampMs::now(),
    };
    mgr.record_transition(transition);
    assert_eq!(mgr.current_epoch(), EpochNumber(1));
    assert_eq!(mgr.transitions().len(), 1);

    // Second epoch at threshold.
    let trigger2 = mgr.should_advance(100).expect("should trigger again");
    let t2 = EpochTransition {
        from_epoch: EpochNumber(1),
        to_epoch: EpochNumber(2),
        trigger: trigger2,
        final_commit_count: 100,
        transitioned_at: TimestampMs::now(),
    };
    mgr.record_transition(t2);
    assert_eq!(mgr.current_epoch(), EpochNumber(2));
    assert_eq!(mgr.transitions().len(), 2);
}

// ── 2. ConsensusEngine epoch advance ─────────────────────────────────────────

#[test]
fn engine_advance_epoch_drains_committed() {
    let tc = TestCommittee::new(4, EpochNumber(0));
    let mut engine = ConsensusEngine::new(tc.epoch, tc.committee.clone());
    assert_eq!(engine.epoch(), EpochNumber(0));

    // Advance epoch to 1 with same committee.
    let next_committee = make_committee(4, EpochNumber(1));
    let (transition, remaining) =
        engine.advance_epoch(next_committee, EpochTransitionTrigger::Manual);
    assert_eq!(transition.from_epoch, EpochNumber(0));
    assert_eq!(transition.to_epoch, EpochNumber(1));
    assert_eq!(engine.epoch(), EpochNumber(1));
    // No commits happened so remaining batches should be empty.
    assert!(remaining.is_empty());
}

// ── 3. Committee rotation (different validator set per epoch) ────────────────

#[test]
fn committee_rotation_changes_validators() {
    let tc0 = TestCommittee::new(4, EpochNumber(0));
    let mut engine = ConsensusEngine::new(tc0.epoch, tc0.committee.clone());

    // Original committee has 4 validators.
    assert_eq!(engine.committee().active_validators().len(), 4);

    // Rotate to a 7-node committee.
    let tc1 = TestCommittee::new(7, EpochNumber(1));
    let (transition, _) =
        engine.advance_epoch(tc1.committee.clone(), EpochTransitionTrigger::Manual);
    assert_eq!(transition.to_epoch, EpochNumber(1));
    assert_eq!(engine.committee().active_validators().len(), 7);

    // Rotate back down to 3-node committee.
    let tc2 = TestCommittee::new(3, EpochNumber(2));
    let (transition2, _) =
        engine.advance_epoch(tc2.committee.clone(), EpochTransitionTrigger::Manual);
    assert_eq!(transition2.to_epoch, EpochNumber(2));
    assert_eq!(engine.committee().active_validators().len(), 3);
}

// ── 4. PersistentCommittee round-trip ────────────────────────────────────────

#[test]
fn committee_persistent_roundtrip() {
    let tc = TestCommittee::new(5, EpochNumber(7));
    let snap = tc.committee.to_persistent();
    assert_eq!(snap.epoch, EpochNumber(7));
    assert_eq!(snap.validators.len(), 5);

    let restored = Committee::from_persistent(snap).expect("roundtrip");
    assert_eq!(restored.epoch(), EpochNumber(7));
    assert_eq!(restored.active_validators().len(), 5);
    // Quorum should be recomputed correctly.
    assert_eq!(restored.active_count(), tc.committee.active_count());
}

// ── 5. Epoch store: persist → load round-trip ────────────────────────────────

#[test]
fn epoch_store_genesis_roundtrip() {
    let store = make_store();
    let committee = make_committee(4, EpochNumber(0));

    epoch_store::persist_initial_epoch(&store, &committee).expect("persist");
    let state = epoch_store::load_epoch_state(&store)
        .expect("load should not error")
        .expect("should have state");

    assert_eq!(state.epoch, EpochNumber(0));
    assert_eq!(state.committee.active_validators().len(), 4);
    assert!(state.transitions.is_empty());
}

// ── 6. Epoch store: transition persist → load ────────────────────────────────

#[test]
fn epoch_store_transition_roundtrip() {
    let store = make_store();
    let c0 = make_committee(4, EpochNumber(0));
    epoch_store::persist_initial_epoch(&store, &c0).expect("persist initial");

    // Simulate transition 0 → 1.
    let c1 = make_committee(5, EpochNumber(1));
    let t = EpochTransition {
        from_epoch: EpochNumber(0),
        to_epoch: EpochNumber(1),
        trigger: EpochTransitionTrigger::CommitThreshold,
        final_commit_count: 10_000,
        transitioned_at: TimestampMs::now(),
    };
    epoch_store::persist_epoch_transition(&store, &c1, &t).expect("persist transition");

    // Load and verify.
    let state = epoch_store::load_epoch_state(&store)
        .expect("load should not error")
        .expect("should have state");
    assert_eq!(state.epoch, EpochNumber(1));
    assert_eq!(state.committee.active_validators().len(), 5);
    assert_eq!(state.transitions.len(), 1);
    assert_eq!(
        state.transitions[0].trigger,
        EpochTransitionTrigger::CommitThreshold
    );
}

// ── 7. Cold restart recovery: epoch manager + store ──────────────────────────

#[test]
fn cold_restart_recovery() {
    let store = make_store();
    let cfg = make_epoch_config(100, 0, 10);

    // First boot: persist initial state.
    let c0 = make_committee(4, EpochNumber(0));
    epoch_store::persist_initial_epoch(&store, &c0).expect("persist");

    let mut mgr = EpochManager::new(cfg.clone(), EpochNumber(0));

    // Advance through two epochs.
    let c1 = make_committee(5, EpochNumber(1));
    let t1 = EpochTransition {
        from_epoch: EpochNumber(0),
        to_epoch: EpochNumber(1),
        trigger: EpochTransitionTrigger::CommitThreshold,
        final_commit_count: 100,
        transitioned_at: TimestampMs(1_700_000_000_000),
    };
    epoch_store::persist_epoch_transition(&store, &c1, &t1).expect("persist t1");
    mgr.record_transition(t1.clone());

    let c2 = make_committee(6, EpochNumber(2));
    let t2 = EpochTransition {
        from_epoch: EpochNumber(1),
        to_epoch: EpochNumber(2),
        trigger: EpochTransitionTrigger::TimeElapsed,
        final_commit_count: 200,
        transitioned_at: TimestampMs(1_700_100_000_000),
    };
    epoch_store::persist_epoch_transition(&store, &c2, &t2).expect("persist t2");
    mgr.record_transition(t2);

    // Simulate cold restart: load from store and recover epoch manager.
    let state = epoch_store::load_epoch_state(&store)
        .expect("load should not error")
        .expect("should have state");
    let recovered =
        EpochManager::recover(cfg, state.epoch, state.epoch_started_at, state.transitions);

    assert_eq!(recovered.current_epoch(), EpochNumber(2));
    assert_eq!(recovered.transitions().len(), 2);
    assert_eq!(
        recovered.transitions()[0].trigger,
        EpochTransitionTrigger::CommitThreshold
    );
    assert_eq!(
        recovered.transitions()[1].trigger,
        EpochTransitionTrigger::TimeElapsed
    );
}

// ── 8. Empty store returns None ──────────────────────────────────────────────

#[test]
fn empty_store_returns_none() {
    let store = make_store();
    let result = epoch_store::load_epoch_state(&store).expect("should not error");
    assert!(result.is_none());
}

// ── 9. Slash then advance: slashed validators carry through ──────────────────

#[test]
fn slash_then_advance_preserves_slash() {
    let tc = TestCommittee::new(5, EpochNumber(0));
    let mut engine = ConsensusEngine::new(tc.epoch, tc.committee.clone());

    // Slash validator 2.
    engine
        .committee_mut()
        .slash(ValidatorIndex(2))
        .expect("slash");

    // Verify the slash.
    let v2 = engine
        .committee()
        .validator_info(ValidatorIndex(2))
        .expect("exists");
    assert!(v2.is_slashed);

    // Snapshot committee and verify it includes the slash.
    let snap = engine.committee().to_persistent();
    assert!(snap.validators[2].is_slashed);

    // Persist the slashed committee and reload.
    let store = make_store();
    epoch_store::persist_initial_epoch(&store, engine.committee()).expect("persist");
    let state = epoch_store::load_epoch_state(&store)
        .expect("load should not error")
        .expect("should have state");
    let v2_loaded = state
        .committee
        .validator_info(ValidatorIndex(2))
        .expect("exists");
    assert!(v2_loaded.is_slashed);
}

// ── 10. EpochConfig disabled transitions ─────────────────────────────────────

#[test]
fn disabled_transitions_never_trigger() {
    let cfg = make_epoch_config(0, 0, 0); // all disabled
    let mgr = EpochManager::new(cfg, EpochNumber(0));
    // Even with massive commit count, should not trigger.
    assert!(mgr.should_advance(1_000_000).is_none());
}

// ── 11. Multi-epoch sequential transitions ───────────────────────────────────

#[test]
fn multi_epoch_sequential_transitions() {
    let store = make_store();
    let cfg = make_epoch_config(50, 0, 5);

    let c0 = make_committee(4, EpochNumber(0));
    epoch_store::persist_initial_epoch(&store, &c0).expect("persist initial");
    let mut mgr = EpochManager::new(cfg.clone(), EpochNumber(0));

    // Simulate 5 epoch transitions.
    for i in 0..5u64 {
        let new_c = make_committee(4, EpochNumber(i + 1));
        let trigger = mgr.should_advance(50).expect("should trigger at threshold");
        let t = EpochTransition {
            from_epoch: EpochNumber(i),
            to_epoch: EpochNumber(i + 1),
            trigger,
            final_commit_count: 50,
            transitioned_at: TimestampMs(1_700_000_000_000 + i * 100_000),
        };
        epoch_store::persist_epoch_transition(&store, &new_c, &t).expect("persist");
        mgr.record_transition(t);
    }

    assert_eq!(mgr.current_epoch(), EpochNumber(5));
    assert_eq!(mgr.transitions().len(), 5);

    // Cold restart recovery.
    let state = epoch_store::load_epoch_state(&store)
        .expect("load")
        .expect("state present");
    assert_eq!(state.epoch, EpochNumber(5));
    assert_eq!(state.transitions.len(), 5);
}

// ── 12. EpochInfoDto construction from manager + engine ─────────────────────

#[test]
fn epoch_info_dto_from_live_state() {
    use nexus_rpc::EpochInfoDto;

    let cfg = make_epoch_config(1000, 3600, 10);
    let mgr = EpochManager::new(cfg.clone(), EpochNumber(3));

    let tc = TestCommittee::new(7, EpochNumber(3));
    let engine = ConsensusEngine::new(tc.epoch, tc.committee);

    let dto = EpochInfoDto {
        epoch: mgr.current_epoch(),
        epoch_started_at: mgr.epoch_started_at(),
        committee_size: engine.committee().active_validators().len(),
        epoch_commits: engine.total_commits(),
        epoch_length_commits: cfg.epoch_length_commits,
        epoch_length_seconds: cfg.epoch_length_seconds,
    };

    assert_eq!(dto.epoch, EpochNumber(3));
    assert_eq!(dto.committee_size, 7);
    assert_eq!(dto.epoch_length_commits, 1000);
    assert_eq!(dto.epoch_length_seconds, 3600);
}
