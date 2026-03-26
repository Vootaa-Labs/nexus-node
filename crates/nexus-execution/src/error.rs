// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Execution error types.
//!
//! [`ExecutionError`] is the unified error enum for the execution layer,
//! covering transaction validation, Move VM failures, state access,
//! Block-STM conflicts, and cross-shard coordination.

use nexus_primitives::{AccountAddress, ContractAddress, EpochNumber, ShardId, TxDigest};
use thiserror::Error;

/// Unified error type for the execution layer.
#[derive(Debug, Error)]
pub enum ExecutionError {
    // ── Transaction validation ──────────────────────────────────────
    /// Transaction signature verification failed.
    #[error("invalid transaction signature for tx {tx_digest}")]
    InvalidSignature {
        /// Digest of the offending transaction.
        tx_digest: TxDigest,
        /// Underlying crypto error.
        #[source]
        source: nexus_crypto::NexusCryptoError,
    },

    /// Sequence number does not match the account's on-chain nonce.
    #[error("sequence number mismatch for {sender}: expected {expected}, got {got}")]
    SequenceNumberMismatch {
        /// Account whose nonce is wrong.
        sender: AccountAddress,
        /// The expected next sequence number.
        expected: u64,
        /// The sequence number in the transaction.
        got: u64,
    },

    /// Transaction expired (its `expiry_epoch` is in the past).
    #[error("transaction expired: expiry epoch {expiry} < current epoch {current}")]
    TransactionExpired {
        /// Epoch at which the transaction expires.
        expiry: EpochNumber,
        /// Current epoch.
        current: EpochNumber,
    },

    /// Transaction payload exceeds the maximum allowed size.
    #[error("payload too large: {size} bytes (max {max})")]
    PayloadTooLarge {
        /// Actual size in bytes.
        size: usize,
        /// Configured maximum size.
        max: usize,
    },

    /// Gas limit is below the minimum required for the transaction type.
    #[error("gas limit too low: {limit} (minimum {minimum})")]
    GasLimitTooLow {
        /// Gas limit specified by the user.
        limit: u64,
        /// Minimum gas required.
        minimum: u64,
    },

    /// Chain ID in the transaction does not match the node's chain.
    #[error("chain id mismatch: expected {expected}, got {got}")]
    ChainIdMismatch {
        /// Node's chain ID.
        expected: u64,
        /// Transaction's chain ID.
        got: u64,
    },

    // ── Move VM execution ───────────────────────────────────────────
    /// Move VM execution aborted with a location and status code.
    #[error("move abort at {location}: code {code}")]
    MoveAbort {
        /// Module or script location that triggered the abort.
        location: String,
        /// Numeric abort code from the Move program.
        code: u64,
    },

    /// Transaction ran out of gas during execution.
    #[error("out of gas: used {used} of {limit}")]
    OutOfGas {
        /// Gas consumed before the limit was hit.
        used: u64,
        /// Gas limit set for the transaction.
        limit: u64,
    },

    /// Contract address does not exist on chain.
    #[error("contract not found: {address}")]
    ContractNotFound {
        /// The missing contract's address.
        address: ContractAddress,
    },

    /// Bytecode verification failed (malformed or unsafe modules).
    #[error("bytecode verification failed: {reason}")]
    BytecodeVerification {
        /// Human-readable verification failure reason.
        reason: String,
    },

    /// Type arguments do not match the function's generic signature.
    #[error("type argument mismatch in {function}: {reason}")]
    TypeMismatch {
        /// Fully qualified function name.
        function: String,
        /// Description of the mismatch.
        reason: String,
    },

    // ── State access ────────────────────────────────────────────────
    /// Account does not exist.
    #[error("account not found: {address}")]
    AccountNotFound {
        /// The missing account's address.
        address: AccountAddress,
    },

    /// Account has insufficient funds for the operation.
    #[error("insufficient balance for {address}: need {required}, have {available}")]
    InsufficientBalance {
        /// Account address.
        address: AccountAddress,
        /// Required amount for the operation.
        required: u64,
        /// Actual available balance.
        available: u64,
    },

    /// State storage I/O error.
    #[error("state storage error: {0}")]
    Storage(String),

    // ── Block-STM ───────────────────────────────────────────────────
    /// Block-STM exceeded the maximum number of re-execution attempts
    /// for a transaction due to persistent read/write conflicts.
    #[error("block-stm: max retries ({retries}) exceeded for tx {tx_index}")]
    MaxRetriesExceeded {
        /// Index of the transaction within the block.
        tx_index: u32,
        /// Number of retries attempted.
        retries: u32,
    },

    /// Block-STM read-set validation detected an inconsistency
    /// during the sequential validation phase.
    #[error("block-stm: read-set validation failed for tx {tx_index}")]
    ReadSetValidationFailed {
        /// Index of the transaction whose read-set is inconsistent.
        tx_index: u32,
    },

    // ── Cross-shard (Phase 2+) ──────────────────────────────────────
    /// Cross-shard HTLC timed out.
    #[error("cross-shard timeout for shard {shard}")]
    CrossShardTimeout {
        /// Shard that failed to respond.
        shard: ShardId,
    },

    /// Target shard is unavailable.
    #[error("shard {shard} unavailable")]
    ShardUnavailable {
        /// The unavailable shard.
        shard: ShardId,
    },

    /// HTLC lock not found.
    #[error("HTLC lock not found: {lock_digest}")]
    HtlcLockNotFound {
        /// Digest of the missing lock.
        lock_digest: TxDigest,
    },

