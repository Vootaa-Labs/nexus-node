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
