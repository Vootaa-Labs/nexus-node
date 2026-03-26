//! Single-transaction execution logic for Block-STM.
//!
//! [`execute_single_tx`] runs one transaction against an [`MvOverlay`],
//! recording its read-set and write-set for later validation.
//!
//! [`validate_tx_preexec`] performs static pre-execution checks
//! (signature, sender-pk binding, digest, expiry, chain_id) that
//! do not require state access (SEC-H1, SEC-H3, SEC-H4).
//!
//! `MoveCall` and `MovePublish` payloads are delegated to the
//! [`MoveExecutor`] adapter, which encapsulates all Move VM logic
//! behind an isolation boundary (TLD-03 §5).

use std::collections::HashMap;

use parking_lot::Mutex;

use crate::error::ExecutionResult;
use crate::move_adapter::{contract_to_account, MoveExecutor, NexusStateView};
use crate::traits::StateView;
use crate::types::{
    compute_lock_hash, compute_tx_digest, ExecutionStatus, HtlcLockRecord, HtlcStatus,
    SignedTransaction, StateChange, TransactionPayload,
};
use nexus_crypto::{domains, DilithiumSigner, Signer};
use nexus_primitives::{AccountAddress, Amount, Blake3Digest, EpochNumber, ShardId};
use serde::{Deserialize, Serialize};

use super::mvhashmap::{MvOverlay, StateKey};

// ── Constants ───────────────────────────────────────────────────────────

/// Gas cost for a simple transfer (sourced from the default gas schedule).
pub(crate) const TRANSFER_GAS: u64 = 1_000;

/// Gas cost for a provenance anchor system transaction.
pub(crate) const ANCHOR_GAS: u64 = 500;

/// Gas cost for an HTLC lock operation.
pub(crate) const HTLC_LOCK_GAS: u64 = 2_000;

/// Gas cost for an HTLC claim operation.
pub(crate) const HTLC_CLAIM_GAS: u64 = 2_000;

/// Gas cost for an HTLC refund operation.
pub(crate) const HTLC_REFUND_GAS: u64 = 2_000;

/// System account used for HTLC escrow state.
const HTLC_SYSTEM_ACCOUNT: AccountAddress = AccountAddress([0x01; 32]);

// ── Anchor state entry ──────────────────────────────────────────────────

/// On-chain representation of a provenance anchor.
///
/// Written to `cf_state` under the system account `[0x00;32]` with key
/// `provenance_anchor_v1:{batch_seq}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AnchorStateEntry {
    /// BLAKE3 digest of the anchored provenance record IDs.
    pub anchor_digest: Blake3Digest,
    /// Batch sequence number.
    pub batch_seq: u64,
    /// Number of records in the batch.
    pub record_count: u32,
}

// ── Per-transaction execution record ────────────────────────────────────

/// The outcome of executing a single transaction (before validation).
#[derive(Debug, Clone)]
pub(crate) struct TxExecutionRecord {
    /// Keys that were read during execution and their observed values.
    pub read_set: HashMap<StateKey, Option<Vec<u8>>>,
    /// Keys that were written during execution and their new values.
    pub write_set: HashMap<StateKey, Option<Vec<u8>>>,
    /// Gas consumed.
    pub gas_used: u64,
    /// Execution outcome status.
    pub status: ExecutionStatus,
    /// State changes (ordered for determinism).
    pub state_changes: Vec<StateChange>,
}

// ── Overlay ↔ StateView bridge ──────────────────────────────────────────

/// Read-only state view over the [`MvOverlay`] for a specific transaction.
///
/// Bridges the Block-STM multi-version overlay to [`StateView`] so it
/// can be consumed by [`NexusStateView`] and the Move VM adapter.
/// All reads are tracked in an internal map for conflict detection.
struct OverlayStateView<'a> {
    overlay: &'a MvOverlay<'a>,
    tx_index: u32,
    /// Tracked reads: populated by each `get()` call.
    reads: Mutex<HashMap<StateKey, Option<Vec<u8>>>>,
}

impl<'a> OverlayStateView<'a> {
    fn new(overlay: &'a MvOverlay<'a>, tx_index: u32) -> Self {
        Self {
            overlay,
            tx_index,
            reads: Mutex::new(HashMap::new()),
        }
    }

    /// Consume self and return all tracked reads.
    fn into_reads(self) -> HashMap<StateKey, Option<Vec<u8>>> {
        self.reads.into_inner()
    }
}

