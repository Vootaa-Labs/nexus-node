//! Block-STM parallel transaction execution engine.
//!
//! Implements the three-phase optimistic concurrency control pipeline:
//!
//! 1. **Phase 1 — Optimistic Parallel Execution**: All transactions execute
//!    concurrently via rayon against a **read-only** base state snapshot.
//!    Per-transaction read-sets and write-sets are recorded locally; no
//!    writes are applied to the shared overlay during this phase (SEC-C1).
//!
//! 2. **Phase 2 — Sequential Validation + Write Application**: Transactions
//!    are validated in order. Writes from each validated transaction are
//!    applied to the overlay before the next transaction is checked.
//!    If transaction `j`'s read-set is inconsistent (due to writes by
//!    earlier transactions), `j` is re-executed against the definitive
//!    overlay state. Adaptive parallelism reduces concurrency under
//!    sustained high conflict rates.
//!
//! 3. **Phase 3 — State Commit**: Validated write-sets are merged into a
//!    single [`BlockExecutionResult`] with per-transaction receipts and
//!    a canonical state root.
//!
//! # Architecture
//!
//! ```text
//! SignedTransaction[] ─┬─ Phase 1 (rayon, read-only base) ─────→ ReadSet/WriteSet[]
//!                      │                                            │
//!                      │  Phase 2 (sequential, overlay writes) ──→ validate / re-execute
//!                      │                                            │
//!                      └──── Phase 3 ──────────────────────────→ BlockExecutionResult
//! ```
//!
//! # Submodules
//!
//! - [`mvhashmap`]  — DashMap-backed MVCC overlay with configurable version cap
//! - [`executor`]   — Single-transaction execution logic
//! - [`adaptive`]   — Conflict-rate-based adaptive parallelism controller

mod adaptive;
mod executor;
pub(crate) mod mvhashmap;

pub use executor::AnchorStateEntry;

use std::collections::BTreeMap;
use std::sync::Arc;

use parking_lot::Mutex;
use rayon::prelude::*;
use tracing::debug;

use crate::error::{ExecutionError, ExecutionResult};
use crate::metrics::ExecutionMetrics;
use crate::move_adapter::query::{self, QueryResult};
use crate::move_adapter::{MoveExecutor, VmConfig};
use crate::traits::StateView;
use crate::types::{BlockExecutionResult, SignedTransaction, StateChange, TransactionReceipt};
use nexus_primitives::{
    AccountAddress, Blake3Digest, CommitSequence, EpochNumber, ShardId, TimestampMs,
};
use nexus_storage::canonical_empty_root;

use adaptive::AdaptiveParallelism;
use executor::{execute_single_tx, validate_tx_preexec};
use mvhashmap::MvHashMap;

// ── BlockStmExecutor ────────────────────────────────────────────────────

/// Block-STM parallel execution engine.
///
/// Implements [`TransactionExecutor`](crate::traits::TransactionExecutor)
/// using the three-phase OCC pipeline:
///
/// 1. Optimistic parallel execution via rayon against a shared [`MvHashMap`]
/// 2. Sequential read-set validation with per-tx re-execution on conflict
/// 3. Aggregate state changes into a single result
///
/// Thread count is adjusted per-batch by an [`AdaptiveParallelism`]
/// controller that tracks conflict rates over a sliding window.
pub struct BlockStmExecutor {
    /// Shard this executor runs on.
    shard_id: ShardId,
    /// Commit sequence for receipts.
    commit_seq: CommitSequence,
    /// Timestamp for receipts.
    timestamp: TimestampMs,
    /// Maximum re-execution attempts per transaction.
    max_retries: u32,
    /// Adaptive thread-count controller (interior-mutable for `&self` API).
    adaptive: Mutex<AdaptiveParallelism>,
    /// Per-shard metrics handle.
    metrics: ExecutionMetrics,
    /// Optional storage-backed state view for `TransactionExecutor` trait.
    ///
    /// When set, `execute_block()` delegates to `execute()` with this view
    /// instead of the previous `NullStateView` fallback (SEC-H11).
    state_view: Option<Arc<dyn StateView>>,
    /// Current epoch for expiry validation (SEC-H3).
    current_epoch: EpochNumber,
    /// Node's chain ID for cross-chain replay prevention (SEC-H4).
    chain_id: u64,
}

impl BlockStmExecutor {
    /// Create a new executor for the given shard and commit sequence.
    pub fn new(shard_id: ShardId, commit_seq: CommitSequence, timestamp: TimestampMs) -> Self {
        let max_workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        Self {
            shard_id,
            commit_seq,
            timestamp,
            max_retries: 5,
            adaptive: Mutex::new(AdaptiveParallelism::new(max_workers)),
            metrics: ExecutionMetrics::new(shard_id),
            state_view: None,
            current_epoch: EpochNumber(0),
            chain_id: 1,
        }
    }

    /// Create an executor with explicit configuration.
    pub fn with_config(
        shard_id: ShardId,
        commit_seq: CommitSequence,
        timestamp: TimestampMs,
        max_retries: u32,
        max_workers: usize,
    ) -> Self {
        Self {
            shard_id,
            commit_seq,
            timestamp,
            max_retries,
            adaptive: Mutex::new(AdaptiveParallelism::new(max_workers)),
            metrics: ExecutionMetrics::new(shard_id),
            state_view: None,
            current_epoch: EpochNumber(0),
            chain_id: 1,
        }
    }

    /// Set the current epoch for expiry validation.
    pub fn set_epoch(&mut self, epoch: EpochNumber) {
        self.current_epoch = epoch;
    }

    /// Set the node's chain ID for cross-chain replay prevention.
    pub fn set_chain_id(&mut self, chain_id: u64) {
        self.chain_id = chain_id;
    }

    /// Attach a storage-backed state view so that `execute_block()` (the
    /// `TransactionExecutor` trait method) reads from real storage instead
    /// of the removed `NullStateView` fallback.
    pub fn set_state_view(&mut self, state: Arc<dyn StateView>) {
        self.state_view = Some(state);
    }

