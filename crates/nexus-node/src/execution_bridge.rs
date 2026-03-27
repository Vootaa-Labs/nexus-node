// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Execution bridge — drains committed sub-DAGs from the consensus engine,
//! resolves the constituent transactions, feeds them to the execution service,
//! and persists the results (receipts + state changes) to storage.
//!
//! This is the critical link that makes the pipeline live:
//!
//! ```text
//! ConsensusEngine                   ExecutionService
//!    (committed buffer)                (Block-STM)
//!        │                                 ▲
//!        └─── ExecutionBridge ─────────────┘
//!                    │
//!                    ├─ resolve certs → txs (via BatchStore)
//!                    ├─ submit to execution
//!                    ├─ persist receipts → cf_receipts
//!                    ├─ persist state changes → cf_state
//!                    └─ update commit_seq + emit WS events
//! ```

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nexus_consensus::{CommittedBatch, ConsensusEngine, EpochManager};
use nexus_execution::service::ExecutionServiceHandle;
use nexus_execution::types::{SignedTransaction, TransactionPayload};
use nexus_intent::{AnchorReceipt, RocksProvenanceStore};
use nexus_primitives::{Blake3Digest, EpochNumber, ShardId, TimestampMs};
use nexus_storage::canonical_empty_root;
use nexus_storage::traits::{StateStorage, WriteBatchOps};
use nexus_storage::ColumnFamily;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use crate::batch_store::BatchStore;
use crate::commitment_tracker::SharedCommitmentTracker;
use crate::node_metrics;
use crate::readiness::SubsystemHandle;
use crate::staking_snapshot::{
    CommitteeRotationPolicy, PersistedElectionResult, RotationOutcome, StakingSnapshot,
};

use crate::backends::SharedChainHead;

/// Maximum number of execution retries before a batch is sent to the
/// dead-letter queue.
const MAX_EXEC_RETRIES: u32 = 3;

/// Maximum number of entries allowed in the dead-letter queue before the
/// bridge halts processing and raises a critical alert.
const MAX_DEAD_LETTER_ENTRIES: usize = 64;

/// A committed batch that failed execution and is awaiting retry.
struct RetryEntry {
    batch: CommittedBatch,
    transactions: Vec<SignedTransaction>,
    anchor_txs: Vec<(Blake3Digest, u64, u32, Blake3Digest)>,
    cert_count: usize,
    retry_count: u32,
}

fn emit_bridge_events(
    events_tx: &Option<broadcast::Sender<nexus_rpc::ws::NodeEvent>>,
    sequence: u64,
    cert_count: usize,
    committed_at_ms: u64,
    receipts: &[nexus_execution::types::TransactionReceipt],
    consensus_status: &nexus_rpc::dto::ConsensusStatusDto,
) {
    if let Some(tx) = events_tx {
        let _ = tx.send(nexus_rpc::ws::NodeEvent::NewCommit {
            sequence,
            certificate_count: cert_count,
            committed_at_ms,
        });

        for receipt in receipts {
            let dto: nexus_rpc::dto::TransactionReceiptDto = receipt.clone().into();
            let _ = tx.send(nexus_rpc::ws::NodeEvent::TransactionExecuted(dto));
        }

        let _ = tx.send(nexus_rpc::ws::NodeEvent::ConsensusStatus(
            consensus_status.clone(),
        ));
    }
}

fn update_chain_head_snapshot(
    chain_head: &SharedChainHead,
    batch: &CommittedBatch,
    consensus_status: &nexus_rpc::dto::ConsensusStatusDto,
    cert_count: usize,
    tx_count: usize,
    gas_total: u64,
    state_root: Blake3Digest,
) {
    chain_head.update(nexus_rpc::dto::ChainHeadDto {
        sequence: batch.sequence.0,
        anchor_digest: hex::encode(batch.anchor.0),
        state_root: hex::encode(state_root.0),
        epoch: consensus_status.epoch.0,
        round: 0,
        cert_count,
        tx_count,
        gas_total,
        committed_at_ms: batch.committed_at.0,
    });
}

/// Default polling interval for checking committed batches.
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(50);

// ── Context structs (D-1: converge long parameter chains) ────────────────────

/// Shared execution pipeline infrastructure passed to the execution bridge.
///
/// Groups the per-shard dispatch, storage, sequencing, event emission, and
/// commitment tracking dependencies that were previously 10+ positional
/// parameters.
pub struct BridgeContext<S: StateStorage> {
    /// Per-shard execution dispatch.
    pub shard_router: ShardRouter,
    /// Persistent batch digest → signed transaction mapping.
    pub batch_store: Arc<BatchStore>,
    /// Storage backend.
    pub store: S,
    /// Global commit sequence counter.
    pub commit_seq: Arc<AtomicU64>,
    /// Optional WebSocket event broadcast channel.
    pub events_tx: Option<broadcast::Sender<nexus_rpc::ws::NodeEvent>>,
    /// Per-shard chain head state.
    pub shard_chain_heads: ShardChainHeads,
    /// Optional intent-provenance store.
    pub provenance_store: Option<Arc<RocksProvenanceStore<S>>>,
    /// Optional commitment-tree tracker.
    pub commitment_tracker: Option<SharedCommitmentTracker>,
}

/// Epoch lifecycle dependencies for the execution bridge.
pub struct EpochContext {
    /// Epoch transition manager (commit-based / time-based triggers).
    pub epoch_manager: Option<Arc<Mutex<EpochManager>>>,
    /// Shared epoch counter exposed to RPC.
    pub epoch_counter: Option<Arc<AtomicU64>>,
    /// Committee rotation policy (election interval, strategy).
    pub rotation_policy: Option<CommitteeRotationPolicy>,
    /// Provides a staking snapshot for committee election.
    pub staking_snapshot_provider: Option<Arc<dyn Fn() -> Option<StakingSnapshot> + Send + Sync>>,
}