    /// HTLC lock has already been claimed.
    #[error("HTLC lock already claimed: {lock_digest}")]
    HtlcAlreadyClaimed {
        /// Digest of the claimed lock.
        lock_digest: TxDigest,
    },

    /// HTLC lock has already been refunded.
    #[error("HTLC lock already refunded: {lock_digest}")]
    HtlcAlreadyRefunded {
        /// Digest of the refunded lock.
        lock_digest: TxDigest,
    },

    /// HTLC preimage does not match the lock hash.
    #[error("HTLC preimage mismatch for lock {lock_digest}")]
    HtlcPreimageMismatch {
        /// Digest of the lock.
        lock_digest: TxDigest,
    },

    /// HTLC refund attempted before timeout epoch.
    #[error(
        "HTLC refund too early for lock {lock_digest}: timeout epoch {timeout}, current {current}"
    )]
    HtlcRefundTooEarly {
        /// Digest of the lock.
        lock_digest: TxDigest,
        /// Timeout epoch.
        timeout: EpochNumber,
        /// Current epoch.
        current: EpochNumber,
    },

    // ── Codec ───────────────────────────────────────────────────────
    /// BCS serialization / deserialization error.
    #[error("codec error: {0}")]
    Codec(String),
}

impl ExecutionError {
    /// Short category label for metrics counters.
    pub fn metric_label(&self) -> &'static str {
        match self {
            Self::InvalidSignature { .. } => "InvalidSignature",
            Self::SequenceNumberMismatch { .. } => "SequenceNumberMismatch",
            Self::TransactionExpired { .. } => "TransactionExpired",
            Self::PayloadTooLarge { .. } => "PayloadTooLarge",
            Self::GasLimitTooLow { .. } => "GasLimitTooLow",
            Self::ChainIdMismatch { .. } => "ChainIdMismatch",
            Self::MoveAbort { .. } => "MoveAbort",
            Self::OutOfGas { .. } => "OutOfGas",
            Self::ContractNotFound { .. } => "ContractNotFound",
            Self::BytecodeVerification { .. } => "BytecodeVerification",
            Self::TypeMismatch { .. } => "TypeMismatch",
            Self::AccountNotFound { .. } => "AccountNotFound",
            Self::InsufficientBalance { .. } => "InsufficientBalance",
            Self::Storage(_) => "Storage",
            Self::MaxRetriesExceeded { .. } => "MaxRetriesExceeded",
            Self::ReadSetValidationFailed { .. } => "ReadSetValidationFailed",
            Self::CrossShardTimeout { .. } => "CrossShardTimeout",
            Self::ShardUnavailable { .. } => "ShardUnavailable",
            Self::HtlcLockNotFound { .. } => "HtlcLockNotFound",
            Self::HtlcAlreadyClaimed { .. } => "HtlcAlreadyClaimed",
            Self::HtlcAlreadyRefunded { .. } => "HtlcAlreadyRefunded",
            Self::HtlcPreimageMismatch { .. } => "HtlcPreimageMismatch",
            Self::HtlcRefundTooEarly { .. } => "HtlcRefundTooEarly",
            Self::Codec(_) => "Codec",
        }
    }
}

/// Convenience alias: `Result<T, ExecutionError>`.
pub type ExecutionResult<T> = Result<T, ExecutionError>;

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_primitives::Blake3Digest;

    #[test]
    fn error_display_includes_context() {
        let err = ExecutionError::SequenceNumberMismatch {
            sender: AccountAddress([1u8; 32]),
            expected: 5,
            got: 3,
        };
        let msg = err.to_string();
        assert!(msg.contains("expected 5"));
        assert!(msg.contains("got 3"));
    }

    #[test]
    fn error_display_move_abort() {
        let err = ExecutionError::MoveAbort {
            location: "0x1::coin::transfer".into(),
            code: 42,
        };
        assert!(err.to_string().contains("code 42"));
    }

    #[test]
    fn error_display_out_of_gas() {
        let err = ExecutionError::OutOfGas {
            used: 999,
            limit: 1000,
        };
        let msg = err.to_string();
        assert!(msg.contains("999"));
        assert!(msg.contains("1000"));
    }

    #[test]
    fn error_display_max_retries() {
        let err = ExecutionError::MaxRetriesExceeded {
            tx_index: 7,
            retries: 10,
        };
        assert!(err.to_string().contains("tx 7"));
    }

    #[test]
    fn error_display_payload_too_large() {
        let err = ExecutionError::PayloadTooLarge {
            size: 2_000_000,
            max: 1_000_000,
        };
        let msg = err.to_string();
        assert!(msg.contains("2000000"));
        assert!(msg.contains("1000000"));
    }

    #[test]
    fn invalid_signature_source_chain() {
        let crypto_err = nexus_crypto::NexusCryptoError::VerificationFailed {
            reason: "test verification failure".into(),
        };
        let err = ExecutionError::InvalidSignature {
            tx_digest: Blake3Digest([0xab; 32]),
            source: crypto_err,
        };
        // std::error::Error::source() should chain through
        use std::error::Error;
        assert!(err.source().is_some());
    }

    #[test]
    fn result_alias_works() {
        let ok: ExecutionResult<u32> = Ok(42);
        assert!(ok.is_ok());

        let err: ExecutionResult<u32> = Err(ExecutionError::Storage("disk full".into()));
        assert!(err.is_err());
    }

    #[test]
    fn error_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ExecutionError>();
    }
}
