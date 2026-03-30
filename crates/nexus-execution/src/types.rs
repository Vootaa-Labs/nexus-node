// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Execution layer data types.
//!
//! Core structures for transaction representation, execution results,
//! and receipts. Types here bridge the consensus layer (which delivers
//! ordered batches) and the Move VM execution engine.

use nexus_crypto::{DilithiumSignature, DilithiumVerifyKey};
use nexus_primitives::{
    AccountAddress, Amount, Blake3Digest, CommitSequence, ContractAddress, EpochNumber, ShardId,
    TimestampMs, TokenId, TxDigest,
};
use serde::{Deserialize, Serialize};

// ── Domain separation constants ─────────────────────────────────────────

/// Domain tag for transaction body hashing.
pub const TX_DOMAIN: &[u8] = b"nexus::execution::transaction::v1";

/// Maximum transaction payload size in bytes (256 KiB).
pub const MAX_TX_PAYLOAD_SIZE: usize = 256 * 1024;

/// Minimum gas limit for any transaction.
pub const MIN_GAS_LIMIT: u64 = 1_000;

// ── Transaction types ───────────────────────────────────────────────────

/// What the transaction asks the chain to do.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TransactionPayload {
    /// Simple token transfer.
    Transfer {
        /// Recipient account.
        recipient: AccountAddress,
        /// Transfer amount.
        amount: Amount,
        /// Which token to transfer.
        token: TokenId,
    },
    /// Call a published Move function.
    MoveCall {
        /// Target contract address.
        contract: ContractAddress,
        /// Fully qualified function name (e.g. `"transfer"`).
        function: String,
        /// Type arguments (serialised tags).
        type_args: Vec<Vec<u8>>,
        /// BCS-encoded call arguments.
        args: Vec<Vec<u8>>,
    },
    /// Publish new Move modules.
    MovePublish {
        /// Compiled Move bytecode modules.
        bytecode_modules: Vec<Vec<u8>>,
    },
    /// Upgrade previously published Move modules.
    MoveUpgrade {
        /// The contract address being upgraded.
        contract: ContractAddress,
        /// New compiled Move bytecode modules.
        bytecode_modules: Vec<Vec<u8>>,
    },
    /// Execute a one-off Move script (not published).
    MoveScript {
        /// Compiled script bytecode.
        bytecode: Vec<u8>,
        /// Type arguments (serialised tags).
        type_args: Vec<Vec<u8>>,
        /// BCS-encoded call arguments.
        args: Vec<Vec<u8>>,
    },
    /// Anchor a batch of provenance records on-chain.
    ///
    /// System transaction submitted by the local node's anchor batch task.
    /// The `anchor_digest` is written to chain state, making the provenance
    /// store tamper-evident under consensus.
    ProvenanceAnchor {
        /// BLAKE3 digest of the anchored provenance record IDs.
        anchor_digest: Blake3Digest,
        /// Monotonically increasing batch sequence number.
        batch_seq: u64,
        /// Number of provenance records in the batch.
        record_count: u32,
    },

    // ── Cross-shard HTLC (Phase V) ─────────────────────────────────
    /// Lock funds on the source shard for a cross-shard transfer.
    ///
    /// The sender's balance is debited and placed in a time-locked escrow.
    /// A corresponding `HtlcClaim` on the target shard releases the funds
    /// to the recipient.  If unclaimed within `timeout_epoch`, the sender
    /// may submit an `HtlcRefund` to reclaim the locked amount.
    HtlcLock {
        /// Recipient on the target shard.
        recipient: AccountAddress,
        /// Amount to lock.
        amount: Amount,
        /// Target shard where the recipient lives.
        target_shard: ShardId,
        /// BLAKE3 hash of the secret preimage — the hashlock.
        lock_hash: Blake3Digest,
        /// Epoch after which the sender may refund.
        timeout_epoch: EpochNumber,
    },
    /// Claim locked funds on the target shard.
    ///
    /// The claimant reveals the preimage whose BLAKE3 hash matches the
    /// `lock_hash` recorded in the corresponding `HtlcLock`.  On success
    /// the locked amount is credited to the recipient.
    HtlcClaim {
        /// Digest of the originating lock transaction.
        lock_digest: TxDigest,
        /// Secret preimage — `BLAKE3(preimage) == lock_hash`.
        preimage: Vec<u8>,
    },
    /// Refund expired HTLC funds back to the original sender.
    ///
    /// Valid only after the lock's `timeout_epoch` has passed and the
    /// lock has not already been claimed.
    HtlcRefund {
        /// Digest of the originating lock transaction.
        lock_digest: TxDigest,
    },
}

