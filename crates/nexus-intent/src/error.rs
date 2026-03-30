// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Intent layer error types.
//!
//! [`IntentError`] is the unified error enum for the intent layer,
//! covering intent parsing, compilation, account resolution, routing,
//! AI agent protocol validation, and cross-shard coordination.

use nexus_primitives::{AccountAddress, ContractAddress, IntentId, ShardId, TokenId};
use thiserror::Error;

/// Unified error type for the intent layer.
#[derive(Debug, Error)]
pub enum IntentError {
    // ── Intent validation ───────────────────────────────────────────
    /// Intent signature verification failed.
    #[error("invalid intent signature for sender {sender}")]
    InvalidSignature {
        /// Sender whose signature failed verification.
        sender: AccountAddress,
        /// Underlying crypto error.
        #[source]
        source: nexus_crypto::NexusCryptoError,
    },

    /// Intent has expired.
    #[error("intent expired: deadline {deadline_ms} < current time {current_ms}")]
    IntentExpired {
        /// Intent deadline timestamp.
        deadline_ms: u64,
        /// Current timestamp.
        current_ms: u64,
    },

    /// Intent nonce is stale (replay protection).
    #[error("stale nonce for {sender}: expected >= {expected}, got {got}")]
    StaleNonce {
        /// Account whose nonce is stale.
        sender: AccountAddress,
        /// Minimum expected nonce.
        expected: u64,
        /// Nonce provided.
        got: u64,
    },

    /// Intent payload exceeds maximum allowed size.
    #[error("intent too large: {size} bytes (max {max})")]
    IntentTooLarge {
        /// Actual size in bytes.
        size: usize,
        /// Configured maximum.
        max: usize,
    },

    // ── Parsing ─────────────────────────────────────────────────────
    /// Failed to parse intent payload.
    #[error("intent parse error: {reason}")]
    ParseError {
        /// Human-readable parse failure reason.
        reason: String,
    },

    /// Malformed agent intent spec.
    #[error("agent spec error: {reason}")]
    AgentSpecError {
        /// Reason for spec validation failure.
        reason: String,
    },

    // ── Account resolution ──────────────────────────────────────────
    /// Account not found in any shard.
    #[error("account not found: {account}")]
    AccountNotFound {
        /// The account that could not be resolved.
        account: AccountAddress,
    },

    /// Insufficient balance for the requested operation.
    #[error("insufficient balance for {account}: need {required}, have {available} of {token:?}")]
    InsufficientBalance {
        /// Account with insufficient funds.
        account: AccountAddress,
        /// Token being spent.
        token: TokenId,
        /// Amount required.
        required: u64,
        /// Amount available.
        available: u64,
    },

    // ── Contract resolution ─────────────────────────────────────────
    /// Contract not found in the registry.
    #[error("contract not found: {contract}")]
    ContractNotFound {
        /// The contract that could not be resolved.
        contract: ContractAddress,
    },

    // ── Compilation / routing ───────────────────────────────────────
    /// Compiled intent plan exceeds maximum step count.
    #[error("too many steps: {steps} (max {max})")]
    TooManySteps {
        /// Number of steps in the plan.
        steps: usize,
        /// Configured maximum.
        max: usize,
    },

    /// No viable route found between shards.
    #[error("no route from shard {from} to shard {to}")]
    NoRoute {
        /// Source shard.
        from: ShardId,
        /// Destination shard.
        to: ShardId,
    },

    /// Gas estimate exceeds the sender's budget.
    #[error("gas budget exceeded: estimated {estimated}, budget {budget}")]
    GasBudgetExceeded {
        /// Estimated gas cost.
        estimated: u64,
        /// Sender's gas budget.
        budget: u64,
    },

    // ── Agent protocol ──────────────────────────────────────────────
    /// AI agent capability token invalid or revoked.
    #[error("agent capability denied: {reason}")]
    AgentCapabilityDenied {
        /// Reason for denial.
        reason: String,
    },

    /// Agent value limit exceeded.
    #[error("agent value limit exceeded: {value} > {limit}")]
    AgentValueLimitExceeded {
        /// Value requested.
        value: u64,
        /// Configured limit.
        limit: u64,
    },

    // ── Internal / propagation ──────────────────────────────────────
    /// BCS serialization / deserialization failure.
    #[error("codec error: {0}")]
    Codec(String),