impl StateView for OverlayStateView<'_> {
    fn get(&self, account: &AccountAddress, key: &[u8]) -> ExecutionResult<Option<Vec<u8>>> {
        let state_key = StateKey {
            account: *account,
            key: key.to_vec(),
        };
        let value = self.overlay.read(self.tx_index, &state_key)?;
        self.reads.lock().insert(state_key, value.clone());
        Ok(value)
    }
}

// ── State key for sequence numbers ──────────────────────────────────────

/// Well-known key under which per-account sequence numbers are stored.
pub(crate) const SEQUENCE_NUMBER_KEY: &[u8] = b"sequence_number";

// ── Pre-execution validation (SEC-H1, SEC-H3, SEC-H4) ──────────────────

/// Static pre-execution validation that does **not** require state access.
///
/// Checks:
/// 1. Digest integrity — `tx.digest == BLAKE3(TX_DOMAIN || BCS(body))`
/// 2. Signature — ML-DSA verification of `(sender_pk, signature)` over the digest
/// 3. Sender binding — `sender == BLAKE3(ACCOUNT_DOMAIN || sender_pk_bytes)`
/// 4. Expiry — `expiry_epoch >= current_epoch`
/// 5. Chain ID — `body.chain_id == expected`
///
/// Returns `None` if all checks pass, or `Some(TxExecutionRecord)` with
/// the appropriate rejection status if any check fails.
pub(crate) fn validate_tx_preexec(
    tx: &SignedTransaction,
    current_epoch: EpochNumber,
    chain_id: u64,
) -> Option<TxExecutionRecord> {
    // 1. Digest integrity.
    let expected_digest = match compute_tx_digest(&tx.body) {
        Ok(d) => d,
        Err(_) => return Some(rejection_record(ExecutionStatus::InvalidSignature)),
    };
    if expected_digest != tx.digest {
        return Some(rejection_record(ExecutionStatus::InvalidSignature));
    }

    // 2. Signature verification.
    if DilithiumSigner::verify(
        &tx.sender_pk,
        domains::USER_TX,
        tx.digest.as_bytes(),
        &tx.signature,
    )
    .is_err()
    {
        return Some(rejection_record(ExecutionStatus::InvalidSignature));
    }

    // 3. Sender ↔ public key binding.
    let derived_sender = AccountAddress::from_dilithium_pubkey(tx.sender_pk.as_bytes());
    if derived_sender != tx.body.sender {
        return Some(rejection_record(ExecutionStatus::SenderMismatch));
    }

    // 4. Expiry epoch.
    if tx.body.expiry_epoch < current_epoch {
        return Some(rejection_record(ExecutionStatus::Expired));
    }

    // 5. Chain ID.
    if tx.body.chain_id != chain_id {
        return Some(rejection_record(ExecutionStatus::ChainIdMismatch));
    }

    None // All static checks passed.
}

/// Create a rejection record with zero gas and no state changes.
fn rejection_record(status: ExecutionStatus) -> TxExecutionRecord {
    TxExecutionRecord {
        read_set: HashMap::new(),
        write_set: HashMap::new(),
        gas_used: 0,
        status,
        state_changes: Vec::new(),
    }
}

// ── Transaction executor (single tx) ────────────────────────────────────

