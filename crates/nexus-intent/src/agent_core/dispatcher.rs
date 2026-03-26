// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Request dispatcher — routes validated agent requests to backends.
//!
//! The dispatcher sits between the session/planner logic and the
//! underlying Intent Compiler, Execution Engine, and Storage.
//! It is responsible for selecting the correct backend based on
//! [`AgentRequestKind`].
//!
//! # Phase 10 Status
//!
//! This file defines the trait boundary and canonical outcome types.
//! The first concrete implementation ships in T-10016.

use nexus_primitives::Blake3Digest;
use serde::{Deserialize, Serialize};

use crate::agent_core::envelope::{AgentEnvelope, AgentRequestKind};
use crate::agent_core::planner::{ConfirmationResponse, ExecutionReceipt, SimulationResult};
use crate::error::{IntentError, IntentResult};

// ── DispatchOutcome ─────────────────────────────────────────────────────

/// Outcome of dispatching an agent request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DispatchOutcome {
    /// Simulation completed successfully.
    Simulated(SimulationResult),
    /// Plan confirmed by human / parent-agent.
    Confirmed(ConfirmationResponse),
    /// Execution completed successfully.
    Executed(ExecutionReceipt),
    /// Query result returned.
    QueryResult {
        /// BCS-encoded result payload.
        payload: Vec<u8>,
    },
    /// Request rejected before dispatch.
    Rejected {
        /// Reason for rejection.
        reason: String,
    },
}

// ── DispatchBackend trait ───────────────────────────────────────────────

/// Backend trait that concrete dispatchers must implement.
///
/// A dispatcher receives a fully validated [`AgentEnvelope`] (session
/// established, capabilities checked) and routes the request to the
/// appropriate processing pipeline.
pub trait DispatchBackend: Send + Sync {
    /// Dispatch a validated agent request.
    ///
    /// Implementations should match on `envelope.request_kind` and
    /// forward to the appropriate backend.
    fn dispatch(&self, envelope: &AgentEnvelope) -> IntentResult<DispatchOutcome>;
}

// ── Routing helpers ─────────────────────────────────────────────────────

/// Determine whether a request kind requires state mutation.
///
/// Read-only requests (simulation, queries) can be routed to replicas;
/// mutating requests (execution) must go to the primary.
pub fn is_mutating(kind: &AgentRequestKind) -> bool {
    matches!(
        kind,
        AgentRequestKind::ExecutePlan { .. } | AgentRequestKind::ConfirmPlan { .. }
    )
}

/// Validate that the envelope is well-formed before dispatch.
///
/// Checks:
/// - Request ID is non-zero.
/// - Session ID is non-zero.
/// - Deadline is in the future relative to `now_ms`.
pub fn pre_dispatch_validate(envelope: &AgentEnvelope, now_ms: u64) -> IntentResult<()> {
    // All-zero digest is used as a sentinel for "unset" — BLAKE3 output
    // will never produce this value in practice, so it safely identifies
    // missing or uninitialised identifiers.
    let zero = Blake3Digest([0u8; 32]);
    if envelope.request_id == zero {
        return Err(IntentError::AgentSpecError {
            reason: "request_id must not be zero".into(),
        });
    }
    if envelope.session_id == zero {
        return Err(IntentError::AgentSpecError {
            reason: "session_id must not be zero".into(),
        });
    }
    if envelope.deadline_ms.0 <= now_ms {
        return Err(IntentError::IntentExpired {
            deadline_ms: envelope.deadline_ms.0,
            current_ms: now_ms,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_core::envelope::{
        AgentEnvelope, AgentExecutionConstraints, AgentPrincipal, AgentRequestKind, ProtocolKind,
    };
    use nexus_primitives::{AccountAddress, Amount, Blake3Digest, TimestampMs};

    fn make_envelope(deadline: u64, request_kind: AgentRequestKind) -> AgentEnvelope {
        AgentEnvelope {
            protocol_kind: ProtocolKind::Mcp,
            protocol_version: "mcp/2025-11-05".to_string(),
            request_id: Blake3Digest([0x01; 32]),
            session_id: Blake3Digest([0x02; 32]),
            idempotency_key: Blake3Digest([0x03; 32]),
            caller: AgentPrincipal {
                address: AccountAddress([0xAA; 32]),
                display_name: None,
            },
            delegated_capability: None,
            request_kind,
            constraints: AgentExecutionConstraints {
                max_gas: 100_000,
                max_total_value: Amount(1_000_000),
                allowed_contracts: vec![],
            },
            deadline_ms: TimestampMs(deadline),
            parent_session_id: None,
        }
    }

    #[test]
    fn is_mutating_for_execute() {
        assert!(is_mutating(&AgentRequestKind::ExecutePlan {
            plan_hash: Blake3Digest([0x01; 32]),
            confirmation_ref: Blake3Digest([0x02; 32]),
        }));
    }

    #[test]
    fn not_mutating_for_simulate() {
        use crate::types::UserIntent;
        use nexus_primitives::TokenId;
        let intent = UserIntent::Transfer {
            to: AccountAddress([0xBB; 32]),
            token: TokenId::Native,
            amount: Amount(100),
        };
        assert!(!is_mutating(&AgentRequestKind::SimulateIntent { intent }));
    }

    #[test]
    fn pre_dispatch_ok() {
        let env = make_envelope(
            2_000_000_000_000,
            AgentRequestKind::IntentRequest {
                intent: crate::types::UserIntent::Transfer {
                    to: AccountAddress([0xBB; 32]),
                    token: nexus_primitives::TokenId::Native,
                    amount: Amount(100),
                },
            },
        );
        assert!(pre_dispatch_validate(&env, 1_000_000_000_000).is_ok());
    }

    #[test]
    fn pre_dispatch_expired() {
        let env = make_envelope(
            1_000,
            AgentRequestKind::IntentRequest {
                intent: crate::types::UserIntent::Transfer {
                    to: AccountAddress([0xBB; 32]),
                    token: nexus_primitives::TokenId::Native,
                    amount: Amount(100),
                },
            },
        );
        assert!(pre_dispatch_validate(&env, 2_000).is_err());
    }

    #[test]
    fn pre_dispatch_zero_request_id() {
        let mut env = make_envelope(
            2_000_000_000_000,
            AgentRequestKind::IntentRequest {
                intent: crate::types::UserIntent::Transfer {
                    to: AccountAddress([0xBB; 32]),
                    token: nexus_primitives::TokenId::Native,
                    amount: Amount(100),
                },
            },
        );
        env.request_id = Blake3Digest([0u8; 32]);
        assert!(pre_dispatch_validate(&env, 1_000).is_err());
    }

    #[test]
    fn pre_dispatch_zero_session_id() {
        let mut env = make_envelope(
            2_000_000_000_000,
            AgentRequestKind::IntentRequest {
                intent: crate::types::UserIntent::Transfer {
                    to: AccountAddress([0xBB; 32]),
                    token: nexus_primitives::TokenId::Native,
                    amount: Amount(100),
                },
            },
        );
        env.session_id = Blake3Digest([0u8; 32]);
        assert!(pre_dispatch_validate(&env, 1_000).is_err());
    }
}