    /// Execute a block of transactions using Block-STM.
    ///
    /// `state` provides the pre-execution state snapshot (read-only).
    ///
    /// **Phase 1** executes all transactions concurrently against the base
    /// state snapshot.  No writes are applied to the shared overlay during
    /// this phase, eliminating non-deterministic write-set pollution
    /// between concurrently executing transactions (SEC-C1).
    ///
    /// **Phase 2** processes transactions in order: it applies each
    /// validated transaction's write-set to the overlay first, then
    /// validates the next transaction's read-set. Invalid transactions
    /// are re-executed against the up-to-date overlay.
    ///
    /// **Phase 3** aggregates results.  The `new_state_root` in the
    /// returned result is a **per-batch flat hash** — the canonical
    /// commitment root is derived by the execution bridge from the
    /// authenticated commitment tree after state is persisted.
    pub fn execute(
        &self,
        transactions: &[SignedTransaction],
        state: &dyn StateView,
    ) -> ExecutionResult<BlockExecutionResult> {
        if transactions.is_empty() {
            return Ok(BlockExecutionResult {
                new_state_root: canonical_empty_root(),
                receipts: vec![],
                gas_used_total: 0,
                execution_ms: 0,
            });
        }

        let start = std::time::Instant::now();
        let n = transactions.len();

        // Determine thread count from adaptive controller.
        let recommended = self.adaptive.lock().recommend_workers();

        // Build a scoped thread pool with the recommended worker count.
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(recommended)
            .build()
            .or_else(|_| rayon::ThreadPoolBuilder::new().build())
            .map_err(|e| ExecutionError::Storage(format!("thread pool creation failed: {e}")))?;

        // ── Pre-validation: static checks (SEC-H1, SEC-H3, SEC-H4) ────
        //
        // Validate signature, sender-pk binding, digest, expiry, and
        // chain_id for every transaction before entering the Block-STM
        // pipeline.  Invalid transactions receive rejection records and
        // do not participate in execution.
        let mut prevalidation: Vec<Option<executor::TxExecutionRecord>> = Vec::with_capacity(n);
        for tx in transactions {
            prevalidation.push(validate_tx_preexec(tx, self.current_epoch, self.chain_id));
        }

        // ── Phase 1: Optimistic parallel execution (read-only base) ─────
        //
        // All transactions execute against the base state snapshot.  No
        // writes are applied to the overlay during this phase — each
        // transaction's write-set is captured locally and applied only
        // in Phase 2 after sequential validation (SEC-C1).
        //
        // Transactions that failed pre-validation are skipped.
        debug_assert!(n <= u32::MAX as usize, "batch size exceeds u32::MAX");
        let overlay = MvHashMap::with_capacity(state, n);
        let move_executor = MoveExecutor::new(VmConfig::default());

        let records: Vec<ExecutionResult<_>> = pool.install(|| {
            transactions
                .par_iter()
                .enumerate()
                .map(|(i, tx)| {
                    if prevalidation[i].is_some() {
                        // Already rejected — return a placeholder that will
                        // be replaced with the rejection record in the merge.
                        return Ok(executor::TxExecutionRecord {
                            read_set: std::collections::HashMap::new(),
                            write_set: std::collections::HashMap::new(),
                            gas_used: 0,
                            status: crate::types::ExecutionStatus::InvalidSignature,
                            state_changes: vec![],
                        });
                    }
                    let record = execute_single_tx(tx, i as u32, &overlay, &move_executor)?;
                    // NOTE: writes are NOT applied here — Phase 2 applies
                    // them sequentially after validation (SEC-C1).
                    Ok(record)
                })
                .collect()
        });

        // Unwrap records, failing the entire block if any tx had a fatal error.
        let mut records = records.into_iter().collect::<ExecutionResult<Vec<_>>>()?;

        // Replace placeholder records with the actual rejection records
        // from pre-validation.
        for (i, rejection) in prevalidation.into_iter().enumerate() {
            if let Some(rej) = rejection {
                records[i] = rej;
            }
        }

        // ── Phase 2: Sequential validation + write application ──────────
        //
        // Process transactions in order.  For each tx i:
        //   1. Skip if pre-validation already rejected the transaction.
        //   2. Validate tx i's read-set against the overlay (which contains
        //      definitive writes from tx 0 .. tx i-1).
        //   3. If valid, apply tx i's write-set to the overlay.
        //   4. If invalid, re-execute tx i against the overlay, then apply
        //      the new write-set.
        //
        // Because Phase 1 did NOT apply any writes, tx 0 always reads
        // base state and is always valid.  Subsequent transactions may
        // need re-execution if their Phase 1 reads diverge from the
        // state produced by earlier definitive writes.
        let mut conflict_count: u32 = 0;

        for i in 0..n {
            // Skip transactions that failed pre-validation — they have
            // empty write-sets and should not participate in OCC.
            if matches!(
                records[i].status,
                crate::types::ExecutionStatus::InvalidSignature
                    | crate::types::ExecutionStatus::SenderMismatch
                    | crate::types::ExecutionStatus::Expired
                    | crate::types::ExecutionStatus::ChainIdMismatch
            ) {
                continue;
            }

            let valid = records[i].read_set.iter().all(|(key, observed)| {
                overlay
                    .validate_read(i as u32, key, observed)
                    .unwrap_or(false)
            });

            if !valid {
                conflict_count = conflict_count.saturating_add(1);

                // Check retry budget.
                if conflict_count > self.max_retries.saturating_mul(n as u32) {
                    return Err(ExecutionError::MaxRetriesExceeded {
                        tx_index: i as u32,
                        retries: conflict_count,
                    });
                }

                // Re-execute against the definitive overlay state.
                let new_record =
                    execute_single_tx(&transactions[i], i as u32, &overlay, &move_executor)?;
                records[i] = new_record;
            }

            // Apply this transaction's definitive write-set to the overlay
            // so that subsequent transactions see it during validation.
            overlay
                .apply_writes(i as u32, &records[i].write_set)
                .map_err(|e| {
                    ExecutionError::Storage(format!("version cap exceeded during Phase 2: {e}"))
                })?;
        }

        let conflict_rate = if n > 1 {
            conflict_count as f64 / (n as f64 - 1.0)
        } else {
            0.0
        };

        // Feed conflict rate to the adaptive controller.
        self.adaptive.lock().record_conflict_rate(conflict_rate);

        // ── Metrics: Phase 2 conflict data ──
        self.metrics.record_conflicts(conflict_count, conflict_rate);

        debug!(
            conflict_count,
            conflict_rate,
            recommended_workers = recommended,
            total_txs = n,
            "Block-STM validation complete"
        );

        // ── Phase 3: Aggregate results + canonical state root ───────────
        let mut gas_used_total = 0u64;
        let mut receipts = Vec::with_capacity(n);

        // Collect all state changes for state root computation.
        // Per-tx state_changes are preserved in receipts for provenance.
        let mut all_state_changes: Vec<StateChange> = Vec::new();

        for (i, record) in records.into_iter().enumerate() {
            gas_used_total = gas_used_total.saturating_add(record.gas_used);
            all_state_changes.extend(record.state_changes.clone());

            receipts.push(TransactionReceipt {
                tx_digest: transactions[i].digest,
                commit_seq: self.commit_seq,
                shard_id: self.shard_id,
                status: record.status,
                gas_used: record.gas_used,
                state_changes: record.state_changes,
                timestamp: self.timestamp,
            });
        }

        // Compute a per-batch flat hash.  The authoritative canonical
        // commitment root is derived by the execution bridge from the
        // authenticated Merkle commitment tree (Phase N unification).
        #[allow(deprecated)]
        let new_state_root = compute_state_root(&all_state_changes);
        let elapsed = start.elapsed();

        // ── Metrics: Phase 3 batch summary ──
        let failed_count = receipts
            .iter()
            .filter(|r| r.status != crate::types::ExecutionStatus::Success)
            .count() as u64;
        self.metrics.record_batch(
            n as u64,
            failed_count,
            gas_used_total,
            elapsed.as_secs_f64(),
        );

        Ok(BlockExecutionResult {
            new_state_root,
            receipts,
            gas_used_total,
            execution_ms: elapsed.as_millis() as u32,
        })
    }