// ── ShardRouter ──────────────────────────────────────────────────────────────

/// Holds per-shard execution service handles for multi-shard dispatch.
///
/// The router resolves a transaction's `target_shard` field to the
/// corresponding [`ExecutionServiceHandle`]. Transactions without an
/// explicit target are routed to `ShardId(0)`.
#[derive(Clone)]
pub struct ShardRouter {
    handles: HashMap<ShardId, ExecutionServiceHandle>,
}

impl ShardRouter {
    /// Create a new router from a map of shard handles.
    pub fn new(handles: HashMap<ShardId, ExecutionServiceHandle>) -> Self {
        Self { handles }
    }

    /// Create a single-shard router (backward-compatible with v0.1.9).
    pub fn single(handle: ExecutionServiceHandle) -> Self {
        let mut handles = HashMap::new();
        handles.insert(ShardId(0), handle);
        Self { handles }
    }

    /// Resolve the execution handle for a given shard.
    pub fn get(&self, shard_id: &ShardId) -> Option<&ExecutionServiceHandle> {
        self.handles.get(shard_id)
    }

    /// Number of shards in this router.
    pub fn num_shards(&self) -> u16 {
        self.handles.len() as u16
    }

    /// Resolve the target shard for a transaction.
    /// Returns `ShardId(0)` if `target_shard` is None.
    pub fn resolve_shard(target: Option<ShardId>) -> ShardId {
        target.unwrap_or(ShardId(0))
    }
}

/// Per-shard chain head map, keyed by `ShardId`.
pub type ShardChainHeads = HashMap<ShardId, SharedChainHead>;

/// Configuration for the execution bridge.
#[derive(Debug, Clone)]
pub struct ExecutionBridgeConfig {
    /// How often to poll the consensus engine for committed batches.
    pub poll_interval: Duration,
    /// Number of shards this bridge manages.
    pub num_shards: u16,
}

impl Default for ExecutionBridgeConfig {
    fn default() -> Self {
        Self {
            poll_interval: DEFAULT_POLL_INTERVAL,
            num_shards: 1,
        }
    }
}