/// Execute a single transaction against the overlay, recording read/write sets.
///
/// - `Transfer`: reads sender balance, writes sender + recipient balances
/// - `MoveCall`: delegates to [`MoveExecutor::execute_function`]
/// - `MovePublish`: delegates to [`MoveExecutor::publish_modules`]
pub(crate) fn execute_single_tx(
    tx: &SignedTransaction,
    tx_index: u32,
    overlay: &MvOverlay<'_>,
    move_executor: &MoveExecutor,
) -> ExecutionResult<TxExecutionRecord> {
    let mut read_set = HashMap::new();
    let mut write_set = HashMap::new();
    let mut state_changes = Vec::new();

    let sender = tx.body.sender;

    // ── B-2: Sequence number / nonce validation (SEC-H2) ────────────
    //
    // Read the on-chain sequence number for the sender and reject the
    // transaction if it doesn't match.  On success, the incremented
    // nonce is written to the write-set so that subsequent transactions
    // from the same sender (validated in Phase 2 order) see the update.
    let seq_key = StateKey {
        account: sender,
        key: SEQUENCE_NUMBER_KEY.to_vec(),
    };
    let seq_raw = overlay.read(tx_index, &seq_key)?;
    read_set.insert(seq_key.clone(), seq_raw.clone());

    let on_chain_seq = seq_raw
        .as_ref()
        .and_then(|b| b.as_slice().try_into().ok())
        .map(u64::from_le_bytes)
        .unwrap_or(0);

    if tx.body.sequence_number != on_chain_seq {
        return Ok(TxExecutionRecord {
            read_set,
            write_set,
            gas_used: 0,
            status: ExecutionStatus::SequenceNumberMismatch {
                expected: on_chain_seq,
                got: tx.body.sequence_number,
            },
            state_changes,
        });
    }

    // Write the incremented sequence number.
    let new_seq = on_chain_seq.wrapping_add(1);
    let new_seq_bytes = new_seq.to_le_bytes().to_vec();
    write_set.insert(seq_key.clone(), Some(new_seq_bytes.clone()));
    state_changes.push(StateChange {
        account: sender,
        key: SEQUENCE_NUMBER_KEY.to_vec(),
        value: Some(new_seq_bytes),
    });

    // ── Payload execution ───────────────────────────────────────────
    let balance_key = b"balance".to_vec();
    let sender_key = StateKey {
        account: sender,
        key: balance_key.clone(),
    };

    match &tx.body.payload {
        TransactionPayload::Transfer {
            recipient, amount, ..
        } => {
            // Read sender balance.
            let sender_balance_raw = overlay.read(tx_index, &sender_key)?;
            read_set.insert(sender_key.clone(), sender_balance_raw.clone());

            let sender_balance = parse_balance(&sender_balance_raw);

            // Check sufficient funds.
            let total_cost = amount
                .0
                .saturating_add(tx.body.gas_limit.saturating_mul(tx.body.gas_price));
            if sender_balance < total_cost {
                return Ok(TxExecutionRecord {
                    read_set,
                    write_set,
                    gas_used: TRANSFER_GAS,
                    status: ExecutionStatus::MoveAbort {
                        location: "nexus::transfer".into(),
                        code: 1, // INSUFFICIENT_BALANCE
                    },
                    state_changes,
                });
            }

            // Read recipient balance.
            let recipient_key = StateKey {
                account: *recipient,
                key: balance_key.clone(),
            };
            let recipient_balance_raw = overlay.read(tx_index, &recipient_key)?;
            read_set.insert(recipient_key.clone(), recipient_balance_raw.clone());
            let recipient_balance = parse_balance(&recipient_balance_raw);

            // Write new balances.
            let new_sender = sender_balance.saturating_sub(total_cost);
            let new_recipient = recipient_balance.saturating_add(amount.0);
            let sender_bytes = new_sender.to_le_bytes().to_vec();
            let recipient_bytes = new_recipient.to_le_bytes().to_vec();

            write_set.insert(sender_key.clone(), Some(sender_bytes.clone()));
            write_set.insert(recipient_key.clone(), Some(recipient_bytes.clone()));

            state_changes.push(StateChange {
                account: sender,
                key: balance_key.clone(),
                value: Some(sender_bytes),
            });
            state_changes.push(StateChange {
                account: *recipient,
                key: balance_key,
                value: Some(recipient_bytes),
            });

            Ok(TxExecutionRecord {
                read_set,
                write_set,
                gas_used: TRANSFER_GAS,
                status: ExecutionStatus::Success,
                state_changes,
            })
        }
        TransactionPayload::MoveCall {
            contract,
            function,
            type_args,
            args,
        } => {
            // Delegate to the Move VM adapter via OverlayStateView bridge.
            let view = OverlayStateView::new(overlay, tx_index);
            let output = {
                let nexus_view = NexusStateView::new(&view);
                let contract_addr = contract_to_account(contract);
                move_executor.execute_function(
                    &nexus_view,
                    sender,
                    contract_addr,
                    function,
                    type_args,
                    args,
                    tx.body.gas_limit,
                )
            };
            let output = output?;

            // Merge tracked overlay reads into the read-set.
            read_set.extend(view.into_reads());

            // Convert VM write-set to StateKey-based write-set.
            for ((acct, key), value) in &output.write_set {
                write_set.insert(
                    StateKey {
                        account: *acct,
                        key: key.clone(),
                    },
                    value.clone(),
                );
            }

            Ok(TxExecutionRecord {
                read_set,
                write_set,
                gas_used: output.gas_used,
                status: output.status,
                state_changes: output.state_changes,
            })
        }
        TransactionPayload::MovePublish {
            bytecode_modules, ..
        } => {
            // Delegate to the Move VM adapter via OverlayStateView bridge.
            let view = OverlayStateView::new(overlay, tx_index);
            let output = {
                let nexus_view = NexusStateView::new(&view);
                move_executor.publish_modules(
                    &nexus_view,
                    sender,
                    bytecode_modules,
                    tx.body.gas_limit,
                )
            };
            let output = output?;

            // Merge tracked overlay reads into the read-set.
            read_set.extend(view.into_reads());

            // Convert VM write-set to StateKey-based write-set.
            for ((acct, key), value) in &output.write_set {
                write_set.insert(
                    StateKey {
                        account: *acct,
                        key: key.clone(),
                    },
                    value.clone(),
                );
            }

            Ok(TxExecutionRecord {
                read_set,
                write_set,
                gas_used: output.gas_used,
                status: output.status,
                state_changes: output.state_changes,
            })
        }

        TransactionPayload::MoveUpgrade {
            contract,
            bytecode_modules,
        } => {
            // Upgrade re-publishes modules under the existing contract address.
            let view = OverlayStateView::new(overlay, tx_index);
            let output = {
                let nexus_view = NexusStateView::new(&view);
                move_executor.publish_modules(
                    &nexus_view,
                    contract_to_account(contract),
                    bytecode_modules,
                    tx.body.gas_limit,
                )
            };
            let output = output?;
            read_set.extend(view.into_reads());
            for ((acct, key), value) in &output.write_set {
                write_set.insert(
                    StateKey {
                        account: *acct,
                        key: key.clone(),
                    },
                    value.clone(),
                );
            }
            Ok(TxExecutionRecord {
                read_set,
                write_set,
                gas_used: output.gas_used,
                status: output.status,
                state_changes: output.state_changes,
            })
        }

        TransactionPayload::MoveScript {
            bytecode,
            type_args,
            args,
        } => {
            // Script execution — treated like a MoveCall with inline bytecode.
            let view = OverlayStateView::new(overlay, tx_index);
            let output = {
                let nexus_view = NexusStateView::new(&view);
                move_executor.execute_script(
                    &nexus_view,
                    sender,
                    bytecode,
                    type_args,
                    args,
                    tx.body.gas_limit,
                )
            };
            let output = output?;
            read_set.extend(view.into_reads());
            for ((acct, key), value) in &output.write_set {
                write_set.insert(
                    StateKey {
                        account: *acct,
                        key: key.clone(),
                    },
                    value.clone(),
                );
            }
            Ok(TxExecutionRecord {
                read_set,
                write_set,
                gas_used: output.gas_used,
                status: output.status,
                state_changes: output.state_changes,
            })
        }

        TransactionPayload::ProvenanceAnchor {
            anchor_digest,
            batch_seq,
            record_count,
        } => {
            // System transaction: write the anchor digest to chain state under
            // a well-known key so any node can verify the anchor on-chain.
            //
            // State key: system account [0x00;32] + "provenance_anchor_v1:{batch_seq}"
            // Value: BCS(AnchorStateEntry { anchor_digest, batch_seq, record_count })
            let system_account = AccountAddress([0u8; 32]);
            let state_key_bytes = format!("provenance_anchor_v1:{batch_seq}").into_bytes();
            let entry = AnchorStateEntry {
                anchor_digest: *anchor_digest,
                batch_seq: *batch_seq,
                record_count: *record_count,
            };
            let value = bcs::to_bytes(&entry).map_err(|e| {
                crate::error::ExecutionError::Codec(format!(
                    "anchor state entry serialization: {e}"
                ))
            })?;

            let anchor_key = StateKey {
                account: system_account,
                key: state_key_bytes.clone(),
            };
            write_set.insert(anchor_key, Some(value.clone()));

            state_changes.push(StateChange {
                account: system_account,
                key: state_key_bytes,
                value: Some(value),
            });

            Ok(TxExecutionRecord {
                read_set,
                write_set,
                gas_used: ANCHOR_GAS,
                status: ExecutionStatus::Success,
                state_changes,
            })
        }

        // ── HTLC Lock (source shard) ───────────────────────────────
        TransactionPayload::HtlcLock {
            recipient,
            amount,
            target_shard,
            lock_hash,
            timeout_epoch,
        } => {
            let ctx = HtlcExecContext {
                tx_index,
                overlay,
                read_set,
                write_set,
                state_changes,
            };
            execute_htlc_lock(
                tx,
                ctx,
                *recipient,
                *amount,
                *target_shard,
                *lock_hash,
                *timeout_epoch,
            )
        }

        // ── HTLC Claim (target shard) ──────────────────────────────
        TransactionPayload::HtlcClaim {
            lock_digest,
            preimage,
        } => {
            let ctx = HtlcExecContext {
                tx_index,
                overlay,
                read_set,
                write_set,
                state_changes,
            };
            execute_htlc_claim(ctx, *lock_digest, preimage)
        }

        // ── HTLC Refund (source shard, after timeout) ──────────────
        TransactionPayload::HtlcRefund { lock_digest } => {
            let ctx = HtlcExecContext {
                tx_index,
                overlay,
                read_set,
                write_set,
                state_changes,
            };
            execute_htlc_refund(ctx, sender, *lock_digest)
        }
    }
}