    /// Execute a block of transactions **serially** (no parallelism).
    ///
    /// This is the reference implementation for differential testing
    /// (A-2).  It processes transactions one by one in order, applying
    /// each write-set before executing the next.  The final result must
    /// be identical to `execute()` for any input.
    pub fn execute_serial(
        &self,
        transactions: &[SignedTransaction],
        state: &dyn StateView,
    ) -> ExecutionResult<BlockExecutionResult> {
        if transactions.is_empty() {
            return Ok(BlockExecutionResult {
                new_state_root: canonical_empty_root(),
                receipts: vec![],
                gas_used_total: 0,
                execution_ms: 0,
            });
        }

        let start = std::time::Instant::now();
        let n = transactions.len();
        let overlay = MvHashMap::with_capacity(state, n);
        let move_executor = MoveExecutor::new(VmConfig::default());

        let mut gas_used_total = 0u64;
        let mut all_state_changes: Vec<StateChange> = Vec::new();
        let mut receipts = Vec::with_capacity(n);

        for (i, tx) in transactions.iter().enumerate() {
            // Pre-validation: skip execution for statically invalid txs.
            if let Some(rej) = validate_tx_preexec(tx, self.current_epoch, self.chain_id) {
                all_state_changes.extend(rej.state_changes.clone());
                receipts.push(TransactionReceipt {
                    tx_digest: tx.digest,
                    commit_seq: self.commit_seq,
                    shard_id: self.shard_id,
                    status: rej.status,
                    gas_used: rej.gas_used,
                    state_changes: rej.state_changes,
                    timestamp: self.timestamp,
                });
                continue;
            }

            let record = execute_single_tx(tx, i as u32, &overlay, &move_executor)?;
            overlay
                .apply_writes(i as u32, &record.write_set)
                .map_err(|e| {
                    ExecutionError::Storage(format!("version cap exceeded in serial executor: {e}"))
                })?;

            gas_used_total = gas_used_total.saturating_add(record.gas_used);
            all_state_changes.extend(record.state_changes.clone());

            receipts.push(TransactionReceipt {
                tx_digest: tx.digest,
                commit_seq: self.commit_seq,
                shard_id: self.shard_id,
                status: record.status,
                gas_used: record.gas_used,
                state_changes: record.state_changes,
                timestamp: self.timestamp,
            });
        }

        #[allow(deprecated)]
        let new_state_root = compute_state_root(&all_state_changes);
        let elapsed = start.elapsed();

        Ok(BlockExecutionResult {
            new_state_root,
            receipts,
            gas_used_total,
            execution_ms: elapsed.as_millis() as u32,
        })
    }

    /// The shard this executor runs on.
    pub fn shard_id(&self) -> ShardId {
        self.shard_id
    }

    /// Execute a read-only view function query against the given state.
    ///
    /// This does **not** go through consensus; it reads directly from
    /// the committed state snapshot.  Used by the RPC layer for
    /// `query_contract` / view function requests.
    pub fn query_view(
        &self,
        state: &dyn StateView,
        contract: AccountAddress,
        function: &str,
        type_args: &[Vec<u8>],
        args: &[Vec<u8>],
    ) -> ExecutionResult<QueryResult> {
        query::query_view_function(state, contract, function, type_args, args)
    }
}

/// Implement `TransactionExecutor` for `BlockStmExecutor`.
///
/// Requires a state view to have been attached via [`set_state_view`].
/// Returns `ExecutionError::Storage` if no state view is configured,
/// preventing the previous `NullStateView` silent-empty-state fallback
/// from ever reaching production (SEC-H11).
impl crate::traits::TransactionExecutor for BlockStmExecutor {
    fn execute_block(
        &self,
        transactions: &[SignedTransaction],
        _state_root: Blake3Digest,
    ) -> ExecutionResult<BlockExecutionResult> {
        let state = self.state_view.as_ref().ok_or_else(|| {
            ExecutionError::Storage(
                "BlockStmExecutor::execute_block called without a state view — \
                 call set_state_view() first or use execute() directly"
                    .into(),
            )
        })?;
        self.execute(transactions, state.as_ref())
    }
}