    /// Execution layer error (propagated from nexus-execution).
    #[error("execution layer: {0}")]
    Execution(#[from] nexus_execution::error::ExecutionError),

    /// Intent compilation timed out.
    #[error("compile timeout after {timeout_ms} ms for intent {intent_id}")]
    CompileTimeout {
        /// The intent that timed out.
        intent_id: IntentId,
        /// Configured timeout in milliseconds.
        timeout_ms: u64,
    },

    /// Internal error (catch-all for unexpected failures).
    #[error("internal: {0}")]
    Internal(String),

    /// Persistent storage error (RocksDB or other backend).
    #[error("storage error: {0}")]
    Storage(String),
}

/// Convenience alias used throughout the intent layer.
pub type IntentResult<T> = Result<T, IntentError>;

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_primitives::{AccountAddress, ContractAddress, IntentId, ShardId, TokenId};

    #[test]
    fn intent_expired_display() {
        let e = IntentError::IntentExpired {
            deadline_ms: 5,
            current_ms: 10,
        };
        assert_eq!(
            e.to_string(),
            "intent expired: deadline 5 < current time 10"
        );
    }

    #[test]
    fn stale_nonce_display() {
        let sender = AccountAddress([0u8; 32]);
        let e = IntentError::StaleNonce {
            sender,
            expected: 5,
            got: 4,
        };
        assert_eq!(
            e.to_string(),
            format!("stale nonce for {sender}: expected >= 5, got 4")
        );
    }

    #[test]
    fn intent_too_large_display() {
        let e = IntentError::IntentTooLarge {
            size: 1000,
            max: 512,
        };
        assert_eq!(e.to_string(), "intent too large: 1000 bytes (max 512)");
    }

    #[test]
    fn parse_error_display() {
        let e = IntentError::ParseError {
            reason: "unexpected token".to_string(),
        };
        assert_eq!(e.to_string(), "intent parse error: unexpected token");
    }

    #[test]
    fn agent_spec_error_display() {
        let e = IntentError::AgentSpecError {
            reason: "missing field".to_string(),
        };
        assert_eq!(e.to_string(), "agent spec error: missing field");
    }

    #[test]
    fn account_not_found_display() {
        let account = AccountAddress([0xABu8; 32]);
        let e = IntentError::AccountNotFound { account };
        assert_eq!(e.to_string(), format!("account not found: {account}"));
    }

    #[test]
    fn insufficient_balance_display() {
        let account = AccountAddress([1u8; 32]);
        let e = IntentError::InsufficientBalance {
            account,
            token: TokenId::Native,
            required: 100,
            available: 50,
        };
        assert_eq!(
            e.to_string(),
            format!("insufficient balance for {account}: need 100, have 50 of Native")
        );
    }

    #[test]
    fn contract_not_found_display() {
        let contract = ContractAddress([0u8; 32]);
        let e = IntentError::ContractNotFound { contract };
        assert_eq!(e.to_string(), format!("contract not found: {contract}"));
    }

    #[test]
    fn too_many_steps_display() {
        let e = IntentError::TooManySteps { steps: 10, max: 5 };
        assert_eq!(e.to_string(), "too many steps: 10 (max 5)");
    }

    #[test]
    fn no_route_display() {
        let e = IntentError::NoRoute {
            from: ShardId(0),
            to: ShardId(1),
        };
        assert_eq!(e.to_string(), "no route from shard 0 to shard 1");
    }

    #[test]
    fn gas_budget_exceeded_display() {
        let e = IntentError::GasBudgetExceeded {
            estimated: 2_000,
            budget: 1_000,
        };
        assert_eq!(
            e.to_string(),
            "gas budget exceeded: estimated 2000, budget 1000"
        );
    }

    #[test]
    fn agent_capability_denied_display() {
        let e = IntentError::AgentCapabilityDenied {
            reason: "revoked".to_string(),
        };
        assert_eq!(e.to_string(), "agent capability denied: revoked");
    }

    #[test]
    fn agent_value_limit_exceeded_display() {
        let e = IntentError::AgentValueLimitExceeded {
            value: 100,
            limit: 50,
        };
        assert_eq!(e.to_string(), "agent value limit exceeded: 100 > 50");
    }

    #[test]
    fn codec_display() {
        let e = IntentError::Codec("bad bytes".to_string());
        assert_eq!(e.to_string(), "codec error: bad bytes");
    }

    #[test]
    fn compile_timeout_display() {
        let intent_id = IntentId::from_bytes([0u8; 32]);
        let e = IntentError::CompileTimeout {
            intent_id,
            timeout_ms: 100,
        };
        assert_eq!(
            e.to_string(),
            format!("compile timeout after 100 ms for intent {intent_id}")
        );
    }

    #[test]
    fn internal_display() {
        let e = IntentError::Internal("unexpected state".to_string());
        assert_eq!(e.to_string(), "internal: unexpected state");
    }

    #[test]
    fn storage_display() {
        let e = IntentError::Storage("disk full".to_string());
        assert_eq!(e.to_string(), "storage error: disk full");
    }
}
