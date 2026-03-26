//! Epoch lifecycle management.
//!
//! [`EpochManager`] tracks the current epoch, evaluates transition
//! conditions, and coordinates the creation of new consensus engine
//! states when the epoch advances.
//!
//! # Design
//!
//! The epoch manager does **not** own the consensus engine — the engine
//! is held behind `Arc<Mutex<ConsensusEngine>>` in the node layer.
//! Instead, this module provides pure decision logic:
//!
//! 1. `should_advance()` — evaluates whether the epoch boundary has
//!    been reached (commit count, wall-clock time, or explicit trigger).
//! 2. Epoch transition records are produced but **not** persisted here;
//!    the node layer is responsible for writing them to `cf_state` via
//!    the epoch store before applying the transition to the live engine.

#![forbid(unsafe_code)]

use crate::types::{EpochConfig, EpochTransition, EpochTransitionTrigger};
use nexus_primitives::{EpochNumber, TimestampMs};

// ── EpochManager ─────────────────────────────────────────────────────────────

/// Manages epoch lifecycle and transition conditions.
///
/// Created once at node startup. The manager holds epoch metadata and
/// the transition configuration but not the consensus engine itself.
#[derive(Debug, Clone)]
pub struct EpochManager {
    /// Configuration governing transition conditions.
    config: EpochConfig,
    /// The current epoch number (updated after each advance).
    current_epoch: EpochNumber,
    /// Wall-clock time at which the current epoch began.
    epoch_started_at: TimestampMs,
    /// History of completed transitions (in-memory; the node layer
    /// also persists these in `cf_state`).
    transitions: Vec<EpochTransition>,
}

impl EpochManager {
    /// Create a new manager starting at `epoch` with the given config.
    pub fn new(config: EpochConfig, epoch: EpochNumber) -> Self {
        Self {
            config,
            current_epoch: epoch,
            epoch_started_at: TimestampMs::now(),
            transitions: Vec::new(),
        }
    }

    /// Restore from persisted state (cold restart).
    ///
    /// `transitions` is the full audit trail loaded from storage.
    pub fn recover(
        config: EpochConfig,
        epoch: EpochNumber,
        epoch_started_at: TimestampMs,
        transitions: Vec<EpochTransition>,
    ) -> Self {
        Self {
            config,
            current_epoch: epoch,
            epoch_started_at,
            transitions,
        }
    }

    // ── Read-side API ───────────────────────────────────────────────

    /// The current epoch.
    pub fn current_epoch(&self) -> EpochNumber {
        self.current_epoch
    }

    /// Wall-clock time when the current epoch started.
    pub fn epoch_started_at(&self) -> TimestampMs {
        self.epoch_started_at
    }

    /// The full transition history.
    pub fn transitions(&self) -> &[EpochTransition] {
        &self.transitions
    }

    /// The epoch configuration.
    pub fn config(&self) -> &EpochConfig {
        &self.config
    }

    // ── Transition evaluation ───────────────────────────────────────

    /// Evaluate whether an epoch transition should occur.
    ///
    /// Returns `Some(trigger)` if a condition is met, `None` otherwise.
    /// The commit count comes from the consensus engine (passed in).
    pub fn should_advance(&self, commit_count: u64) -> Option<EpochTransitionTrigger> {
        // Minimum commits guard.
        if commit_count < self.config.min_epoch_commits {
            return None;
        }

        // Commit-based threshold.
        if self.config.epoch_length_commits > 0 && commit_count >= self.config.epoch_length_commits
        {
            return Some(EpochTransitionTrigger::CommitThreshold);
        }

        // Time-based threshold.
        if self.config.epoch_length_seconds > 0 {
            let now = TimestampMs::now();
            let elapsed_secs = now.0.saturating_sub(self.epoch_started_at.0) / 1000;
            if elapsed_secs >= self.config.epoch_length_seconds {
                return Some(EpochTransitionTrigger::TimeElapsed);
            }
        }

        None
    }