/// Compute a **per-batch** flat state hash from a set of state changes.
///
/// 1. Deduplicate by `(account, key)` — last write wins (preserves tx order).
/// 2. Sort by `(account, key)` for deterministic iteration order.
/// 3. Hash with domain-separated BLAKE3.
///
/// **Deprecated (Phase N)**: This flat hash is NOT the canonical commitment
/// root.  The execution bridge derives the authoritative root from the
/// authenticated BLAKE3 Merkle commitment tree after persisting state.
/// This function is retained only for differential-testing determinism.
#[deprecated(note = "use the canonical commitment root from the commitment tracker instead")]
fn compute_state_root(changes: &[StateChange]) -> Blake3Digest {
    // Deduplicate: last write per (account, key) wins.
    // Use the raw [u8;32] for ordering since AccountAddress doesn't impl Ord.
    #[allow(clippy::type_complexity)]
    let mut final_state: BTreeMap<([u8; 32], &[u8]), Option<&[u8]>> = BTreeMap::new();
    for change in changes {
        final_state.insert(
            (change.account.0, change.key.as_slice()),
            change.value.as_deref(),
        );
    }

    let mut hasher = blake3::Hasher::new();
    hasher.update(b"nexus::execution::state_root::v2");
    // Iterate in BTreeMap's sorted order (deterministic).
    for ((account, key), value) in &final_state {
        hasher.update(account);
        hasher.update(key);
        if let Some(v) = value {
            hasher.update(v);
        } else {
            hasher.update(b"\x00"); // Sentinel for delete.
        }
    }
    Blake3Digest(*hasher.finalize().as_bytes())
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(deprecated)] // compute_state_root() used for determinism tests
mod tests {
    use super::*;
    use crate::types::{
        compute_tx_digest, ExecutionStatus, TransactionBody, TransactionPayload, TX_DOMAIN,
    };
    use executor::TRANSFER_GAS;
    use nexus_crypto::{DilithiumSigner, DilithiumSigningKey, DilithiumVerifyKey, Signer};
    use nexus_primitives::{Amount, EpochNumber, TokenId};
    use std::collections::HashMap as StdHashMap;

    // ── Test helpers ────────────────────────────────────────────────

    /// In-memory state view for testing.
    struct MemStateView {
        data: StdHashMap<(AccountAddress, Vec<u8>), Vec<u8>>,
    }

    impl MemStateView {
        fn new() -> Self {
            Self {
                data: StdHashMap::new(),
            }
        }

        fn set_balance(&mut self, addr: AccountAddress, balance: u64) {
            self.data
                .insert((addr, b"balance".to_vec()), balance.to_le_bytes().to_vec());
        }
    }

    impl StateView for MemStateView {
        fn get(&self, account: &AccountAddress, key: &[u8]) -> ExecutionResult<Option<Vec<u8>>> {
            Ok(self.data.get(&(*account, key.to_vec())).cloned())
        }
    }

    /// A test account with a keypair and a derived address.
    struct TestAccount {
        sk: DilithiumSigningKey,
        pk: DilithiumVerifyKey,
        address: AccountAddress,
    }

    impl TestAccount {
        fn random() -> Self {
            let (sk, pk) = DilithiumSigner::generate_keypair();
            let address = AccountAddress::from_dilithium_pubkey(pk.as_bytes());
            Self { sk, pk, address }
        }
    }

    /// Recipient-only address (no keypair needed).
    fn recipient_addr() -> AccountAddress {
        AccountAddress([0xBB; 32])
    }

    fn make_transfer(
        sender: &TestAccount,
        recipient: AccountAddress,
        amount: u64,
        nonce: u64,
    ) -> SignedTransaction {
        let body = TransactionBody {
            sender: sender.address,
            sequence_number: nonce,
            expiry_epoch: EpochNumber(1000),
            gas_limit: 50_000,
            gas_price: 1,
            target_shard: None,
            payload: TransactionPayload::Transfer {
                recipient,
                amount: Amount(amount),
                token: TokenId::Native,
            },
            chain_id: 1,
        };
        let digest = compute_tx_digest(&body).unwrap();
        let sig = DilithiumSigner::sign(&sender.sk, TX_DOMAIN, digest.as_bytes());
        SignedTransaction {
            body,
            signature: sig,
            sender_pk: sender.pk.clone(),
            digest,
        }
    }

    // ── Existing tests (updated for authenticated transactions) ─────

    #[test]
    fn empty_block() {
        let executor = BlockStmExecutor::new(ShardId(0), CommitSequence(1), TimestampMs::now());
        let state = MemStateView::new();
        let result = executor.execute(&[], &state).unwrap();
        assert!(result.receipts.is_empty());
        assert_eq!(result.gas_used_total, 0);
    }

    #[test]
    fn single_transfer_success() {
        let executor = BlockStmExecutor::new(ShardId(0), CommitSequence(1), TimestampMs::now());
        let alice = TestAccount::random();
        let mut state = MemStateView::new();
        state.set_balance(alice.address, 1_000_000);

        let tx = make_transfer(&alice, recipient_addr(), 500, 0);
        let result = executor.execute(&[tx], &state).unwrap();

        assert_eq!(result.receipts.len(), 1);
        assert_eq!(result.receipts[0].status, ExecutionStatus::Success);
        assert_eq!(result.receipts[0].gas_used, TRANSFER_GAS);
    }

    #[test]
    fn transfer_insufficient_balance() {
        let executor = BlockStmExecutor::new(ShardId(0), CommitSequence(1), TimestampMs::now());
        let alice = TestAccount::random();
        let mut state = MemStateView::new();
        state.set_balance(alice.address, 100);

        let tx = make_transfer(&alice, recipient_addr(), 500, 0);
        let result = executor.execute(&[tx], &state).unwrap();

        assert_eq!(result.receipts.len(), 1);
        assert!(
            matches!(result.receipts[0].status, ExecutionStatus::MoveAbort { .. }),
            "expected MoveAbort, got {:?}",
            result.receipts[0].status
        );
    }

    #[test]
    fn independent_transfers_no_conflict() {
        let executor = BlockStmExecutor::new(ShardId(0), CommitSequence(1), TimestampMs::now());
        let alice = TestAccount::random();
        let carol = TestAccount::random();
        let bob = recipient_addr();
        let mut state = MemStateView::new();
        state.set_balance(alice.address, 1_000_000);
        state.set_balance(carol.address, 1_000_000);

        let tx1 = make_transfer(&alice, bob, 100, 0);
        let tx2 = make_transfer(&carol, bob, 200, 0);
        let result = executor.execute(&[tx1, tx2], &state).unwrap();

        assert_eq!(result.receipts.len(), 2);
        assert_eq!(result.receipts[0].status, ExecutionStatus::Success);
        assert_eq!(result.receipts[1].status, ExecutionStatus::Success);
        assert_eq!(result.gas_used_total, TRANSFER_GAS * 2);
    }

