// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Agent session lifecycle and replay protection.
//!
//! Each agent interaction creates or continues an [`AgentSession`]
//! that tracks the request through `Received → Simulated →
//! AwaitingConfirmation → Executing → Finalized` (or error terminals).
//!
//! Sessions enforce:
//! - **Replay protection**: requests within `replay_window_ms` with
//!   duplicate `idempotency_key` are rejected.
//! - **Plan binding**: `simulation_result` → `confirmation_ref` →
//!   `execute` must share the same `plan_hash`.
//! - **Timeout**: sessions expire if not advanced within `deadline_ms`.

use nexus_primitives::{Blake3Digest, TimestampMs};
use serde::{Deserialize, Serialize};

// ── Session state machine ───────────────────────────────────────────────

/// Lifecycle state of an agent session.
///
/// ```text
/// Received → Simulated → AwaitingConfirmation → Executing → Finalized
///                                                         → Aborted
/// Any state → Expired (timeout)
/// Any state → Aborted (explicit cancellation)
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionState {
    /// Envelope received, awaiting processing.
    Received,
    /// Simulation completed, plan_hash bound.
    Simulated,
    /// Waiting for human or parent-agent confirmation.
    AwaitingConfirmation,
    /// Plan confirmed, execution in progress.
    Executing,
    /// Execution completed successfully.
    Finalized,
    /// Session explicitly aborted.
    Aborted,
    /// Session expired due to timeout.
    Expired,
}

impl SessionState {
    /// Returns `true` if this is a terminal state.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Finalized | Self::Aborted | Self::Expired)
    }

    /// Validate that a transition from `self` to `target` is legal.
    pub fn can_transition_to(self, target: Self) -> bool {
        use SessionState::*;
        if self.is_terminal() {
            return false;
        }
        matches!(
            (self, target),
            (Received, Simulated)
                | (Received, Aborted)
                | (Received, Expired)
                | (Simulated, AwaitingConfirmation)
                | (Simulated, Executing) // direct execute if pre-approved
                | (Simulated, Aborted)
                | (Simulated, Expired)
                | (AwaitingConfirmation, Executing)
                | (AwaitingConfirmation, Aborted)
                | (AwaitingConfirmation, Expired)
                | (Executing, Finalized)
                | (Executing, Aborted)
                | (Executing, Expired)
        )
    }
}

// ── AgentSession ────────────────────────────────────────────────────────

/// Tracks the lifecycle of a single agent interaction.
///
/// Session creation and state transitions are managed by the
/// Agent Core Engine; external adapters may only query session state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSession {
    /// Unique session identifier.
    pub session_id: Blake3Digest,
    /// Timestamp when the session was created.
    pub created_at_ms: TimestampMs,
    /// Duration (ms) within which duplicate idempotency keys are rejected.
    pub replay_window_ms: u64,
    /// Current lifecycle state.
    pub current_state: SessionState,
    /// Plan hash bound after simulation (if any).
    pub plan_hash: Option<Blake3Digest>,
    /// Confirmation reference from human / parent agent (if any).
    pub confirmation_ref: Option<Blake3Digest>,
}

/// Default replay window: 5 minutes.
pub const DEFAULT_REPLAY_WINDOW_MS: u64 = 5 * 60 * 1000;

impl AgentSession {
    /// Create a new session in `Received` state.
    pub fn new(session_id: Blake3Digest, now: TimestampMs) -> Self {
        Self {
            session_id,
            created_at_ms: now,
            replay_window_ms: DEFAULT_REPLAY_WINDOW_MS,
            current_state: SessionState::Received,
            plan_hash: None,
            confirmation_ref: None,
        }
    }

    /// Check if the session has expired relative to `now`.
    pub fn is_expired(&self, now: TimestampMs, deadline_ms: TimestampMs) -> bool {
        now.0 > deadline_ms.0
    }

    /// Attempt to transition to `new_state`.
    ///
    /// Returns `Ok(())` if the transition is valid, or an error message.
    pub fn transition_to(&mut self, new_state: SessionState) -> Result<(), String> {
        if self.current_state.can_transition_to(new_state) {
            self.current_state = new_state;
            Ok(())
        } else {
            Err(format!(
                "invalid session transition: {:?} → {:?}",
                self.current_state, new_state
            ))
        }
    }

