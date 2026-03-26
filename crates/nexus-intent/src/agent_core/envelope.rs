// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Canonical agent request envelope.
//!
//! [`AgentEnvelope`] is the single entry type for every agent request
//! regardless of originating protocol (MCP, A2A, REST).  It captures
//! caller identity, delegation, idempotency, constraints, and the
//! concrete request payload.

use nexus_primitives::{
    AccountAddress, Amount, Blake3Digest, ContractAddress, TimestampMs, TokenId,
};
use serde::{Deserialize, Serialize};

// ── Domain constants ────────────────────────────────────────────────────

/// Domain tag for envelope digest computation.
pub const ENVELOPE_DOMAIN: &[u8] = b"nexus::agent_core::envelope::v1";

// ── ProtocolKind ────────────────────────────────────────────────────────

/// Identifies which external protocol produced the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProtocolKind {
    /// Model Context Protocol adapter.
    Mcp,
    /// Agent-to-Agent protocol.
    A2a,
    /// REST tool / debug interface.
    RestTool,
}

// ── AgentPrincipal ──────────────────────────────────────────────────────

/// Identity of the calling agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentPrincipal {
    /// On-chain account address of the agent.
    pub address: AccountAddress,
    /// Human-readable display name (optional, not authoritative).
    pub display_name: Option<String>,
}

// ── CapabilityTokenId ───────────────────────────────────────────────────

/// Reference to a Move Capability Token authorising this request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityTokenId {
    /// Token identifier.
    pub token_id: TokenId,
    /// Account that owns the token.
    pub owner: AccountAddress,
}

// ── AgentExecutionConstraints ───────────────────────────────────────────

/// Static execution constraints attached to a request envelope.
///
/// These define upper bounds on what the request is allowed to do.
/// Runtime capability validation (delegation chains, dynamic balance)
/// is handled by [`super::capability_snapshot`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentExecutionConstraints {
    /// Maximum gas the request may consume.
    pub max_gas: u64,
    /// Maximum total value the request may transfer.
    pub max_total_value: Amount,
    /// Contracts the request may call (empty = all allowed).
    pub allowed_contracts: Vec<ContractAddress>,
}

// ── AgentRequestKind ────────────────────────────────────────────────────

/// Discriminated payload of an agent request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentRequestKind {
    /// Submit an intent for compilation and execution.
    IntentRequest {
        /// The user-level intent to process.
        intent: crate::types::UserIntent,
    },
    /// Simulate an intent (dry-run, no state change).
    SimulateIntent {
        /// The user-level intent to simulate.
        intent: crate::types::UserIntent,
    },
    /// Confirm a previously simulated plan (human / parent-agent approval).
    ConfirmPlan {
        /// Hash of the plan being confirmed.
        plan_hash: Blake3Digest,
    },
    /// Reject a previously simulated plan (human / parent-agent denial).
    RejectPlan {
        /// Hash of the plan being rejected.
        plan_hash: Blake3Digest,
        /// Optional reason for rejection.
        reason: Option<String>,
    },
    /// Execute a previously simulated and confirmed plan.
    ExecutePlan {
        /// Hash binding the simulation result to this execution.
        plan_hash: Blake3Digest,
        /// Confirmation reference from human / parent agent.
        confirmation_ref: Blake3Digest,
    },
    /// Query read-only on-chain data (balance, status, contract state).
    Query {
        /// Query description.
        query_kind: QueryKind,
    },
    /// Query provenance / audit trail.
    QueryProvenance {
        /// Provenance filter.
        filter: ProvenanceFilter,
    },
}

/// Read-only query variants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum QueryKind {
    /// Query account balance.
    Balance {
        /// Account to check.
        account: AccountAddress,
    },
    /// Query intent / transaction status.
    IntentStatus {
        /// Intent digest or tx hash.
        digest: Blake3Digest,
    },
    /// Query contract state.
    ContractState {
        /// Contract address.
        contract: ContractAddress,
        /// Resource tag or function name.
        resource: String,
    },
}

/// Filter for provenance queries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProvenanceFilter {
    /// By agent identity.
    ByAgent {
        /// Agent account address.
        agent_id: AccountAddress,
    },
    /// By session.
    BySession {
        /// Session identifier.
        session_id: Blake3Digest,
    },
    /// By capability token.
    ByCapability {
        /// Token identifier.
        token_id: TokenId,
    },
    /// By transaction hash.
    ByTransaction {
        /// Transaction digest.
        tx_hash: Blake3Digest,
    },
}

// ── AgentEnvelope ───────────────────────────────────────────────────────

/// Canonical agent request envelope.
///
/// Every agent request — regardless of external protocol — is re-packed
/// into this envelope before entering ACE processing.  The envelope
/// provides a single authoritative representation of caller identity,
/// capability delegation, replay protection, and execution constraints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentEnvelope {
    /// Which external protocol produced this request.
    pub protocol_kind: ProtocolKind,
    /// External protocol version string (e.g. `"mcp/2025-11-05"`).
    pub protocol_version: String,
    /// Unique request identifier (BLAKE3 digest).
    pub request_id: Blake3Digest,
    /// Session this request belongs to.
    pub session_id: Blake3Digest,
    /// Idempotency key for deduplication.
    pub idempotency_key: Blake3Digest,
    /// Caller agent identity.
    pub caller: AgentPrincipal,
    /// Optional delegated capability token.
    pub delegated_capability: Option<CapabilityTokenId>,
    /// Concrete request payload.
    pub request_kind: AgentRequestKind,
    /// Execution constraints.
    pub constraints: AgentExecutionConstraints,
    /// Deadline (ms since epoch) after which the request expires.
    pub deadline_ms: TimestampMs,
    /// Parent session for sub-agent delegation chains.
    pub parent_session_id: Option<Blake3Digest>,
}