    #[test]
    fn conflicting_transfers_resolved() {
        let executor = BlockStmExecutor::new(ShardId(0), CommitSequence(1), TimestampMs::now());
        let alice = TestAccount::random();
        let mut state = MemStateView::new();
        state.set_balance(alice.address, 1_000_000);

        let tx1 = make_transfer(&alice, recipient_addr(), 100, 0);
        let tx2 = make_transfer(&alice, AccountAddress([0xCC; 32]), 200, 1);
        let result = executor.execute(&[tx1, tx2], &state).unwrap();

        assert_eq!(result.receipts.len(), 2);
        assert_eq!(result.receipts[0].status, ExecutionStatus::Success);
        assert_eq!(result.receipts[1].status, ExecutionStatus::Success);
    }

    #[test]
    fn receipts_preserve_order() {
        let executor = BlockStmExecutor::new(ShardId(0), CommitSequence(42), TimestampMs::now());
        let alice = TestAccount::random();
        let mut state = MemStateView::new();
        state.set_balance(alice.address, 10_000_000);

        let txs: Vec<_> = (0..5)
            .map(|i| make_transfer(&alice, recipient_addr(), 10, i))
            .collect();

        let digests: Vec<_> = txs.iter().map(|tx| tx.digest).collect();
        let result = executor.execute(&txs, &state).unwrap();

        assert_eq!(result.receipts.len(), 5);
        for (i, receipt) in result.receipts.iter().enumerate() {
            assert_eq!(receipt.tx_digest, digests[i]);
            assert_eq!(receipt.commit_seq, CommitSequence(42));
            assert_eq!(receipt.shard_id, ShardId(0));
        }
    }

    #[test]
    fn state_root_deterministic() {
        let executor = BlockStmExecutor::new(ShardId(0), CommitSequence(1), TimestampMs(1000));
        let alice = TestAccount::random();
        let mut state = MemStateView::new();
        state.set_balance(alice.address, 1_000_000);

        let tx = make_transfer(&alice, recipient_addr(), 500, 0);
        let r1 = executor.execute(&[tx.clone()], &state).unwrap();
        let r2 = executor.execute(&[tx], &state).unwrap();
        assert_eq!(r1.new_state_root, r2.new_state_root);
    }

    #[test]
    fn trait_impl_works_with_state_view() {
        use crate::traits::TransactionExecutor;

        let mut executor = BlockStmExecutor::new(ShardId(0), CommitSequence(1), TimestampMs::now());
        let state = Arc::new(MemStateView::new());
        executor.set_state_view(state);
        let result = executor
            .execute_block(&[], Blake3Digest([0u8; 32]))
            .unwrap();
        assert!(result.receipts.is_empty());
    }

    #[test]
    fn trait_impl_rejects_missing_state_view() {
        use crate::traits::TransactionExecutor;

        let alice = TestAccount::random();
        let executor = BlockStmExecutor::new(ShardId(0), CommitSequence(1), TimestampMs::now());
        let _ok = executor.execute_block(&[], Blake3Digest([0u8; 32]));
        let tx = make_transfer(&alice, recipient_addr(), 100, 0);
        let result = executor.execute_block(&[tx], Blake3Digest([0u8; 32]));
        assert!(result.is_err(), "must fail without state view");
    }

    #[test]
    fn compute_state_root_empty() {
        let root = compute_state_root(&[]);
        let root2 = compute_state_root(&[]);
        assert_eq!(root, root2);
    }

    #[test]
    fn compute_state_root_changes_with_data() {
        let addr = AccountAddress([0xAA; 32]);
        let r1 = compute_state_root(&[StateChange {
            account: addr,
            key: b"balance".to_vec(),
            value: Some(vec![1, 2, 3]),
        }]);
        let r2 = compute_state_root(&[StateChange {
            account: addr,
            key: b"balance".to_vec(),
            value: Some(vec![4, 5, 6]),
        }]);
        assert_ne!(r1, r2);
    }

    #[test]
    #[cfg(not(feature = "move-vm"))]
    fn move_call_out_of_gas() {
        let executor = BlockStmExecutor::new(ShardId(0), CommitSequence(1), TimestampMs::now());
        let acct = TestAccount::random();
        let state = MemStateView::new();

        let body = TransactionBody {
            sender: acct.address,
            sequence_number: 0,
            expiry_epoch: EpochNumber(1000),
            gas_limit: 100,
            gas_price: 1,
            target_shard: None,
            payload: TransactionPayload::MoveCall {
                contract: nexus_primitives::ContractAddress([0xDD; 32]),
                function: "do_thing".into(),
                type_args: vec![],
                args: vec![],
            },
            chain_id: 1,
        };
        let digest = compute_tx_digest(&body).unwrap();
        let sig = DilithiumSigner::sign(&acct.sk, TX_DOMAIN, digest.as_bytes());
        let tx = SignedTransaction {
            body,
            signature: sig,
            sender_pk: acct.pk.clone(),
            digest,
        };

        let result = executor.execute(&[tx], &state).unwrap();
        assert_eq!(result.receipts[0].status, ExecutionStatus::OutOfGas);
    }

    #[test]
    fn shard_id_accessor() {
        let executor = BlockStmExecutor::new(ShardId(7), CommitSequence(0), TimestampMs(0));
        assert_eq!(executor.shard_id(), ShardId(7));
    }

    #[test]
    fn with_config_constructor() {
        let executor =
            BlockStmExecutor::with_config(ShardId(1), CommitSequence(10), TimestampMs(5000), 3, 4);
        assert_eq!(executor.shard_id(), ShardId(1));
        assert_eq!(executor.max_retries, 3);
    }

    #[test]
    fn adaptive_parallelism_integration() {
        let executor =
            BlockStmExecutor::with_config(ShardId(0), CommitSequence(1), TimestampMs::now(), 5, 8);
        let alice = TestAccount::random();
        let mut state = MemStateView::new();
        state.set_balance(alice.address, 10_000_000);

        for batch in 0..4u64 {
            let txs: Vec<_> = (0..3)
                .map(|i| make_transfer(&alice, recipient_addr(), 10, batch * 3 + i))
                .collect();
            let result = executor.execute(&txs, &state).unwrap();
            assert_eq!(result.receipts.len(), 3);
        }

        let avg = executor.adaptive.lock().average_conflict_rate();
        assert!(avg >= 0.0);
    }