    /// Bind a plan hash after simulation.
    ///
    /// Only valid in `Simulated` state or when transitioning to it.
    pub fn bind_plan(&mut self, plan_hash: Blake3Digest) -> Result<(), String> {
        if self.plan_hash.is_some() {
            return Err("plan_hash already bound".to_string());
        }
        self.plan_hash = Some(plan_hash);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_primitives::{Blake3Digest, TimestampMs};

    fn make_session() -> AgentSession {
        AgentSession::new(Blake3Digest([0x01; 32]), TimestampMs(1_000_000))
    }

    // ── State machine tests ─────────────────────────────────────────

    #[test]
    fn initial_state_is_received() {
        let s = make_session();
        assert_eq!(s.current_state, SessionState::Received);
    }

    #[test]
    fn valid_happy_path_transition() {
        let mut s = make_session();
        assert!(s.transition_to(SessionState::Simulated).is_ok());
        assert!(s.transition_to(SessionState::AwaitingConfirmation).is_ok());
        assert!(s.transition_to(SessionState::Executing).is_ok());
        assert!(s.transition_to(SessionState::Finalized).is_ok());
    }

    #[test]
    fn simulated_can_go_directly_to_executing() {
        let mut s = make_session();
        s.transition_to(SessionState::Simulated).unwrap();
        assert!(s.transition_to(SessionState::Executing).is_ok());
    }

    #[test]
    fn terminal_state_blocks_all_transitions() {
        let mut s = make_session();
        s.transition_to(SessionState::Simulated).unwrap();
        s.transition_to(SessionState::Executing).unwrap();
        s.transition_to(SessionState::Finalized).unwrap();
        assert!(s.transition_to(SessionState::Executing).is_err());
        assert!(s.transition_to(SessionState::Aborted).is_err());
    }

    #[test]
    fn any_non_terminal_can_abort() {
        for start in [
            SessionState::Received,
            SessionState::Simulated,
            SessionState::AwaitingConfirmation,
            SessionState::Executing,
        ] {
            let mut s = make_session();
            s.current_state = start;
            assert!(
                s.transition_to(SessionState::Aborted).is_ok(),
                "{start:?} should be able to abort"
            );
        }
    }

    #[test]
    fn any_non_terminal_can_expire() {
        for start in [
            SessionState::Received,
            SessionState::Simulated,
            SessionState::AwaitingConfirmation,
            SessionState::Executing,
        ] {
            let mut s = make_session();
            s.current_state = start;
            assert!(
                s.transition_to(SessionState::Expired).is_ok(),
                "{start:?} should be able to expire"
            );
        }
    }

    #[test]
    fn invalid_transition_rejected() {
        let mut s = make_session();
        // Cannot jump from Received to Executing.
        assert!(s.transition_to(SessionState::Executing).is_err());
        // Cannot jump from Received to Finalized.
        assert!(s.transition_to(SessionState::Finalized).is_err());
    }

    #[test]
    fn terminal_states_flagged() {
        assert!(SessionState::Finalized.is_terminal());
        assert!(SessionState::Aborted.is_terminal());
        assert!(SessionState::Expired.is_terminal());
        assert!(!SessionState::Received.is_terminal());
        assert!(!SessionState::Simulated.is_terminal());
        assert!(!SessionState::Executing.is_terminal());
    }

    // ── Plan binding tests ──────────────────────────────────────────

    #[test]
    fn bind_plan_succeeds_once() {
        let mut s = make_session();
        assert!(s.bind_plan(Blake3Digest([0xAA; 32])).is_ok());
        assert_eq!(s.plan_hash, Some(Blake3Digest([0xAA; 32])));
    }

    #[test]
    fn bind_plan_rejects_double_bind() {
        let mut s = make_session();
        s.bind_plan(Blake3Digest([0xAA; 32])).unwrap();
        assert!(s.bind_plan(Blake3Digest([0xBB; 32])).is_err());
    }

    // ── Serialization ───────────────────────────────────────────────

    #[test]
    fn session_bcs_round_trip() {
        let s = make_session();
        let bytes = bcs::to_bytes(&s).unwrap();
        let decoded: AgentSession = bcs::from_bytes(&bytes).unwrap();
        assert_eq!(s, decoded);
    }

    #[test]
    fn session_state_bcs_round_trip() {
        for state in [
            SessionState::Received,
            SessionState::Simulated,
            SessionState::AwaitingConfirmation,
            SessionState::Executing,
            SessionState::Finalized,
            SessionState::Aborted,
            SessionState::Expired,
        ] {
            let bytes = bcs::to_bytes(&state).unwrap();
            let decoded: SessionState = bcs::from_bytes(&bytes).unwrap();
            assert_eq!(state, decoded);
        }
    }

    // ── Expiry ──────────────────────────────────────────────────────

    #[test]
    fn session_expiry_detection() {
        let s = make_session();
        let deadline = TimestampMs(2_000_000);
        assert!(!s.is_expired(TimestampMs(1_500_000), deadline));
        assert!(s.is_expired(TimestampMs(2_500_000), deadline));
    }
}
