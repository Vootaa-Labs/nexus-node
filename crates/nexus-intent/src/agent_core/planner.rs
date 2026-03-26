// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Simulate → Plan → Confirm → Execute binding logic.
//!
//! The planner bridges the gap between raw [`AgentEnvelope`] requests
//! and the intent compilation pipeline.  It enforces the plan-binding
//! invariant: once a `plan_hash` is produced by simulation, all
//! subsequent confirmation and execution steps must match that hash.
//!
//! # Phase 10 Status
//!
//! This file defines the canonical schema and trait boundary.
//! Full async implementation follows in T-10015.

use nexus_primitives::{Blake3Digest, TimestampMs};
use serde::{Deserialize, Serialize};

use crate::agent_core::envelope::AgentEnvelope;
use crate::agent_core::session::{AgentSession, SessionState};
use crate::error::{IntentError, IntentResult};

// ── Domain constants ────────────────────────────────────────────────────

/// Domain tag for plan digest computation.
pub const PLAN_DOMAIN: &[u8] = b"nexus::agent_core::plan::v1";

// ── SimulationResult ────────────────────────────────────────────────────

/// Result of a dry-run simulation (no state change).
///
/// Contains the plan hash that all subsequent steps must match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimulationResult {
    /// Identifier of the session that ran the simulation.
    pub session_id: Blake3Digest,
    /// Hash that binds simulation → confirmation → execution.
    pub plan_hash: Blake3Digest,
    /// Estimated gas across all plan steps.
    pub estimated_gas: u64,
    /// Number of intents in the plan.
    pub step_count: usize,
    /// Whether the plan requires cross-shard coordination.
    pub requires_cross_shard: bool,
    /// Timestamp of the simulation.
    pub simulated_at_ms: TimestampMs,
    /// Human-readable summary of the plan (for confirmation display).
    pub summary: String,
}

// ── ConfirmationRequest / ConfirmationResponse ──────────────────────────

/// Request for human or parent-agent confirmation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfirmationRequest {
    /// Session awaiting confirmation.
    pub session_id: Blake3Digest,
    /// Plan hash to confirm.
    pub plan_hash: Blake3Digest,
    /// Human-readable summary of the plan.
    pub summary: String,
}

/// Confirmation from a human or parent agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfirmationResponse {
    /// Session being confirmed.
    pub session_id: Blake3Digest,
    /// Plan hash confirmed.
    pub plan_hash: Blake3Digest,
    /// Unique confirmation reference (controls replay).
    pub confirmation_ref: Blake3Digest,
    /// Timestamp of confirmation.
    pub confirmed_at_ms: TimestampMs,
}

// ── ExecutionReceipt ────────────────────────────────────────────────────

/// Receipt returned after execution completes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionReceipt {
    /// Session that executed the plan.
    pub session_id: Blake3Digest,
    /// Plan hash that was executed.
    pub plan_hash: Blake3Digest,
    /// Transaction hashes produced by execution.
    pub tx_hashes: Vec<Blake3Digest>,
    /// Total gas consumed.
    pub gas_used: u64,
    /// Timestamp of completion.
    pub completed_at_ms: TimestampMs,
}

// ── Plan-hash computation ───────────────────────────────────────────────

/// Compute the plan hash given an envelope and simulation bytes.
///
/// `BLAKE3(PLAN_DOMAIN ‖ BCS(envelope_digest) ‖ simulation_data)`
pub fn compute_plan_hash(
    envelope_digest: &Blake3Digest,
    simulation_data: &[u8],
) -> IntentResult<Blake3Digest> {
    let env_bytes =
        bcs::to_bytes(envelope_digest).map_err(|e| IntentError::Codec(e.to_string()))?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(PLAN_DOMAIN);
    hasher.update(&env_bytes);
    hasher.update(simulation_data);
    let hash: [u8; 32] = *hasher.finalize().as_bytes();
    Ok(Blake3Digest(hash))
}

// ── Plan-binding validation ─────────────────────────────────────────────

/// Validate that a confirmation matches the session's bound plan.
///
/// # Errors
///
/// - `AgentCapabilityDenied` if no plan is bound or hashes mismatch.
pub fn validate_plan_binding(
    session: &AgentSession,
    supplied_plan_hash: &Blake3Digest,
) -> IntentResult<()> {
    match &session.plan_hash {
        None => Err(IntentError::AgentCapabilityDenied {
            reason: "session has no bound plan hash".into(),
        }),
        Some(bound) if bound != supplied_plan_hash => Err(IntentError::AgentCapabilityDenied {
            reason: format!(
                "plan hash mismatch: bound {:?}, supplied {:?}",
                bound, supplied_plan_hash
            ),
        }),
        Some(_) => Ok(()),
    }
}