    #[test]
    fn shared_mvhashmap_resolves_conflicts() {
        let executor =
            BlockStmExecutor::with_config(ShardId(0), CommitSequence(1), TimestampMs::now(), 5, 2);
        let alice = TestAccount::random();
        let mut state = MemStateView::new();
        state.set_balance(alice.address, 1_000_000);

        let tx0 = make_transfer(&alice, recipient_addr(), 100, 0);
        let tx1 = make_transfer(&alice, AccountAddress([0xCC; 32]), 200, 1);
        let result = executor.execute(&[tx0, tx1], &state).unwrap();

        assert_eq!(result.receipts.len(), 2);
        assert_eq!(result.receipts[0].status, ExecutionStatus::Success);
        assert_eq!(result.receipts[1].status, ExecutionStatus::Success);
    }

    #[test]
    fn many_independent_txs_no_conflicts() {
        let executor =
            BlockStmExecutor::with_config(ShardId(0), CommitSequence(1), TimestampMs::now(), 5, 4);
        let mut state = MemStateView::new();
        let senders: Vec<TestAccount> = (0..8).map(|_| TestAccount::random()).collect();
        for s in &senders {
            state.set_balance(s.address, 1_000_000);
        }

        let txs: Vec<_> = senders
            .iter()
            .map(|s| make_transfer(s, recipient_addr(), 100, 0))
            .collect();

        let result = executor.execute(&txs, &state).unwrap();
        assert_eq!(result.receipts.len(), 8);
        for receipt in &result.receipts {
            assert_eq!(receipt.status, ExecutionStatus::Success);
        }
    }

    // ── Phase A acceptance tests ────────────────────────────────────

    #[test]
    fn block_stm_parallel_must_match_serial_reference() {
        let executor =
            BlockStmExecutor::with_config(ShardId(0), CommitSequence(1), TimestampMs(1000), 5, 4);
        let alice = TestAccount::random();
        let carol = TestAccount::random();
        let mut state = MemStateView::new();
        state.set_balance(alice.address, 10_000_000);
        state.set_balance(carol.address, 5_000_000);

        let txs = vec![
            make_transfer(&alice, recipient_addr(), 100, 0),
            make_transfer(&alice, carol.address, 200, 1),
            make_transfer(&carol, recipient_addr(), 50, 0),
            make_transfer(&alice, recipient_addr(), 300, 2),
        ];

        let parallel = executor.execute(&txs, &state).unwrap();
        let serial = executor.execute_serial(&txs, &state).unwrap();

        assert_eq!(
            parallel.new_state_root, serial.new_state_root,
            "parallel and serial state roots must be identical"
        );
        assert_eq!(parallel.gas_used_total, serial.gas_used_total);
        assert_eq!(parallel.receipts.len(), serial.receipts.len());
        for (i, (p, s)) in parallel
            .receipts
            .iter()
            .zip(serial.receipts.iter())
            .enumerate()
        {
            assert_eq!(p.status, s.status, "receipt {i}: status mismatch");
            assert_eq!(p.gas_used, s.gas_used, "receipt {i}: gas mismatch");
            assert_eq!(
                p.state_changes, s.state_changes,
                "receipt {i}: state changes mismatch"
            );
        }
    }

    #[test]
    fn block_stm_parallel_matches_serial_independent() {
        let executor =
            BlockStmExecutor::with_config(ShardId(0), CommitSequence(1), TimestampMs(1000), 5, 4);
        let mut state = MemStateView::new();
        let senders: Vec<TestAccount> = (0..6).map(|_| TestAccount::random()).collect();
        for s in &senders {
            state.set_balance(s.address, 1_000_000);
        }

        let txs: Vec<_> = senders
            .iter()
            .map(|s| make_transfer(s, recipient_addr(), 100, 0))
            .collect();

        let parallel = executor.execute(&txs, &state).unwrap();
        let serial = executor.execute_serial(&txs, &state).unwrap();

        assert_eq!(parallel.new_state_root, serial.new_state_root);
        assert_eq!(parallel.gas_used_total, serial.gas_used_total);
        for (p, s) in parallel.receipts.iter().zip(serial.receipts.iter()) {
            assert_eq!(p.status, s.status);
        }
    }

    #[test]
    fn state_root_must_be_independent_of_change_iteration_order() {
        let addr_a = AccountAddress([0xAA; 32]);
        let addr_b = AccountAddress([0xBB; 32]);
        let changes_a = vec![
            StateChange {
                account: addr_a,
                key: b"balance".to_vec(),
                value: Some(vec![1]),
            },
            StateChange {
                account: addr_b,
                key: b"balance".to_vec(),
                value: Some(vec![2]),
            },
        ];
        let changes_b = vec![
            StateChange {
                account: addr_b,
                key: b"balance".to_vec(),
                value: Some(vec![2]),
            },
            StateChange {
                account: addr_a,
                key: b"balance".to_vec(),
                value: Some(vec![1]),
            },
        ];
        assert_eq!(
            compute_state_root(&changes_a),
            compute_state_root(&changes_b),
            "state root must be order-independent"
        );
    }

    #[test]
    fn state_root_deduplicates_same_key_writes() {
        let addr = AccountAddress([0xAA; 32]);
        let changes_with_dup = vec![
            StateChange {
                account: addr,
                key: b"balance".to_vec(),
                value: Some(vec![1]),
            },
            StateChange {
                account: addr,
                key: b"balance".to_vec(),
                value: Some(vec![2]),
            },
        ];
        let changes_final_only = vec![StateChange {
            account: addr,
            key: b"balance".to_vec(),
            value: Some(vec![2]),
        }];
        assert_eq!(
            compute_state_root(&changes_with_dup),
            compute_state_root(&changes_final_only),
            "duplicated key writes must reduce to final value"
        );
    }

    // ── Phase B acceptance tests ────────────────────────────────────