    /// Record a completed transition and advance the epoch counter.
    ///
    /// Call this **after** the node layer has:
    /// 1. Persisted the new committee to `cf_state`.
    /// 2. Called `ConsensusEngine::advance_epoch()`.
    pub fn record_transition(&mut self, transition: EpochTransition) {
        self.current_epoch = transition.to_epoch;
        self.epoch_started_at = transition.transitioned_at;
        self.transitions.push(transition);
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> EpochConfig {
        EpochConfig {
            epoch_length_commits: 100,
            epoch_length_seconds: 0, // disable time-based
            min_epoch_commits: 10,
        }
    }

    #[test]
    fn initial_epoch_is_correct() {
        let mgr = EpochManager::new(test_config(), EpochNumber(5));
        assert_eq!(mgr.current_epoch(), EpochNumber(5));
        assert!(mgr.transitions().is_empty());
    }

    #[test]
    fn should_not_advance_below_minimum() {
        let mgr = EpochManager::new(test_config(), EpochNumber(0));
        // 9 commits < min_epoch_commits (10)
        assert!(mgr.should_advance(9).is_none());
    }

    #[test]
    fn should_not_advance_below_threshold() {
        let mgr = EpochManager::new(test_config(), EpochNumber(0));
        // 50 commits >= min(10) but < threshold(100)
        assert!(mgr.should_advance(50).is_none());
    }

    #[test]
    fn should_advance_at_commit_threshold() {
        let mgr = EpochManager::new(test_config(), EpochNumber(0));
        let trigger = mgr.should_advance(100);
        assert_eq!(trigger, Some(EpochTransitionTrigger::CommitThreshold));
    }

    #[test]
    fn should_advance_above_commit_threshold() {
        let mgr = EpochManager::new(test_config(), EpochNumber(0));
        let trigger = mgr.should_advance(150);
        assert_eq!(trigger, Some(EpochTransitionTrigger::CommitThreshold));
    }

    #[test]
    fn record_transition_advances_epoch() {
        let mut mgr = EpochManager::new(test_config(), EpochNumber(0));
        let now = TimestampMs::now();

        mgr.record_transition(EpochTransition {
            from_epoch: EpochNumber(0),
            to_epoch: EpochNumber(1),
            trigger: EpochTransitionTrigger::CommitThreshold,
            final_commit_count: 100,
            transitioned_at: now,
        });

        assert_eq!(mgr.current_epoch(), EpochNumber(1));
        assert_eq!(mgr.transitions().len(), 1);
        assert_eq!(mgr.epoch_started_at(), now);
    }

    #[test]
    fn multiple_transitions_accumulate() {
        let mut mgr = EpochManager::new(test_config(), EpochNumber(0));
        let now = TimestampMs::now();

        for i in 0..3 {
            mgr.record_transition(EpochTransition {
                from_epoch: EpochNumber(i),
                to_epoch: EpochNumber(i + 1),
                trigger: EpochTransitionTrigger::CommitThreshold,
                final_commit_count: 100,
                transitioned_at: TimestampMs(now.0 + i * 1000),
            });
        }

        assert_eq!(mgr.current_epoch(), EpochNumber(3));
        assert_eq!(mgr.transitions().len(), 3);
    }

    #[test]
    fn recover_restores_state() {
        let now = TimestampMs::now();
        let transitions = vec![EpochTransition {
            from_epoch: EpochNumber(0),
            to_epoch: EpochNumber(1),
            trigger: EpochTransitionTrigger::Manual,
            final_commit_count: 50,
            transitioned_at: now,
        }];

        let mgr = EpochManager::recover(test_config(), EpochNumber(1), now, transitions.clone());

        assert_eq!(mgr.current_epoch(), EpochNumber(1));
        assert_eq!(mgr.transitions().len(), 1);
        assert_eq!(mgr.epoch_started_at(), now);
    }

    #[test]
    fn disabled_commit_threshold_never_triggers() {
        let config = EpochConfig {
            epoch_length_commits: 0,
            epoch_length_seconds: 0,
            min_epoch_commits: 0,
        };
        let mgr = EpochManager::new(config, EpochNumber(0));
        assert!(mgr.should_advance(1_000_000).is_none());
    }

    #[test]
    fn time_based_transition() {
        let config = EpochConfig {
            epoch_length_commits: 0,
            epoch_length_seconds: 1, // 1 second
            min_epoch_commits: 0,
        };
        // Epoch started 2 seconds ago.
        let mgr = EpochManager::recover(
            config,
            EpochNumber(0),
            TimestampMs(TimestampMs::now().0.saturating_sub(2000)),
            Vec::new(),
        );
        let trigger = mgr.should_advance(0);
        assert_eq!(trigger, Some(EpochTransitionTrigger::TimeElapsed));
    }
}