// ── HTLC helpers ────────────────────────────────────────────────────────

/// Mutable execution state passed to HTLC helper functions.
///
/// Groups the per-transaction overlay position and accumulation buffers
/// that were previously 5 positional parameters (D-1 convergence).
struct HtlcExecContext<'a> {
    tx_index: u32,
    overlay: &'a MvOverlay<'a>,
    read_set: HashMap<StateKey, Option<Vec<u8>>>,
    write_set: HashMap<StateKey, Option<Vec<u8>>>,
    state_changes: Vec<StateChange>,
}

/// State key for an HTLC lock record under the HTLC system account.
fn htlc_state_key(lock_digest: &Blake3Digest) -> StateKey {
    let mut key = b"htlc_lock_v1:".to_vec();
    key.extend_from_slice(lock_digest.as_bytes());
    StateKey {
        account: HTLC_SYSTEM_ACCOUNT,
        key,
    }
}

/// Execute an HtlcLock: debit sender, write lock record to state.
fn execute_htlc_lock(
    tx: &SignedTransaction,
    mut ctx: HtlcExecContext<'_>,
    recipient: AccountAddress,
    amount: Amount,
    target_shard: ShardId,
    lock_hash: Blake3Digest,
    timeout_epoch: EpochNumber,
) -> ExecutionResult<TxExecutionRecord> {
    let sender = tx.body.sender;
    let balance_key = b"balance".to_vec();
    let sender_key = StateKey {
        account: sender,
        key: balance_key.clone(),
    };

    // Read sender balance.
    let sender_balance_raw = ctx.overlay.read(ctx.tx_index, &sender_key)?;
    ctx.read_set
        .insert(sender_key.clone(), sender_balance_raw.clone());
    let sender_balance = parse_balance(&sender_balance_raw);

    // Check sufficient funds (amount + gas).
    let total_cost = amount
        .0
        .saturating_add(tx.body.gas_limit.saturating_mul(tx.body.gas_price));
    if sender_balance < total_cost {
        return Ok(TxExecutionRecord {
            read_set: ctx.read_set,
            write_set: ctx.write_set,
            gas_used: HTLC_LOCK_GAS,
            status: ExecutionStatus::MoveAbort {
                location: "nexus::htlc_lock".into(),
                code: 1, // INSUFFICIENT_BALANCE
            },
            state_changes: ctx.state_changes,
        });
    }

    // Debit sender.
    let new_sender_balance = sender_balance.saturating_sub(total_cost);
    let sender_bytes = new_sender_balance.to_le_bytes().to_vec();
    ctx.write_set
        .insert(sender_key.clone(), Some(sender_bytes.clone()));
    ctx.state_changes.push(StateChange {
        account: sender,
        key: balance_key,
        value: Some(sender_bytes),
    });

    // Compute the lock digest (this is the tx digest of the lock tx).
    let lock_digest = tx.digest;

    // Build the lock record.
    // source_shard is derived from the tx's target_shard (the lock tx
    // executes on the source shard, which is where the sender's funds live).
    let source_shard = tx.body.target_shard.unwrap_or(ShardId(0));
    let record = HtlcLockRecord {
        lock_digest,
        sender,
        recipient,
        amount,
        source_shard,
        target_shard,
        lock_hash,
        timeout_epoch,
        status: HtlcStatus::Pending,
        created_epoch: tx.body.expiry_epoch, // approximation; actual epoch set by bridge
    };

    let record_bytes = bcs::to_bytes(&record).map_err(|e| {
        crate::error::ExecutionError::Codec(format!("htlc lock record serialization: {e}"))
    })?;

    let htlc_key = htlc_state_key(&lock_digest);
    ctx.write_set
        .insert(htlc_key.clone(), Some(record_bytes.clone()));
    ctx.state_changes.push(StateChange {
        account: HTLC_SYSTEM_ACCOUNT,
        key: htlc_key.key,
        value: Some(record_bytes),
    });

    Ok(TxExecutionRecord {
        read_set: ctx.read_set,
        write_set: ctx.write_set,
        gas_used: HTLC_LOCK_GAS,
        status: ExecutionStatus::Success,
        state_changes: ctx.state_changes,
    })
}