/// Spawn the execution bridge background task.
///
/// The bridge runs a periodic loop:
/// 1. Lock the consensus engine and drain committed sub-DAGs
/// 2. For each committed batch, resolve transactions from the batch store
/// 3. Group transactions by `target_shard` and route to the correct
///    shard execution service via the [`ShardRouter`]
/// 4. Persist receipts and state changes to storage (per-shard)
/// 5. Update per-shard chain heads and the global commit sequence counter
/// 6. Emit WebSocket events
///
/// When `rotation_policy` is provided the bridge will attempt committee
/// election at configured epoch boundaries instead of cloning the current
/// committee.  The `staking_snapshot_provider` supplies a
/// `StakingSnapshot` from canonical committed state.
pub fn spawn_execution_bridge<S: StateStorage>(
    config: ExecutionBridgeConfig,
    engine: Arc<Mutex<ConsensusEngine>>,
    ctx: BridgeContext<S>,
    epoch_ctx: EpochContext,
    readiness_handle: SubsystemHandle,
) -> JoinHandle<()> {
    let EpochContext {
        epoch_manager,
        epoch_counter,
        rotation_policy,
        staking_snapshot_provider,
    } = epoch_ctx;

    tokio::spawn(async move {
        debug!(
            poll_ms = config.poll_interval.as_millis() as u64,
            num_shards = config.num_shards,
            "execution bridge started"
        );

        let mut retry_queue: VecDeque<RetryEntry> = VecDeque::new();
        let mut dead_letter_count: usize = 0;

        loop {
            tokio::time::sleep(config.poll_interval).await;

            // ── 0. Process retry queue before draining new batches ────
            let retry_len = retry_queue.len();
            let mut still_failing: VecDeque<RetryEntry> = VecDeque::new();
            while let Some(mut entry) = retry_queue.pop_front() {
                let consensus_status = {
                    let eng = match engine.lock() {
                        Ok(g) => g,
                        Err(_) => {
                            still_failing.push_back(entry);
                            continue;
                        }
                    };
                    nexus_rpc::dto::ConsensusStatusDto {
                        epoch: eng.epoch(),
                        dag_size: eng.dag_size(),
                        total_commits: eng.total_commits(),
                        pending_commits: eng.pending_commits(),
                    }
                };

                match execute_resolved_batch(
                    &entry.batch,
                    entry.transactions.clone(),
                    &entry.anchor_txs,
                    entry.cert_count,
                    &ctx,
                    &consensus_status,
                    None, // no DAG needed for GC on retry
                )
                .await
                {
                    Ok(()) => {
                        info!(
                            sequence = entry.batch.sequence.0,
                            retries = entry.retry_count,
                            "execution bridge: retry succeeded"
                        );
                    }
                    Err(e) => {
                        entry.retry_count += 1;
                        if entry.retry_count >= MAX_EXEC_RETRIES {
                            error!(
                                sequence = entry.batch.sequence.0,
                                retries = entry.retry_count,
                                error = %e,
                                "execution bridge: batch moved to dead-letter after max retries"
                            );
                            persist_dead_letter(&ctx.store, &entry.batch, &e.to_string()).await;
                            dead_letter_count += 1;
                            metrics::counter!("nexus_bridge_dead_letter_total").increment(1);

                            if dead_letter_count >= MAX_DEAD_LETTER_ENTRIES {
                                error!(
                                    dead_letters = dead_letter_count,
                                    "execution bridge: CRITICAL — dead-letter queue limit reached, HALTING bridge"
                                );
                                metrics::gauge!("nexus_bridge_halted").set(1.0);
                                // Circuit-breaker: stop the bridge loop so the
                                // node becomes visibly un-serviceable rather
                                // than silently dropping batches.
                                return;
                            }
                        } else {
                            warn!(
                                sequence = entry.batch.sequence.0,
                                retry = entry.retry_count,
                                error = %e,
                                "execution bridge: scheduling batch for retry"
                            );
                            metrics::counter!("nexus_bridge_retry_total").increment(1);
                            still_failing.push_back(entry);
                        }
                    }
                }
            }
            retry_queue = still_failing;
            if retry_len > 0 && !retry_queue.is_empty() {
                debug!(
                    pending_retries = retry_queue.len(),
                    "execution bridge: retry queue status"
                );
            }

            // ── 1. Drain committed batches from consensus engine ─────
            let batches = {
                let mut eng = match engine.lock() {
                    Ok(g) => g,
                    Err(_) => {
                        warn!("execution bridge: engine lock poisoned");
                        continue;
                    }
                };
                let committed = eng.take_committed();
                if committed.is_empty() {
                    continue;
                }

                info!(
                    count = committed.len(),
                    "execution bridge: drained committed batches"
                );
                // Also capture the DAG for cert→batch resolution
                let dag = eng.dag().clone();
                let consensus_status = nexus_rpc::dto::ConsensusStatusDto {
                    epoch: eng.epoch(),
                    dag_size: eng.dag_size(),
                    total_commits: eng.total_commits(),
                    pending_commits: eng.pending_commits(),
                };
                (committed, dag, consensus_status)
            };

            let (committed_batches, dag, consensus_status) = batches;

            for batch in committed_batches {
                process_committed_batch(&batch, &dag, &ctx, &consensus_status, &mut retry_queue)
                    .await;
            }
            readiness_handle.report_progress();

            // ── Epoch boundary check ─────────────────────────────────
            // After processing all committed batches, evaluate whether
            // an epoch transition should occur.  If so, run the
            // commitment tracker's cross-tree consistency check and
            // advance the epoch in the consensus engine.
            if let Some(ref mgr) = epoch_manager {
                if let Ok(mut epoch_mgr) = mgr.lock() {
                    if let Some(trigger) = epoch_mgr.should_advance(consensus_status.total_commits)
                    {
                        // 1. Run commitment tracker consistency check.
                        let consistency_ok = if let Some(ref tracker) = ctx.commitment_tracker {
                            match tracker.write() {
                                Ok(mut ct) => match ct.epoch_boundary_check() {
                                    Ok(()) => {
                                        info!(
                                            epoch = consensus_status.epoch.0,
                                            "epoch boundary: commitment consistency check passed"
                                        );
                                        true
                                    }
                                    Err(e) => {
                                        error!(
                                            epoch = consensus_status.epoch.0,
                                            error = %e,
                                            "epoch boundary: CRITICAL — commitment consistency check FAILED, halting epoch advance"
                                        );
                                        metrics::gauge!("nexus_epoch_consistency_failure").set(1.0);
                                        false
                                    }
                                },
                                Err(_) => {
                                    warn!("epoch boundary: commitment tracker lock poisoned");
                                    false
                                }
                            }
                        } else {
                            true // no tracker — skip check
                        };

                        if consistency_ok {
                            // 2. Determine the next committee.
                            //
                            // If a rotation policy is configured, attempt
                            // election from the staking snapshot at election
                            // boundaries.  Otherwise fall back to cloning
                            // the current committee (legacy path).
                            if let Ok(mut eng) = engine.lock() {
                                let next_epoch = EpochNumber(eng.epoch().0 + 1);
                                let (committee, persisted_election) = resolve_next_committee(
                                    &eng,
                                    next_epoch,
                                    &rotation_policy,
                                    &staking_snapshot_provider,
                                );

                                let (transition, _flushed) =
                                    eng.advance_epoch(committee.clone(), trigger);

                                // 3. Persist the transition to storage
                                // (including election result when available).
                                let persist_res = if let Some(ref er) = persisted_election {
                                    crate::epoch_store::persist_epoch_transition_with_election(
                                        &ctx.store,
                                        &committee,
                                        &transition,
                                        Some(er),
                                    )
                                } else {
                                    crate::epoch_store::persist_epoch_transition(
                                        &ctx.store,
                                        &committee,
                                        &transition,
                                    )
                                };
                                if let Err(e) = persist_res {
                                    metrics::counter!("nexus_epoch_persist_failure_total")
                                        .increment(1);
                                    error!(
                                        epoch = transition.to_epoch.0,
                                        error = %e,
                                        "epoch boundary: failed to persist transition — \
                                         in-memory state has advanced but durable state has not; \
                                         a crash before the next successful persist will cause rollback"
                                    );
                                }

                                // 4. Update the shared epoch counter.
                                if let Some(ref counter) = epoch_counter {
                                    counter.store(transition.to_epoch.0, Ordering::Release);
                                }

                                info!(
                                    from_epoch = transition.from_epoch.0,
                                    to_epoch = transition.to_epoch.0,
                                    trigger = ?trigger,
                                    "epoch boundary: advanced epoch"
                                );

                                // 5b. Emit ConsensusStatus WS event.
                                if let Some(ref tx) = ctx.events_tx {
                                    let status = nexus_rpc::dto::ConsensusStatusDto {
                                        epoch: transition.to_epoch,
                                        dag_size: eng.dag_size(),
                                        total_commits: eng.total_commits(),
                                        pending_commits: eng.pending_commits(),
                                    };
                                    let _ =
                                        tx.send(nexus_rpc::ws::NodeEvent::ConsensusStatus(status));
                                }

                                // 6. Record transition in the manager.
                                epoch_mgr.record_transition(transition);
                            }
                        }
                    }
                }
            }
        }
    })
}