/// Unsigned transaction body — everything that gets signed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TransactionBody {
    /// Sender's account address.
    pub sender: AccountAddress,
    /// Monotonically increasing nonce for replay protection.
    pub sequence_number: u64,
    /// Transaction is invalid after this epoch.
    pub expiry_epoch: EpochNumber,
    /// Maximum gas the sender is willing to burn.
    pub gas_limit: u64,
    /// Price per gas unit (determines priority in the fee market).
    pub gas_price: u64,
    /// Explicit shard routing hint (None = auto-route).
    pub target_shard: Option<ShardId>,
    /// Action to execute.
    pub payload: TransactionPayload,
    /// Prevents cross-chain replay.
    pub chain_id: u64,
}

/// A fully signed transaction ready for consensus ingestion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedTransaction {
    /// The unsigned body that was signed.
    pub body: TransactionBody,
    /// ML-DSA (Dilithium3) signature over BCS(body).
    pub signature: DilithiumSignature,
    /// Sender's public key for verification.
    pub sender_pk: DilithiumVerifyKey,
    /// BLAKE3(TX_DOMAIN ‖ BCS(body)).
    pub digest: TxDigest,
}

// ── Execution result types ──────────────────────────────────────────────

/// Outcome status for a single transaction.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExecutionStatus {
    /// Transaction executed successfully.
    Success,
    /// Move program aborted with a location + error code.
    MoveAbort {
        /// Module::function that triggered the abort.
        location: String,
        /// Numeric abort code.
        code: u64,
    },
    /// Transaction ran out of gas before completing.
    OutOfGas,

    // ── Pre-execution validation rejections (Phase B) ───────────────
    /// Transaction signature is invalid or digest doesn't match body.
    InvalidSignature,
    /// `sender_pk` does not derive to `body.sender`.
    SenderMismatch,
    /// `body.sequence_number` does not match the on-chain nonce.
    SequenceNumberMismatch {
        /// Expected (on-chain) sequence number.
        expected: u64,
        /// Actual sequence number in the transaction.
        got: u64,
    },
    /// Transaction expired (`expiry_epoch` < current epoch).
    Expired,
    /// `body.chain_id` does not match the node's chain ID.
    ChainIdMismatch,

    // ── HTLC-specific statuses (Phase V) ────────────────────────────
    /// HTLC lock not found in state.
    HtlcLockNotFound,
    /// HTLC lock already claimed.
    HtlcAlreadyClaimed,
    /// HTLC lock already refunded.
    HtlcAlreadyRefunded,
    /// HTLC preimage does not match lock hash.
    HtlcPreimageMismatch,
    /// HTLC refund attempted before timeout epoch.
    HtlcRefundTooEarly,

    // ── Move VM failure (non-abort) ─────────────────────────────────
    /// Move VM error that is not a program abort (e.g. bytecode
    /// verification failure, contract not found, type mismatch).
    MoveVmError {
        /// Human-readable error description.
        reason: String,
    },
}

/// A single key/value state mutation produced by a transaction.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StateChange {
    /// The account whose state was modified.
    pub account: AccountAddress,
    /// Storage key (Move resource tag or raw key).
    pub key: Vec<u8>,
    /// New value, or `None` if the key was deleted.
    pub value: Option<Vec<u8>>,
}

/// Receipt for a single executed transaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionReceipt {
    /// Digest of the transaction that was executed.
    pub tx_digest: TxDigest,
    /// Global commit sequence assigned by Shoal++.
    pub commit_seq: CommitSequence,
    /// Shard on which execution occurred.
    pub shard_id: ShardId,
    /// Outcome status.
    pub status: ExecutionStatus,
    /// Gas actually consumed.
    pub gas_used: u64,
    /// State mutations produced.
    pub state_changes: Vec<StateChange>,
    /// Contract events emitted during execution.
    #[serde(default)]
    pub events: Vec<crate::move_adapter::events::ContractEvent>,
    /// Wall-clock time of execution.
    pub timestamp: TimestampMs,
}