/// Execute an HtlcClaim: verify preimage, credit recipient, mark lock claimed.
fn execute_htlc_claim(
    mut ctx: HtlcExecContext<'_>,
    lock_digest: Blake3Digest,
    preimage: &[u8],
) -> ExecutionResult<TxExecutionRecord> {
    // Read the lock record.
    let htlc_key = htlc_state_key(&lock_digest);
    let lock_raw = ctx.overlay.read(ctx.tx_index, &htlc_key)?;
    ctx.read_set.insert(htlc_key.clone(), lock_raw.clone());

    let lock_bytes = match lock_raw {
        Some(b) => b,
        None => {
            return Ok(TxExecutionRecord {
                read_set: ctx.read_set,
                write_set: ctx.write_set,
                gas_used: HTLC_CLAIM_GAS,
                status: ExecutionStatus::HtlcLockNotFound,
                state_changes: ctx.state_changes,
            });
        }
    };

    let mut record: HtlcLockRecord = bcs::from_bytes(&lock_bytes).map_err(|e| {
        crate::error::ExecutionError::Codec(format!("htlc lock record deserialization: {e}"))
    })?;

    // Check status — must be Pending.
    match record.status {
        HtlcStatus::Pending => {}
        HtlcStatus::Claimed => {
            return Ok(TxExecutionRecord {
                read_set: ctx.read_set,
                write_set: ctx.write_set,
                gas_used: HTLC_CLAIM_GAS,
                status: ExecutionStatus::HtlcAlreadyClaimed,
                state_changes: ctx.state_changes,
            });
        }
        HtlcStatus::Refunded => {
            return Ok(TxExecutionRecord {
                read_set: ctx.read_set,
                write_set: ctx.write_set,
                gas_used: HTLC_CLAIM_GAS,
                status: ExecutionStatus::HtlcAlreadyRefunded,
                state_changes: ctx.state_changes,
            });
        }
    }

    // Verify preimage: BLAKE3(HTLC_LOCK_DOMAIN ‖ preimage) == lock_hash.
    let computed_hash = compute_lock_hash(preimage);
    if computed_hash != record.lock_hash {
        return Ok(TxExecutionRecord {
            read_set: ctx.read_set,
            write_set: ctx.write_set,
            gas_used: HTLC_CLAIM_GAS,
            status: ExecutionStatus::HtlcPreimageMismatch,
            state_changes: ctx.state_changes,
        });
    }

    // Credit recipient.
    let recipient = record.recipient;
    let balance_key = b"balance".to_vec();
    let recipient_key = StateKey {
        account: recipient,
        key: balance_key.clone(),
    };
    let recipient_balance_raw = ctx.overlay.read(ctx.tx_index, &recipient_key)?;
    ctx.read_set
        .insert(recipient_key.clone(), recipient_balance_raw.clone());
    let recipient_balance = parse_balance(&recipient_balance_raw);

    let new_recipient_balance = recipient_balance.saturating_add(record.amount.0);
    let recipient_bytes = new_recipient_balance.to_le_bytes().to_vec();
    ctx.write_set
        .insert(recipient_key.clone(), Some(recipient_bytes.clone()));
    ctx.state_changes.push(StateChange {
        account: recipient,
        key: balance_key,
        value: Some(recipient_bytes),
    });

    // Update lock status to Claimed.
    record.status = HtlcStatus::Claimed;
    let updated_record_bytes = bcs::to_bytes(&record).map_err(|e| {
        crate::error::ExecutionError::Codec(format!("htlc lock record serialization: {e}"))
    })?;
    ctx.write_set
        .insert(htlc_key.clone(), Some(updated_record_bytes.clone()));
    ctx.state_changes.push(StateChange {
        account: HTLC_SYSTEM_ACCOUNT,
        key: htlc_key.key,
        value: Some(updated_record_bytes),
    });

    Ok(TxExecutionRecord {
        read_set: ctx.read_set,
        write_set: ctx.write_set,
        gas_used: HTLC_CLAIM_GAS,
        status: ExecutionStatus::Success,
        state_changes: ctx.state_changes,
    })
}