// ── Committee resolution ─────────────────────────────────────────────────────

/// Decide which committee should govern the next epoch.
///
/// If a `CommitteeRotationPolicy` is active and the next epoch is an
/// election boundary, attempts election from the staking snapshot.
/// On election failure (or if no policy / snapshot is available), falls
/// back to cloning the current committee.
///
/// Returns `(committee, Option<PersistedElectionResult>)`.
fn resolve_next_committee(
    eng: &ConsensusEngine,
    next_epoch: EpochNumber,
    rotation_policy: &Option<CommitteeRotationPolicy>,
    snapshot_provider: &Option<Arc<dyn Fn() -> Option<StakingSnapshot> + Send + Sync>>,
) -> (nexus_consensus::Committee, Option<PersistedElectionResult>) {
    let policy = match rotation_policy {
        Some(p) => p,
        None => {
            // Legacy path: no rotation policy — clone current committee.
            return (eng.committee().clone(), None);
        }
    };

    let snapshot = snapshot_provider.as_ref().and_then(|provider| provider());

    match crate::staking_snapshot::attempt_rotation(snapshot.as_ref(), policy, next_epoch) {
        RotationOutcome::Elected(result) => {
            info!(
                epoch = next_epoch.0,
                elected = result.elected.len(),
                total_stake = result.total_effective_stake,
                "epoch boundary: committee elected from staking snapshot"
            );
            metrics::counter!("nexus_epoch_election_total").increment(1);

            // Convert election result to committee using key lookup from
            // current committee. Keys are mapped from deterministic
            // validator-index-based addresses (the canonical mapping used by
            // the identity registry and the snapshot provider).
            let current = eng.committee();
            let key_map: std::collections::HashMap<_, _> = current
                .all_validators()
                .iter()
                .map(|v| {
                    let addr = crate::validator_identity::address_from_validator_index(v.index);
                    (addr, v.falcon_pub_key.clone())
                })
                .collect();

            // Try to build committee from election; fall back on failure.
            match crate::staking_snapshot::election_to_committee(&result, &|addr| {
                key_map.get(addr).cloned()
            }) {
                Ok(committee) => {
                    let persisted = PersistedElectionResult::from(&result);
                    (committee, Some(persisted))
                }
                Err(e) => {
                    warn!(
                        epoch = next_epoch.0,
                        error = %e,
                        "epoch boundary: election-to-committee failed, falling back to current"
                    );
                    metrics::counter!("nexus_epoch_election_fallback_total").increment(1);
                    (eng.committee().clone(), None)
                }
            }
        }
        RotationOutcome::NotElectionEpoch => {
            debug!(
                epoch = next_epoch.0,
                interval = policy.election_epoch_interval,
                "epoch boundary: not an election epoch, carrying forward committee"
            );
            (eng.committee().clone(), None)
        }
        RotationOutcome::Fallback { reason } => {
            warn!(
                epoch = next_epoch.0,
                error = %reason,
                "epoch boundary: election failed, carrying forward current committee as safety fallback"
            );
            metrics::counter!("nexus_epoch_election_fallback_total").increment(1);
            (eng.committee().clone(), None)
        }
    }
}

/// Process a single committed sub-DAG through the execution pipeline.
///
/// Resolves transactions, groups them by `target_shard`, submits each
/// shard group to the appropriate execution service via the [`ShardRouter`],
/// and on failure enqueues the batch into the retry queue instead of
/// silently dropping it.
async fn process_committed_batch<S: StateStorage>(
    batch: &CommittedBatch,
    dag: &nexus_consensus::InMemoryDag,
    ctx: &BridgeContext<S>,
    consensus_status: &nexus_rpc::dto::ConsensusStatusDto,
    retry_queue: &mut VecDeque<RetryEntry>,
) {
    let sequence = batch.sequence;
    let cert_count = batch.certificates.len();

    // 2. Resolve committed certificates → transactions (deduplicated)
    let mut transactions: Vec<SignedTransaction> = Vec::new();
    let mut seen_digests = std::collections::HashSet::new();

    for cert_digest in &batch.certificates {
        if let Some(cert) = dag.get_by_digest(cert_digest) {
            if let Some(txs) = ctx.batch_store.get(&cert.batch_digest) {
                for tx in txs {
                    if seen_digests.insert(tx.digest) {
                        transactions.push(tx);
                    }
                }
            } else {
                debug!(
                    batch_digest = %cert.batch_digest,
                    "execution bridge: batch not found in store (from remote validator)"
                );
            }
        } else {
            warn!(
                cert_digest = %cert_digest,
                "execution bridge: certificate not found in DAG"
            );
        }
    }

    if transactions.is_empty() {
        debug!(
            sequence = sequence.0,
            "execution bridge: no transactions to execute for committed batch"
        );
        let state_root = if let Some(tracker) = &ctx.commitment_tracker {
            tracker
                .read()
                .ok()
                .map(|ct| ct.commitment_root())
                .unwrap_or_else(canonical_empty_root)
        } else {
            canonical_empty_root()
        };

        ctx.commit_seq.store(sequence.0, Ordering::Release);
        // Update shard 0 chain head for empty batch.
        if let Some(chain_head) = ctx.shard_chain_heads.get(&ShardId(0)) {
            update_chain_head_snapshot(
                chain_head,
                batch,
                consensus_status,
                cert_count,
                0,
                0,
                state_root,
            );
        }
        emit_bridge_events(
            &ctx.events_tx,
            sequence.0,
            cert_count,
            batch.committed_at.0,
            &[],
            consensus_status,
        );
        return;
    }

    info!(
        sequence = sequence.0,
        certs = cert_count,
        txs = transactions.len(),
        "execution bridge: submitting batch to execution"
    );

    // Pre-collect ProvenanceAnchor metadata for receipt backfill.
    let anchor_txs: Vec<(Blake3Digest, u64, u32, Blake3Digest)> = transactions
        .iter()
        .filter_map(|tx| match &tx.body.payload {
            TransactionPayload::ProvenanceAnchor {
                anchor_digest,
                batch_seq,
                record_count,
            } => Some((*anchor_digest, *batch_seq, *record_count, tx.digest)),
            _ => None,
        })
        .collect();

    // 3. Execute and persist — on failure, enqueue for retry.
    if let Err(e) = execute_resolved_batch(
        batch,
        transactions.clone(),
        &anchor_txs,
        cert_count,
        ctx,
        consensus_status,
        Some(dag),
    )
    .await
    {
        warn!(
            sequence = sequence.0,
            error = %e,
            "execution bridge: scheduling failed batch for retry"
        );
        metrics::counter!("nexus_bridge_retry_total").increment(1);

        retry_queue.push_back(RetryEntry {
            batch: batch.clone(),
            transactions,
            anchor_txs,
            cert_count,
            retry_count: 1,
        });
    }
}