    /// B-1: Execution must reject a transaction with a forged signature.
    #[test]
    fn execution_should_reject_forged_signed_transaction() {
        let executor = BlockStmExecutor::new(ShardId(0), CommitSequence(1), TimestampMs::now());
        let alice = TestAccount::random();
        let mut state = MemStateView::new();
        state.set_balance(alice.address, 1_000_000);

        // Create a valid transaction, then tamper with the signature
        // by re-signing with a different key.
        let mut tx = make_transfer(&alice, recipient_addr(), 100, 0);
        let (evil_sk, _) = DilithiumSigner::generate_keypair();
        tx.signature = DilithiumSigner::sign(&evil_sk, TX_DOMAIN, tx.digest.as_bytes());

        let result = executor.execute(&[tx], &state).unwrap();
        assert_eq!(result.receipts.len(), 1);
        assert_eq!(
            result.receipts[0].status,
            ExecutionStatus::InvalidSignature,
            "forged signature must be rejected"
        );
    }

    /// B-1: Execution must reject a transaction where sender_pk doesn't
    /// derive to body.sender (sender binding mismatch).
    #[test]
    fn execution_should_reject_sender_pk_mismatch() {
        let executor = BlockStmExecutor::new(ShardId(0), CommitSequence(1), TimestampMs::now());
        let alice = TestAccount::random();
        let other = TestAccount::random();
        let mut state = MemStateView::new();
        state.set_balance(alice.address, 1_000_000);

        // Sign with alice's key but use a body claiming to be from alice.
        // Replace sender_pk with another key — signature still valid but
        // derived address won't match.
        let mut tx = make_transfer(&alice, recipient_addr(), 100, 0);
        tx.sender_pk = other.pk.clone();

        let result = executor.execute(&[tx], &state).unwrap();
        assert_eq!(result.receipts.len(), 1);
        // Should fail either as InvalidSignature (sig doesn't verify
        // with wrong pk) or SenderMismatch.
        assert!(
            matches!(
                result.receipts[0].status,
                ExecutionStatus::InvalidSignature | ExecutionStatus::SenderMismatch
            ),
            "sender-pk mismatch must be rejected, got {:?}",
            result.receipts[0].status
        );
    }

    /// B-2: Execution must reject a replayed sequence number.
    #[test]
    fn execution_should_reject_replayed_sequence_number() {
        let executor = BlockStmExecutor::new(ShardId(0), CommitSequence(1), TimestampMs::now());
        let alice = TestAccount::random();
        let mut state = MemStateView::new();
        state.set_balance(alice.address, 10_000_000);

        // Two transactions with the same nonce from the same sender.
        let tx0 = make_transfer(&alice, recipient_addr(), 100, 0);
        let tx1 = make_transfer(&alice, recipient_addr(), 200, 0); // replayed nonce

        let result = executor.execute(&[tx0, tx1], &state).unwrap();
        assert_eq!(result.receipts.len(), 2);
        assert_eq!(result.receipts[0].status, ExecutionStatus::Success);
        // Second tx should fail because nonce 0 was already consumed.
        assert!(
            matches!(
                result.receipts[1].status,
                ExecutionStatus::SequenceNumberMismatch {
                    expected: 1,
                    got: 0
                }
            ),
            "replayed nonce must be rejected, got {:?}",
            result.receipts[1].status
        );
    }

    /// B-3: Execution must reject an expired transaction.
    #[test]
    fn execution_should_reject_expired_transaction() {
        let mut executor = BlockStmExecutor::new(ShardId(0), CommitSequence(1), TimestampMs::now());
        executor.set_epoch(EpochNumber(5000));

        let alice = TestAccount::random();
        let mut state = MemStateView::new();
        state.set_balance(alice.address, 1_000_000);

        // make_transfer sets expiry_epoch to 1000, which is < 5000.
        let tx = make_transfer(&alice, recipient_addr(), 100, 0);
        let result = executor.execute(&[tx], &state).unwrap();

        assert_eq!(result.receipts.len(), 1);
        assert_eq!(
            result.receipts[0].status,
            ExecutionStatus::Expired,
            "expired transaction must be rejected"
        );
    }

    /// B-4: Execution must reject a transaction with the wrong chain_id.
    #[test]
    fn execution_should_reject_wrong_chain_id() {
        let mut executor = BlockStmExecutor::new(ShardId(0), CommitSequence(1), TimestampMs::now());
        executor.set_chain_id(99);

        let alice = TestAccount::random();
        let mut state = MemStateView::new();
        state.set_balance(alice.address, 1_000_000);

        // make_transfer uses chain_id: 1, but executor expects 99.
        let tx = make_transfer(&alice, recipient_addr(), 100, 0);
        let result = executor.execute(&[tx], &state).unwrap();

        assert_eq!(result.receipts.len(), 1);
        assert_eq!(
            result.receipts[0].status,
            ExecutionStatus::ChainIdMismatch,
            "wrong chain_id must be rejected"
        );
    }
}

// ── F-1: Block-STM property tests ──────────────────────────────────────

#[cfg(test)]
mod property_tests {
    use super::*;
    use crate::types::{compute_tx_digest, TransactionBody, TransactionPayload, TX_DOMAIN};
    use nexus_crypto::{DilithiumSigner, DilithiumSigningKey, DilithiumVerifyKey, Signer};
    use nexus_primitives::{Amount, EpochNumber, TokenId};
    use proptest::prelude::*;
    use std::collections::HashMap as StdHashMap;

    struct MemStateView {
        data: StdHashMap<(AccountAddress, Vec<u8>), Vec<u8>>,
    }

    impl MemStateView {
        fn new() -> Self {
            Self {
                data: StdHashMap::new(),
            }
        }

        fn set_balance(&mut self, addr: AccountAddress, balance: u64) {
            self.data
                .insert((addr, b"balance".to_vec()), balance.to_le_bytes().to_vec());
        }
    }

    impl StateView for MemStateView {
        fn get(
            &self,
            account: &AccountAddress,
            key: &[u8],
        ) -> crate::error::ExecutionResult<Option<Vec<u8>>> {
            Ok(self.data.get(&(*account, key.to_vec())).cloned())
        }
    }

    struct TestAccount {
        sk: DilithiumSigningKey,
        pk: DilithiumVerifyKey,
        address: AccountAddress,
    }

    impl TestAccount {
        fn random() -> Self {
            let (sk, pk) = DilithiumSigner::generate_keypair();
            let address = AccountAddress::from_dilithium_pubkey(pk.as_bytes());
            Self { sk, pk, address }
        }
    }