/// Aggregate result of executing a batch of transactions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockExecutionResult {
    /// Per-batch state hash.
    ///
    /// For empty batches this is the canonical commitment empty-root.
    /// For non-empty batches this is a flat hash of state changes
    /// produced by the executor — **not** the authenticated commitment
    /// root.  The execution bridge replaces this value with the
    /// canonical commitment root derived from the Merkle tree after
    /// persisting state.
    pub new_state_root: Blake3Digest,
    /// Per-transaction receipts, one for each input transaction (in order).
    pub receipts: Vec<TransactionReceipt>,
    /// Total gas consumed across all transactions.
    pub gas_used_total: u64,
    /// Wall-clock execution time in milliseconds.
    pub execution_ms: u32,
}

// ── Cross-shard HTLC types ──────────────────────────────────────────────

/// Lifecycle status of an HTLC lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HtlcStatus {
    /// Funds are locked; awaiting claim or timeout.
    Pending,
    /// Recipient has claimed the funds with a valid preimage.
    Claimed,
    /// Lock expired and the sender has been refunded.
    Refunded,
}

/// Persistent record of an HTLC lock, stored under `cf_htlc_locks`.
///
/// Key: `lock_digest` (32 bytes) — the digest of the originating
/// `HtlcLock` transaction.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HtlcLockRecord {
    /// Digest of the HtlcLock transaction (also the storage key).
    pub lock_digest: TxDigest,
    /// Sender who locked the funds (on the source shard).
    pub sender: AccountAddress,
    /// Intended recipient (on the target shard).
    pub recipient: AccountAddress,
    /// Locked amount.
    pub amount: Amount,
    /// Source shard where the lock was executed and funds escrowed.
    pub source_shard: ShardId,
    /// Target shard where the claim should be executed.
    pub target_shard: ShardId,
    /// BLAKE3 hash of the secret preimage (hashlock).
    pub lock_hash: Blake3Digest,
    /// Epoch after which the sender may request a refund.
    pub timeout_epoch: EpochNumber,
    /// Current lifecycle status.
    pub status: HtlcStatus,
    /// Epoch at which the lock was created.
    pub created_epoch: EpochNumber,
}

/// Domain tag for HTLC lock-hash computation.
pub const HTLC_LOCK_DOMAIN: &[u8] = b"nexus::htlc::lock_hash::v1";

/// Compute the HTLC lock hash from a preimage.
///
/// `BLAKE3(HTLC_LOCK_DOMAIN ‖ preimage)`
pub fn compute_lock_hash(preimage: &[u8]) -> Blake3Digest {
    let mut hasher = blake3::Hasher::new();
    hasher.update(HTLC_LOCK_DOMAIN);
    hasher.update(preimage);
    let hash: [u8; 32] = *hasher.finalize().as_bytes();
    Blake3Digest(hash)
}

// ── Helpers ─────────────────────────────────────────────────────────────