/// Execute a resolved batch of transactions across multiple shards and persist results.
///
/// Transactions are grouped by `target_shard` and executed on the
/// corresponding shard's [`ExecutionServiceHandle`] via the [`ShardRouter`].
/// Shard groups are processed in deterministic order (ascending shard index).
///
/// Returns `Ok(())` on success or an error string on failure so the caller
/// can decide whether to retry or dead-letter.
async fn execute_resolved_batch<S: StateStorage>(
    batch: &CommittedBatch,
    transactions: Vec<SignedTransaction>,
    anchor_txs: &[(Blake3Digest, u64, u32, Blake3Digest)],
    cert_count: usize,
    ctx: &BridgeContext<S>,
    consensus_status: &nexus_rpc::dto::ConsensusStatusDto,
    dag: Option<&nexus_consensus::InMemoryDag>,
) -> Result<(), String> {
    let sequence = batch.sequence;

    // ── 1. Group transactions by target shard ────────────────────────
    let mut shard_groups: HashMap<ShardId, Vec<SignedTransaction>> = HashMap::new();
    for tx in transactions {
        let shard_id = ShardRouter::resolve_shard(tx.body.target_shard);
        shard_groups.entry(shard_id).or_default().push(tx);
    }

    // Sort shard IDs for deterministic execution order.
    let mut shard_ids: Vec<ShardId> = shard_groups.keys().copied().collect();
    shard_ids.sort_by_key(|s| s.0);

    // ── 2. Execute each shard group ──────────────────────────────────
    let mut all_receipts: Vec<nexus_execution::types::TransactionReceipt> = Vec::new();
    let mut total_gas = 0u64;
    let mut total_tx_count = 0usize;

    for shard_id in &shard_ids {
        let shard_txs = shard_groups.remove(shard_id).unwrap_or_default();
        let shard_tx_count = shard_txs.len();

        let exec_handle = ctx
            .shard_router
            .get(shard_id)
            .ok_or_else(|| format!("no execution service for shard {}", shard_id.0))?;

        // Execute on the shard's execution service.
        let exec_start = Instant::now();
        let block_result = exec_handle
            .submit_batch(batch.clone(), shard_txs)
            .await
            .map_err(|e| format!("execution failed for shard {}: {e}", shard_id.0))?;

        let exec_elapsed = exec_start.elapsed().as_secs_f64();
        node_metrics::bridge_batch_executed(exec_elapsed);

        info!(
            sequence = sequence.0,
            shard = shard_id.0,
            gas = block_result.gas_used_total,
            receipts = block_result.receipts.len(),
            exec_ms = block_result.execution_ms,
            "execution bridge: shard batch executed"
        );

        // Persist receipts and state changes with the correct shard_id.
        let persist_start = Instant::now();
        persist_results(&ctx.store, &block_result, *shard_id)
            .await
            .map_err(|e| format!("persist failed for shard {}: {e}", shard_id.0))?;
        node_metrics::bridge_persist_latency(persist_start.elapsed().as_secs_f64());

        // Feed state changes into the commitment tracker (keys are
        // already shard-prefixed, so a single tracker handles all shards).
        let shard_state_root = update_commitment_tracker(
            &ctx.commitment_tracker,
            &block_result,
            *shard_id,
            sequence.0,
        )?;

        // Update per-shard chain head.
        if let Some(chain_head) = ctx.shard_chain_heads.get(shard_id) {
            update_chain_head_snapshot(
                chain_head,
                batch,
                consensus_status,
                cert_count,
                shard_tx_count,
                block_result.gas_used_total,
                shard_state_root,
            );
        }

        all_receipts.extend(block_result.receipts);
        total_gas += block_result.gas_used_total;
        total_tx_count += shard_tx_count;
    }

    info!(
        sequence = sequence.0,
        shards = shard_ids.len(),
        total_txs = total_tx_count,
        total_gas,
        "execution bridge: all shard groups executed"
    );

    // ── 3. Backfill anchor receipts ──────────────────────────────────
    if let Some(prov_store) = &ctx.provenance_store {
        if let Err(e) = backfill_anchor_receipts(
            prov_store,
            anchor_txs,
            &all_receipts,
            sequence.0,
            batch.committed_at,
        )
        .await
        {
            warn!(
                sequence = sequence.0,
                error = %e,
                "execution bridge: anchor backfill failed (non-fatal)"
            );
        }
    }

    // ── 4. Update global commit sequence (once per batch) ────────────
    ctx.commit_seq.store(sequence.0, Ordering::Release);

    // ── 5. Emit combined WebSocket events ────────────────────────────
    emit_bridge_events(
        &ctx.events_tx,
        sequence.0,
        cert_count,
        batch.committed_at.0,
        &all_receipts,
        consensus_status,
    );

    // ── 6. GC batch store entries ────────────────────────────────────
    if let Some(dag) = dag {
        for cert_digest in &batch.certificates {
            if let Some(cert) = dag.get_by_digest(cert_digest) {
                ctx.batch_store.remove(&cert.batch_digest);
            }
        }
    }

    Ok(())
}