    fn make_transfer(
        sender: &TestAccount,
        recipient: AccountAddress,
        amount: u64,
        nonce: u64,
    ) -> crate::types::SignedTransaction {
        let body = TransactionBody {
            sender: sender.address,
            sequence_number: nonce,
            expiry_epoch: EpochNumber(1000),
            gas_limit: 50_000,
            gas_price: 1,
            target_shard: None,
            payload: TransactionPayload::Transfer {
                recipient,
                amount: Amount(amount),
                token: TokenId::Native,
            },
            chain_id: 1,
        };
        let digest = compute_tx_digest(&body).unwrap();
        let sig = DilithiumSigner::sign(&sender.sk, TX_DOMAIN, digest.as_bytes());
        crate::types::SignedTransaction {
            body,
            signature: sig,
            sender_pk: sender.pk.clone(),
            digest,
        }
    }

    // ── Property 1: parallel == serial for any valid batch ──────────

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(20))]

        /// For any batch of independent single-sender transfers, the
        /// parallel executor must produce the exact same state root and
        /// receipts as the serial reference executor.
        #[test]
        fn parallel_matches_serial_any_batch(
            sender_count in 2usize..=8,
            amount in 1u64..=10_000,
        ) {
            let executor = BlockStmExecutor::with_config(
                ShardId(0), CommitSequence(1), TimestampMs(1000), 5, 4,
            );
            let mut state = MemStateView::new();
            let senders: Vec<TestAccount> =
                (0..sender_count).map(|_| TestAccount::random()).collect();
            for s in &senders {
                state.set_balance(s.address, 1_000_000_000);
            }

            let txs: Vec<_> = senders
                .iter()
                .map(|s| make_transfer(s, AccountAddress([0xBB; 32]), amount, 0))
                .collect();

            let par = executor.execute(&txs, &state).unwrap();
            let ser = executor.execute_serial(&txs, &state).unwrap();

            prop_assert_eq!(par.new_state_root, ser.new_state_root);
            prop_assert_eq!(par.gas_used_total, ser.gas_used_total);
            prop_assert_eq!(par.receipts.len(), ser.receipts.len());
            for (i, (p, s)) in par.receipts.iter().zip(ser.receipts.iter()).enumerate() {
                prop_assert_eq!(&p.status, &s.status, "receipt {}: status", i);
                prop_assert_eq!(&p.state_changes, &s.state_changes, "receipt {}: changes", i);
            }
        }

        /// For conflicting transactions from the same sender, parallel
        /// and serial must still converge.
        #[test]
        fn parallel_matches_serial_conflicting(
            tx_count in 2usize..=6,
            amount in 1u64..=1_000,
        ) {
            let executor = BlockStmExecutor::with_config(
                ShardId(0), CommitSequence(1), TimestampMs(1000), 5, 2,
            );
            let alice = TestAccount::random();
            let mut state = MemStateView::new();
            state.set_balance(alice.address, 1_000_000_000);

            let txs: Vec<_> = (0..tx_count)
                .map(|i| make_transfer(&alice, AccountAddress([0xBB; 32]), amount, i as u64))
                .collect();

            let par = executor.execute(&txs, &state).unwrap();
            let ser = executor.execute_serial(&txs, &state).unwrap();

            prop_assert_eq!(par.new_state_root, ser.new_state_root);
            prop_assert_eq!(par.gas_used_total, ser.gas_used_total);
        }
    }

    // ── Property 2: state root determinism ─────────────────────────

    /// Thin wrapper so proptest-expanded code doesn't trigger the deprecation lint.
    #[allow(deprecated)]
    fn test_compute_state_root(changes: &[StateChange]) -> Blake3Digest {
        compute_state_root(changes)
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(30))]

        /// The state root function is pure: same input → same output.
        #[test]
        fn state_root_is_pure(
            n in 1usize..=20,
            seed in any::<u8>(),
        ) {
            let changes: Vec<StateChange> = (0..n)
                .map(|i| {
                    let mut addr = [0u8; 32];
                    addr[0] = seed;
                    addr[1] = i as u8;
                    StateChange {
                        account: AccountAddress(addr),
                        key: vec![i as u8],
                        value: Some(vec![seed, i as u8]),
                    }
                })
                .collect();

            let r1 = test_compute_state_root(&changes);
            let r2 = test_compute_state_root(&changes);
            prop_assert_eq!(r1, r2);
        }

        /// State root is independent of insertion order.
        #[test]
        fn state_root_order_independent(
            n in 2usize..=12,
            seed in any::<u8>(),
        ) {
            let mut changes: Vec<StateChange> = (0..n)
                .map(|i| {
                    let mut addr = [0u8; 32];
                    addr[0] = seed;
                    addr[1] = i as u8;
                    StateChange {
                        account: AccountAddress(addr),
                        key: vec![i as u8],
                        value: Some(vec![seed]),
                    }
                })
                .collect();

            let root_fwd = test_compute_state_root(&changes);
            changes.reverse();
            let root_rev = test_compute_state_root(&changes);
            prop_assert_eq!(root_fwd, root_rev);
        }

        /// Duplicate writes to the same key collapse to last-write-wins.
        #[test]
        fn state_root_dedup_last_write_wins(
            first_val in any::<u8>(),
            final_val in any::<u8>(),
        ) {
            let addr = AccountAddress([0x42; 32]);
            let with_dup = vec![
                StateChange { account: addr, key: b"k".to_vec(), value: Some(vec![first_val]) },
                StateChange { account: addr, key: b"k".to_vec(), value: Some(vec![final_val]) },
            ];
            let final_only = vec![
                StateChange { account: addr, key: b"k".to_vec(), value: Some(vec![final_val]) },
            ];
            prop_assert_eq!(
                test_compute_state_root(&with_dup),
                test_compute_state_root(&final_only),
            );
        }
    }

    // ── Property 3: execution never panics on empty/single ─────────

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(10))]

        /// Executing 0 or 1 transactions must always succeed.
        #[test]
        fn execute_never_panics_small_batch(use_single in proptest::bool::ANY) {
            let executor = BlockStmExecutor::new(
                ShardId(0), CommitSequence(1), TimestampMs(1000),
            );
            let state = MemStateView::new();

            if use_single {
                let alice = TestAccount::random();
                let tx = make_transfer(&alice, AccountAddress([0xBB; 32]), 1, 0);
                let _ = executor.execute(&[tx], &state);
            } else {
                let result = executor.execute(&[], &state).unwrap();
                prop_assert!(result.receipts.is_empty());
            }
        }
    }
}