/// Execute an HtlcRefund: verify timeout, credit sender, mark lock refunded.
fn execute_htlc_refund(
    mut ctx: HtlcExecContext<'_>,
    sender: AccountAddress,
    lock_digest: Blake3Digest,
) -> ExecutionResult<TxExecutionRecord> {
    // Read the lock record.
    let htlc_key = htlc_state_key(&lock_digest);
    let lock_raw = ctx.overlay.read(ctx.tx_index, &htlc_key)?;
    ctx.read_set.insert(htlc_key.clone(), lock_raw.clone());

    let lock_bytes = match lock_raw {
        Some(b) => b,
        None => {
            return Ok(TxExecutionRecord {
                read_set: ctx.read_set,
                write_set: ctx.write_set,
                gas_used: HTLC_REFUND_GAS,
                status: ExecutionStatus::HtlcLockNotFound,
                state_changes: ctx.state_changes,
            });
        }
    };

    let mut record: HtlcLockRecord = bcs::from_bytes(&lock_bytes).map_err(|e| {
        crate::error::ExecutionError::Codec(format!("htlc lock record deserialization: {e}"))
    })?;

    // Check status — must be Pending.
    match record.status {
        HtlcStatus::Pending => {}
        HtlcStatus::Claimed => {
            return Ok(TxExecutionRecord {
                read_set: ctx.read_set,
                write_set: ctx.write_set,
                gas_used: HTLC_REFUND_GAS,
                status: ExecutionStatus::HtlcAlreadyClaimed,
                state_changes: ctx.state_changes,
            });
        }
        HtlcStatus::Refunded => {
            return Ok(TxExecutionRecord {
                read_set: ctx.read_set,
                write_set: ctx.write_set,
                gas_used: HTLC_REFUND_GAS,
                status: ExecutionStatus::HtlcAlreadyRefunded,
                state_changes: ctx.state_changes,
            });
        }
    }

    // Verify sender is the original lock sender.
    if sender != record.sender {
        return Ok(TxExecutionRecord {
            read_set: ctx.read_set,
            write_set: ctx.write_set,
            gas_used: HTLC_REFUND_GAS,
            status: ExecutionStatus::MoveAbort {
                location: "nexus::htlc_refund".into(),
                code: 2, // UNAUTHORIZED — not the original sender
            },
            state_changes: ctx.state_changes,
        });
    }

    // Verify timeout has passed: the tx's expiry_epoch serves as the
    // "current epoch" proxy here — the executor stamps it with the
    // current epoch during pre-validation.  For a more precise check
    // we use the lock's timeout directly against the current epoch
    // embedded in the transaction's expiry field.
    // NOTE: In Phase V-6 we will pass current_epoch from the executor context.
    // For now, we accept the refund if the tx exists and the lock is expired
    // by comparing against the lock's timeout epoch.  The execution bridge
    // will only accept refund txs whose expiry_epoch >= timeout_epoch.
    //
    // Simplified check: refund is valid if the lock's timeout has been reached.
    // The actual current epoch check is enforced by the refund tx's
    // expiry_epoch being set to >= timeout_epoch by the intent resolver.
    // We read the epoch state key for a definitive check.
    let epoch_key = StateKey {
        account: AccountAddress([0u8; 32]),
        key: b"current_epoch".to_vec(),
    };
    let epoch_raw = ctx.overlay.read(ctx.tx_index, &epoch_key)?;
    ctx.read_set.insert(epoch_key, epoch_raw.clone());
    let current_epoch = epoch_raw
        .as_ref()
        .and_then(|b| b.as_slice().try_into().ok())
        .map(u64::from_le_bytes)
        .unwrap_or(0);

    if current_epoch < record.timeout_epoch.0 {
        return Ok(TxExecutionRecord {
            read_set: ctx.read_set,
            write_set: ctx.write_set,
            gas_used: HTLC_REFUND_GAS,
            status: ExecutionStatus::HtlcRefundTooEarly,
            state_changes: ctx.state_changes,
        });
    }

    // Credit sender (refund the locked amount).
    let balance_key = b"balance".to_vec();
    let sender_key = StateKey {
        account: sender,
        key: balance_key.clone(),
    };
    let sender_balance_raw = ctx.overlay.read(ctx.tx_index, &sender_key)?;
    ctx.read_set
        .insert(sender_key.clone(), sender_balance_raw.clone());
    let sender_balance = parse_balance(&sender_balance_raw);

    let new_sender_balance = sender_balance.saturating_add(record.amount.0);
    let sender_bytes = new_sender_balance.to_le_bytes().to_vec();
    ctx.write_set
        .insert(sender_key.clone(), Some(sender_bytes.clone()));
    ctx.state_changes.push(StateChange {
        account: sender,
        key: balance_key,
        value: Some(sender_bytes),
    });

    // Update lock status to Refunded.
    record.status = HtlcStatus::Refunded;
    let updated_record_bytes = bcs::to_bytes(&record).map_err(|e| {
        crate::error::ExecutionError::Codec(format!("htlc lock record serialization: {e}"))
    })?;
    ctx.write_set
        .insert(htlc_key.clone(), Some(updated_record_bytes.clone()));
    ctx.state_changes.push(StateChange {
        account: HTLC_SYSTEM_ACCOUNT,
        key: htlc_key.key,
        value: Some(updated_record_bytes),
    });

    Ok(TxExecutionRecord {
        read_set: ctx.read_set,
        write_set: ctx.write_set,
        gas_used: HTLC_REFUND_GAS,
        status: ExecutionStatus::Success,
        state_changes: ctx.state_changes,
    })
}