/// Feed state changes from a shard's execution result into the commitment
/// tracker and return the resulting state root.
fn update_commitment_tracker(
    commitment_tracker: &Option<SharedCommitmentTracker>,
    block_result: &nexus_execution::types::BlockExecutionResult,
    shard_id: ShardId,
    sequence: u64,
) -> Result<Blake3Digest, String> {
    let Some(tracker) = commitment_tracker else {
        return Ok(canonical_empty_root());
    };

    let mut ct = tracker
        .write()
        .map_err(|_| "commitment tracker: lock poisoned".to_string())?;

    let keyed_changes: Vec<(Vec<u8>, Option<Vec<u8>>)> = block_result
        .receipts
        .iter()
        .flat_map(|r| &r.state_changes)
        .map(|change| {
            let mut key = nexus_storage::AccountKey {
                shard_id,
                address: change.account,
            }
            .to_bytes();
            key.extend_from_slice(&change.key);
            (key, change.value.clone())
        })
        .collect();

    let entries: Vec<crate::commitment_tracker::StateChangeEntry<'_>> = keyed_changes
        .iter()
        .map(|(k, v)| crate::commitment_tracker::StateChangeEntry {
            key: k.as_slice(),
            value: v.as_deref(),
        })
        .collect();

    ct.try_apply_state_changes(&entries).map_err(|e| {
        format!(
            "commitment persistence failed for shard {}: {e}",
            shard_id.0
        )
    })?;

    let root = ct.commitment_root();
    debug!(
        sequence,
        shard = shard_id.0,
        commitment_root = %hex::encode(root.0),
        entries = ct.entry_count(),
        "commitment tracker: updated after shard block"
    );
    Ok(root)
}

/// Persist a dead-letter record for a batch that exhausted all retries.
async fn persist_dead_letter<S: StateStorage>(store: &S, batch: &CommittedBatch, error_msg: &str) {
    let record = serde_json::json!({
        "sequence": batch.sequence.0,
        "anchor": hex::encode(batch.anchor.0),
        "committed_at_ms": batch.committed_at.0,
        "cert_count": batch.certificates.len(),
        "error": error_msg,
        "dead_letter_at_ms": TimestampMs::now().0,
    });

    let key = {
        let mut k = b"dead_letter:".to_vec();
        k.extend_from_slice(&batch.sequence.0.to_be_bytes());
        k
    };

    let value = match serde_json::to_vec(&record) {
        Ok(v) => v,
        Err(e) => {
            error!(sequence = batch.sequence.0, error = %e, "failed to serialize dead-letter record");
            return;
        }
    };

    let mut wb = store.new_batch();
    wb.put_cf(ColumnFamily::Blocks.as_str(), key, value);
    if let Err(e) = store.write_batch(wb).await {
        // Fail-fast: losing both the batch *and* the dead-letter record
        // means permanent silent data loss.  Halt the node so operators
        // can investigate storage health before restarting.
        panic!(
            "CRITICAL: failed to persist dead-letter record for seq {} — \
             committed batch is irrecoverably lost: {e}",
            batch.sequence.0
        );
    }
}

/// Persist execution results (receipts + state changes) to storage.
async fn persist_results<S: StateStorage>(
    store: &S,
    result: &nexus_execution::types::BlockExecutionResult,
    shard_id: ShardId,
) -> Result<(), nexus_storage::StorageError> {
    let mut batch = store.new_batch();

    for receipt in &result.receipts {
        // Write receipt to cf_receipts: key = tx_digest bytes, value = JSON
        let receipt_bytes = serde_json::to_vec(receipt).map_err(|e| {
            nexus_storage::StorageError::Serialization(format!("receipt serialization error: {e}"))
        })?;
        batch.put_cf(
            ColumnFamily::Receipts.as_str(),
            receipt.tx_digest.0.to_vec(),
            receipt_bytes,
        );

        // Apply state changes to cf_state.
        // The storage key is the composite `AccountKey ‖ change.key` so that
        // different state entries for the same account (balance, code,
        // resources) occupy distinct keys — matching the read path in
        // `StorageStateView::get`.
        for change in &receipt.state_changes {
            let mut key = nexus_storage::AccountKey {
                shard_id,
                address: change.account,
            }
            .to_bytes();
            key.extend_from_slice(&change.key);

            match &change.value {
                Some(value) => {
                    batch.put_cf(ColumnFamily::State.as_str(), key, value.clone());
                }
                None => {
                    batch.delete_cf(ColumnFamily::State.as_str(), key);
                }
            }

            // Mirror HTLC lock records to cf_htlc_locks for efficient queries.
            // HTLC state change keys start with "htlc_lock_v1:" under the
            // HTLC system account ([0x01; 32]).
            if change.account.0 == [0x01u8; 32] && change.key.starts_with(b"htlc_lock_v1:") {
                // Key in cf_htlc_locks is the lock_digest (32 bytes after prefix).
                let lock_digest_bytes = &change.key[b"htlc_lock_v1:".len()..];
                if lock_digest_bytes.len() == 32 {
                    match &change.value {
                        Some(value) => {
                            batch.put_cf(
                                ColumnFamily::HtlcLocks.as_str(),
                                lock_digest_bytes.to_vec(),
                                value.clone(),
                            );
                        }
                        None => {
                            batch.delete_cf(
                                ColumnFamily::HtlcLocks.as_str(),
                                lock_digest_bytes.to_vec(),
                            );
                        }
                    }
                }
            }
        }
    }

    store.write_batch(batch).await
}