/// Validate that a session is in the correct state to accept a
/// confirmation response.
pub fn validate_confirmation_state(session: &AgentSession) -> IntentResult<()> {
    if session.current_state != SessionState::AwaitingConfirmation {
        return Err(IntentError::AgentCapabilityDenied {
            reason: format!(
                "session not awaiting confirmation, current state: {:?}",
                session.current_state,
            ),
        });
    }
    Ok(())
}

// ── PlannerBackend trait ────────────────────────────────────────────────

/// Trait that concrete planner implementations must satisfy.
///
/// Provides the simulate → confirm → execute pipeline for agent requests.
pub trait PlannerBackend: Send + Sync {
    /// Simulate an envelope without modifying state.
    fn simulate(&self, envelope: &AgentEnvelope) -> IntentResult<SimulationResult>;

    /// Confirm a previously simulated plan.
    ///
    /// Validates plan binding and returns a `ConfirmationResponse`
    /// that the engine can store. Returns `None` if confirmation is
    /// not required (policy pre-approved).
    fn confirm(
        &self,
        session: &AgentSession,
        plan_hash: &Blake3Digest,
    ) -> IntentResult<ConfirmationResponse>;

    /// Execute a confirmed plan.
    fn execute(
        &self,
        session: &AgentSession,
        confirmation_ref: &Blake3Digest,
    ) -> IntentResult<ExecutionReceipt>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_primitives::{Blake3Digest, TimestampMs};

    fn make_session(plan: Option<Blake3Digest>, state: SessionState) -> AgentSession {
        AgentSession {
            session_id: Blake3Digest([0x01; 32]),
            created_at_ms: TimestampMs(1_000),
            replay_window_ms: 300_000,
            current_state: state,
            plan_hash: plan,
            confirmation_ref: None,
        }
    }

    #[test]
    fn plan_hash_deterministic() {
        let env_d = Blake3Digest([0xAA; 32]);
        let sim = b"simulation_data";
        let h1 = compute_plan_hash(&env_d, sim).unwrap();
        let h2 = compute_plan_hash(&env_d, sim).unwrap();
        assert_eq!(h1, h2);
    }

    #[test]
    fn plan_hash_varies_with_input() {
        let env_d = Blake3Digest([0xAA; 32]);
        let h1 = compute_plan_hash(&env_d, b"data_a").unwrap();
        let h2 = compute_plan_hash(&env_d, b"data_b").unwrap();
        assert_ne!(h1, h2);
    }

    #[test]
    fn validate_binding_ok() {
        let plan = Blake3Digest([0x05; 32]);
        let session = make_session(Some(plan), SessionState::Simulated);
        assert!(validate_plan_binding(&session, &plan).is_ok());
    }

    #[test]
    fn validate_binding_no_plan() {
        let session = make_session(None, SessionState::Received);
        assert!(validate_plan_binding(&session, &Blake3Digest([0x05; 32])).is_err());
    }

    #[test]
    fn validate_binding_mismatch() {
        let session = make_session(Some(Blake3Digest([0x01; 32])), SessionState::Simulated);
        assert!(validate_plan_binding(&session, &Blake3Digest([0x02; 32])).is_err());
    }

    #[test]
    fn confirmation_state_ok() {
        let session = make_session(
            Some(Blake3Digest([0x01; 32])),
            SessionState::AwaitingConfirmation,
        );
        assert!(validate_confirmation_state(&session).is_ok());
    }

    #[test]
    fn confirmation_state_wrong_state() {
        let session = make_session(Some(Blake3Digest([0x01; 32])), SessionState::Received);
        assert!(validate_confirmation_state(&session).is_err());
    }

    #[test]
    fn simulation_result_bcs_round_trip() {
        let sr = SimulationResult {
            session_id: Blake3Digest([0x01; 32]),
            plan_hash: Blake3Digest([0x02; 32]),
            estimated_gas: 42_000,
            step_count: 3,
            requires_cross_shard: true,
            simulated_at_ms: TimestampMs(1_700_000_000_000),
            summary: "Transfer 1000 NXS to 0xBB..BB".to_string(),
        };
        let bytes = bcs::to_bytes(&sr).unwrap();
        let decoded: SimulationResult = bcs::from_bytes(&bytes).unwrap();
        assert_eq!(sr, decoded);
    }

    #[test]
    fn execution_receipt_bcs_round_trip() {
        let receipt = ExecutionReceipt {
            session_id: Blake3Digest([0x01; 32]),
            plan_hash: Blake3Digest([0x02; 32]),
            tx_hashes: vec![Blake3Digest([0x03; 32])],
            gas_used: 10_000,
            completed_at_ms: TimestampMs(1_700_000_001_000),
        };
        let bytes = bcs::to_bytes(&receipt).unwrap();
        let decoded: ExecutionReceipt = bcs::from_bytes(&bytes).unwrap();
        assert_eq!(receipt, decoded);
    }
}