/// Parse a balance from raw bytes (little-endian u64), defaulting to 0.
pub(crate) fn parse_balance(raw: &Option<Vec<u8>>) -> u64 {
    raw.as_ref()
        .and_then(|b| b.as_slice().try_into().ok())
        .map(u64::from_le_bytes)
        .unwrap_or(0)
}

// ── Gas calibration tests (D-4) ─────────────────────────────────────────

#[cfg(test)]
mod gas_calibration_tests {
    use super::*;

    /// Maximum reasonable gas for a single native operation.
    /// Any constant exceeding this suggests a calibration error.
    const MAX_SINGLE_OP_GAS: u64 = 100_000;

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn gas_constants_are_nonzero() {
        assert!(TRANSFER_GAS > 0, "TRANSFER_GAS must be non-zero");
        assert!(ANCHOR_GAS > 0, "ANCHOR_GAS must be non-zero");
        assert!(HTLC_LOCK_GAS > 0, "HTLC_LOCK_GAS must be non-zero");
        assert!(HTLC_CLAIM_GAS > 0, "HTLC_CLAIM_GAS must be non-zero");
        assert!(HTLC_REFUND_GAS > 0, "HTLC_REFUND_GAS must be non-zero");
    }

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn gas_constants_within_calibrated_bounds() {
        for (name, value) in [
            ("TRANSFER_GAS", TRANSFER_GAS),
            ("ANCHOR_GAS", ANCHOR_GAS),
            ("HTLC_LOCK_GAS", HTLC_LOCK_GAS),
            ("HTLC_CLAIM_GAS", HTLC_CLAIM_GAS),
            ("HTLC_REFUND_GAS", HTLC_REFUND_GAS),
        ] {
            assert!(
                value <= MAX_SINGLE_OP_GAS,
                "{name} = {value} exceeds calibrated upper bound {MAX_SINGLE_OP_GAS}"
            );
        }
    }

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn htlc_operations_cost_at_least_transfer() {
        // HTLC operations are strictly more complex than a simple transfer.
        assert!(
            HTLC_LOCK_GAS >= TRANSFER_GAS,
            "HTLC lock should cost at least as much as a transfer"
        );
        assert!(
            HTLC_CLAIM_GAS >= TRANSFER_GAS,
            "HTLC claim should cost at least as much as a transfer"
        );
        assert!(
            HTLC_REFUND_GAS >= TRANSFER_GAS,
            "HTLC refund should cost at least as much as a transfer"
        );
    }

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn anchor_is_cheapest_operation() {
        // Provenance anchoring is a lightweight system write — should be
        // the cheapest operation in the gas schedule.
        assert!(
            ANCHOR_GAS <= TRANSFER_GAS,
            "anchor gas ({ANCHOR_GAS}) should be <= transfer gas ({TRANSFER_GAS})"
        );
    }
}