/// Backfill anchor receipts for any `ProvenanceAnchor` transactions that
/// executed successfully in this batch.
async fn backfill_anchor_receipts<S: StateStorage>(
    prov_store: &RocksProvenanceStore<S>,
    anchor_txs: &[(Blake3Digest, u64, u32, Blake3Digest)],
    receipts: &[nexus_execution::types::TransactionReceipt],
    block_height: u64,
    committed_at: TimestampMs,
) -> Result<(), String> {
    use nexus_execution::types::ExecutionStatus;

    let mut first_error: Option<String> = None;

    for &(anchor_digest, batch_seq, record_count, tx_digest) in anchor_txs {
        // Find the matching receipt and confirm success.
        let ok = receipts
            .iter()
            .any(|r| r.tx_digest == tx_digest && r.status == ExecutionStatus::Success);
        if !ok {
            warn!(
                batch_seq,
                anchor = %anchor_digest,
                "anchor backfill: receipt not found or execution failed"
            );
            continue;
        }

        let receipt = AnchorReceipt {
            batch_seq,
            anchor_digest,
            tx_hash: tx_digest,
            block_height,
            anchored_at_ms: committed_at,
        };

        if let Err(e) = prov_store.store_anchor_receipt(&receipt).await {
            let msg =
                format!("anchor backfill: failed to store receipt for batch {batch_seq}: {e}");
            error!(batch_seq, error = %e, "anchor backfill: failed to store anchor receipt");
            if first_error.is_none() {
                first_error = Some(msg);
            }
            continue;
        }

        // Update the watermark so the anchor batch task knows how far we've anchored.
        if let Err(e) = prov_store
            .update_anchor_metadata(batch_seq, record_count as u64)
            .await
        {
            let msg =
                format!("anchor backfill: failed to update metadata for batch {batch_seq}: {e}");
            error!(batch_seq, error = %e, "anchor backfill: failed to update anchor metadata");
            if first_error.is_none() {
                first_error = Some(msg);
            }
        }

        info!(
            batch_seq,
            records = record_count,
            block_height,
            "anchor backfill: stored AnchorReceipt"
        );
    }

    match first_error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_execution::types::{
        BlockExecutionResult, ExecutionStatus, StateChange, TransactionReceipt,
    };
    use nexus_primitives::{AccountAddress, Blake3Digest, CommitSequence, ShardId, TimestampMs};
    use nexus_storage::traits::StateStorage;
    use nexus_storage::MemoryStore;

    fn make_receipt(digest_seed: u8, seq: u64) -> TransactionReceipt {
        TransactionReceipt {
            tx_digest: Blake3Digest([digest_seed; 32]),
            commit_seq: CommitSequence(seq),
            shard_id: ShardId(0),
            status: ExecutionStatus::Success,
            gas_used: 100,
            state_changes: vec![StateChange {
                account: AccountAddress([digest_seed; 32]),
                key: b"balance".to_vec(),
                value: Some(1000u64.to_le_bytes().to_vec()),
            }],
            timestamp: TimestampMs::now(),
        }
    }

    #[tokio::test]
    async fn persist_results_stores_receipts() {
        let store = MemoryStore::new();
        let shard_id = ShardId(0);

        let receipt = make_receipt(0xAA, 1);
        let result = BlockExecutionResult {
            new_state_root: Blake3Digest([0u8; 32]),
            receipts: vec![receipt.clone()],
            gas_used_total: 100,
            execution_ms: 5,
        };

        persist_results(&store, &result, shard_id).await.unwrap();

        // Verify receipt is stored
        let raw = store
            .get(ColumnFamily::Receipts.as_str(), &receipt.tx_digest.0)
            .await
            .unwrap();
        assert!(raw.is_some(), "receipt should be stored");

        let stored: TransactionReceipt = serde_json::from_slice(&raw.unwrap()).unwrap();
        assert_eq!(stored.tx_digest, receipt.tx_digest);
        assert_eq!(stored.gas_used, 100);
    }

    #[tokio::test]
    async fn persist_results_applies_state_changes() {
        let store = MemoryStore::new();
        let shard_id = ShardId(0);

        let receipt = make_receipt(0xBB, 1);
        let result = BlockExecutionResult {
            new_state_root: Blake3Digest([0u8; 32]),
            receipts: vec![receipt.clone()],
            gas_used_total: 100,
            execution_ms: 5,
        };

        persist_results(&store, &result, shard_id).await.unwrap();

        // Verify state change is applied — key is AccountKey ‖ change.key
        let mut key = nexus_storage::AccountKey {
            shard_id,
            address: AccountAddress([0xBB; 32]),
        }
        .to_bytes();
        key.extend_from_slice(b"balance");
        let raw = store.get(ColumnFamily::State.as_str(), &key).await.unwrap();
        assert!(raw.is_some(), "state change should be applied");
        assert_eq!(raw.unwrap(), 1000u64.to_le_bytes().to_vec());
    }

    #[tokio::test]
    async fn persist_results_handles_empty_receipts() {
        let store = MemoryStore::new();
        let result = BlockExecutionResult {
            new_state_root: Blake3Digest([0u8; 32]),
            receipts: vec![],
            gas_used_total: 0,
            execution_ms: 0,
        };

        let res = persist_results(&store, &result, ShardId(0)).await;
        assert!(res.is_ok());
    }

    #[test]
    fn execution_bridge_config_defaults() {
        let config = ExecutionBridgeConfig::default();
        assert_eq!(config.poll_interval, Duration::from_millis(50));
        assert_eq!(config.num_shards, 1);
    }

    #[test]
    fn shard_router_resolve_shard_defaults_to_zero() {
        assert_eq!(ShardRouter::resolve_shard(None), ShardId(0));
        assert_eq!(ShardRouter::resolve_shard(Some(ShardId(3))), ShardId(3));
    }

    #[tokio::test]
    async fn persist_multiple_receipts_atomically() {
        let store = MemoryStore::new();
        let shard_id = ShardId(0);

        let result = BlockExecutionResult {
            new_state_root: Blake3Digest([0u8; 32]),
            receipts: vec![make_receipt(0x01, 1), make_receipt(0x02, 1)],
            gas_used_total: 200,
            execution_ms: 10,
        };

        persist_results(&store, &result, shard_id).await.unwrap();

        // Both receipts should be stored
        let r1 = store
            .get(ColumnFamily::Receipts.as_str(), &[0x01; 32])
            .await
            .unwrap();
        let r2 = store
            .get(ColumnFamily::Receipts.as_str(), &[0x02; 32])
            .await
            .unwrap();
        assert!(r1.is_some());
        assert!(r2.is_some());
    }

    #[test]
    fn emit_bridge_events_sends_consensus_and_receipts() {
        let (tx, mut rx) = nexus_rpc::event_channel();
        let receipt = make_receipt(0xAB, 7);
        let status = nexus_rpc::dto::ConsensusStatusDto {
            epoch: nexus_primitives::EpochNumber(1),
            dag_size: 4,
            total_commits: 7,
            pending_commits: 0,
        };

        emit_bridge_events(
            &Some(tx),
            7,
            1,
            1234,
            std::slice::from_ref(&receipt),
            &status,
        );

        match rx.try_recv().unwrap() {
            nexus_rpc::ws::NodeEvent::NewCommit { sequence, .. } => assert_eq!(sequence, 7),
            other => panic!("unexpected first event: {other:?}"),
        }

        match rx.try_recv().unwrap() {
            nexus_rpc::ws::NodeEvent::TransactionExecuted(dto) => {
                assert_eq!(dto.tx_digest, receipt.tx_digest)
            }
            other => panic!("unexpected second event: {other:?}"),
        }

        match rx.try_recv().unwrap() {
            nexus_rpc::ws::NodeEvent::ConsensusStatus(dto) => {
                assert_eq!(dto.total_commits, 7);
                assert_eq!(dto.dag_size, 4);
            }
            other => panic!("unexpected third event: {other:?}"),
        }
    }

    #[test]
    fn update_chain_head_snapshot_records_empty_commit() {
        let chain_head = SharedChainHead::new();
        let consensus_status = nexus_rpc::dto::ConsensusStatusDto {
            epoch: nexus_primitives::EpochNumber(2),
            dag_size: 9,
            total_commits: 4,
            pending_commits: 0,
        };
        let batch = CommittedBatch {
            anchor: Blake3Digest([0xAB; 32]),
            certificates: vec![Blake3Digest([0xCD; 32])],
            sequence: CommitSequence(4),
            committed_at: TimestampMs(1234),
        };

        update_chain_head_snapshot(
            &chain_head,
            &batch,
            &consensus_status,
            batch.certificates.len(),
            0,
            0,
            Blake3Digest([0u8; 32]),
        );

        let head = chain_head.get().expect("chain head should be updated");
        assert_eq!(head.sequence, 4);
        assert_eq!(head.epoch, 2);
        assert_eq!(head.tx_count, 0);
        assert_eq!(head.gas_total, 0);
        assert_eq!(head.state_root, hex::encode([0u8; 32]));
    }

    // ── Phase A acceptance tests ─────────────────────────────────────────

    #[tokio::test]
    async fn execution_failure_should_not_drop_committed_batch() {
        // A-1 / SEC-C1: when execution fails, the batch must be persisted
        // as a dead-letter record rather than silently dropped.
        let store = MemoryStore::new();
        let batch = CommittedBatch {
            anchor: Blake3Digest([0xDE; 32]),
            certificates: vec![Blake3Digest([0xAD; 32])],
            sequence: CommitSequence(99),
            committed_at: TimestampMs(5555),
        };

        persist_dead_letter(&store, &batch, "simulated execution failure").await;

        // Verify a dead-letter record exists in the Blocks CF.
        let mut key = b"dead_letter:".to_vec();
        key.extend_from_slice(&99u64.to_be_bytes());

        let raw = store
            .get(ColumnFamily::Blocks.as_str(), &key)
            .await
            .unwrap();
        assert!(raw.is_some(), "dead-letter record must be persisted");

        let record: serde_json::Value = serde_json::from_slice(&raw.unwrap()).unwrap();
        assert_eq!(record["sequence"], 99);
        assert_eq!(record["error"], "simulated execution failure");
    }

    #[test]
    fn dead_letter_circuit_breaker_constant_is_bounded() {
        // E-5 / SEC-M16: the dead-letter limit must be finite so
        // `return` actually fires and halts the bridge.
        const _: () = assert!(MAX_DEAD_LETTER_ENTRIES > 0);
        const _: () = assert!(MAX_DEAD_LETTER_ENTRIES <= 256);
    }
}