/// Compute the canonical digest of a transaction body.
///
/// `BLAKE3(TX_DOMAIN ‖ BCS(body))`
///
/// # Errors
///
/// Returns [`ExecutionError::Codec`] if BCS serialization fails.
pub fn compute_tx_digest(body: &TransactionBody) -> crate::error::ExecutionResult<TxDigest> {
    let body_bytes =
        bcs::to_bytes(body).map_err(|e| crate::error::ExecutionError::Codec(e.to_string()))?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(TX_DOMAIN);
    hasher.update(&body_bytes);
    let hash: [u8; 32] = *hasher.finalize().as_bytes();
    Ok(Blake3Digest(hash))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_primitives::Blake3Digest;

    fn sample_body() -> TransactionBody {
        TransactionBody {
            sender: AccountAddress([0xAA; 32]),
            sequence_number: 1,
            expiry_epoch: EpochNumber(100),
            gas_limit: 50_000,
            gas_price: 10,
            target_shard: None,
            payload: TransactionPayload::Transfer {
                recipient: AccountAddress([0xBB; 32]),
                amount: Amount(1_000),
                token: TokenId::Native,
            },
            chain_id: 1,
        }
    }

    #[test]
    fn tx_digest_deterministic() {
        let body = sample_body();
        let d1 = compute_tx_digest(&body).unwrap();
        let d2 = compute_tx_digest(&body).unwrap();
        assert_eq!(d1, d2);
    }

    #[test]
    fn tx_digest_changes_with_nonce() {
        let mut body = sample_body();
        let d1 = compute_tx_digest(&body).unwrap();
        body.sequence_number = 2;
        let d2 = compute_tx_digest(&body).unwrap();
        assert_ne!(d1, d2);
    }

    #[test]
    fn tx_digest_changes_with_sender() {
        let mut body = sample_body();
        let d1 = compute_tx_digest(&body).unwrap();
        body.sender = AccountAddress([0xCC; 32]);
        let d2 = compute_tx_digest(&body).unwrap();
        assert_ne!(d1, d2);
    }

    #[test]
    fn tx_digest_changes_with_chain_id() {
        let mut body = sample_body();
        let d1 = compute_tx_digest(&body).unwrap();
        body.chain_id = 99;
        let d2 = compute_tx_digest(&body).unwrap();
        assert_ne!(d1, d2);
    }

    #[test]
    fn execution_status_success_eq() {
        assert_eq!(ExecutionStatus::Success, ExecutionStatus::Success);
    }

    #[test]
    fn execution_status_abort_eq() {
        let a = ExecutionStatus::MoveAbort {
            location: "0x1::coin".into(),
            code: 7,
        };
        let b = ExecutionStatus::MoveAbort {
            location: "0x1::coin".into(),
            code: 7,
        };
        assert_eq!(a, b);
    }

    #[test]
    fn state_change_delete() {
        let change = StateChange {
            account: AccountAddress([1u8; 32]),
            key: b"balance".to_vec(),
            value: None,
        };
        assert!(change.value.is_none());
    }

    #[test]
    fn transaction_payload_variants() {
        // Transfer
        let transfer = TransactionPayload::Transfer {
            recipient: AccountAddress([2u8; 32]),
            amount: Amount(500),
            token: TokenId::Native,
        };
        assert!(matches!(transfer, TransactionPayload::Transfer { .. }));

        // MoveCall
        let call = TransactionPayload::MoveCall {
            contract: ContractAddress([3u8; 32]),
            function: "do_something".into(),
            type_args: vec![],
            args: vec![vec![1, 2, 3]],
        };
        assert!(matches!(call, TransactionPayload::MoveCall { .. }));

        // MovePublish
        let publish = TransactionPayload::MovePublish {
            bytecode_modules: vec![vec![0xDE, 0xAD]],
        };
        assert!(matches!(publish, TransactionPayload::MovePublish { .. }));
    }

    #[test]
    fn signed_transaction_holds_digest() {
        use nexus_crypto::{DilithiumSigner, Signer};
        let body = sample_body();
        let digest = compute_tx_digest(&body).unwrap();
        let (sk, pk) = DilithiumSigner::generate_keypair();
        let sig = DilithiumSigner::sign(&sk, TX_DOMAIN, digest.as_bytes());
        let tx = SignedTransaction {
            body,
            signature: sig,
            sender_pk: pk,
            digest,
        };
        assert_eq!(tx.digest, digest);
    }

    #[test]
    fn block_execution_result_empty() {
        let result = BlockExecutionResult {
            new_state_root: Blake3Digest([0u8; 32]),
            receipts: vec![],
            gas_used_total: 0,
            execution_ms: 0,
        };
        assert!(result.receipts.is_empty());
        assert_eq!(result.gas_used_total, 0);
    }

    #[test]
    fn max_payload_constant() {
        assert_eq!(MAX_TX_PAYLOAD_SIZE, 262_144); // 256 * 1024
    }

    #[test]
    fn min_gas_constant() {
        assert_eq!(MIN_GAS_LIMIT, 1_000);
    }
}