// ── Digest computation ──────────────────────────────────────────────────

/// Compute the canonical BLAKE3 digest of an agent envelope.
///
/// `BLAKE3(ENVELOPE_DOMAIN ‖ BCS(envelope))`
///
/// # Errors
///
/// Returns an error if BCS serialization fails.
pub fn compute_envelope_digest(
    envelope: &AgentEnvelope,
) -> Result<Blake3Digest, crate::error::IntentError> {
    let bytes =
        bcs::to_bytes(envelope).map_err(|e| crate::error::IntentError::Codec(e.to_string()))?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(ENVELOPE_DOMAIN);
    hasher.update(&bytes);
    let hash: [u8; 32] = *hasher.finalize().as_bytes();
    Ok(Blake3Digest(hash))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_primitives::{AccountAddress, Amount, Blake3Digest, TimestampMs};

    fn sample_envelope() -> AgentEnvelope {
        AgentEnvelope {
            protocol_kind: ProtocolKind::Mcp,
            protocol_version: "mcp/2025-11-05".to_string(),
            request_id: Blake3Digest([0x01; 32]),
            session_id: Blake3Digest([0x02; 32]),
            idempotency_key: Blake3Digest([0x03; 32]),
            caller: AgentPrincipal {
                address: AccountAddress([0xAA; 32]),
                display_name: Some("test-agent".to_string()),
            },
            delegated_capability: None,
            request_kind: AgentRequestKind::Query {
                query_kind: QueryKind::Balance {
                    account: AccountAddress([0xBB; 32]),
                },
            },
            constraints: AgentExecutionConstraints {
                max_gas: 100_000,
                max_total_value: Amount(50_000),
                allowed_contracts: vec![],
            },
            deadline_ms: TimestampMs(1_700_000_000_000),
            parent_session_id: None,
        }
    }

    #[test]
    fn envelope_bcs_round_trip() {
        let env = sample_envelope();
        let bytes = bcs::to_bytes(&env).unwrap();
        let decoded: AgentEnvelope = bcs::from_bytes(&bytes).unwrap();
        assert_eq!(env, decoded);
    }

    #[test]
    fn envelope_digest_deterministic() {
        let env = sample_envelope();
        let d1 = compute_envelope_digest(&env).unwrap();
        let d2 = compute_envelope_digest(&env).unwrap();
        assert_eq!(d1, d2);
    }

    #[test]
    fn envelope_digest_changes_with_request() {
        let mut env = sample_envelope();
        let d1 = compute_envelope_digest(&env).unwrap();
        env.request_kind = AgentRequestKind::SimulateIntent {
            intent: crate::types::UserIntent::Transfer {
                to: AccountAddress([0xCC; 32]),
                token: nexus_primitives::TokenId::Native,
                amount: nexus_primitives::Amount(1000),
            },
        };
        let d2 = compute_envelope_digest(&env).unwrap();
        assert_ne!(d1, d2);
    }

    #[test]
    fn envelope_digest_changes_with_session() {
        let mut env = sample_envelope();
        let d1 = compute_envelope_digest(&env).unwrap();
        env.session_id = Blake3Digest([0xFF; 32]);
        let d2 = compute_envelope_digest(&env).unwrap();
        assert_ne!(d1, d2);
    }

    #[test]
    fn protocol_kind_variants() {
        for kind in [ProtocolKind::Mcp, ProtocolKind::A2a, ProtocolKind::RestTool] {
            let bytes = bcs::to_bytes(&kind).unwrap();
            let decoded: ProtocolKind = bcs::from_bytes(&bytes).unwrap();
            assert_eq!(kind, decoded);
        }
    }

    #[test]
    fn request_kind_intent_round_trip() {
        let rk = AgentRequestKind::IntentRequest {
            intent: crate::types::UserIntent::Transfer {
                to: AccountAddress([0xBB; 32]),
                token: nexus_primitives::TokenId::Native,
                amount: nexus_primitives::Amount(500),
            },
        };
        let bytes = bcs::to_bytes(&rk).unwrap();
        let decoded: AgentRequestKind = bcs::from_bytes(&bytes).unwrap();
        assert_eq!(rk, decoded);
    }

    #[test]
    fn request_kind_execute_plan_round_trip() {
        let rk = AgentRequestKind::ExecutePlan {
            plan_hash: Blake3Digest([0xDD; 32]),
            confirmation_ref: Blake3Digest([0xEE; 32]),
        };
        let bytes = bcs::to_bytes(&rk).unwrap();
        let decoded: AgentRequestKind = bcs::from_bytes(&bytes).unwrap();
        assert_eq!(rk, decoded);
    }
}
