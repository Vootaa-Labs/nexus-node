// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! ExecutionService — async actor for processing committed batches.
//!
//! Bridges the consensus layer (which delivers [`CommittedBatch`] sequences)
//! to the execution engine ([`BlockStmExecutor`]).  The actor model follows
//! DEV-04 §3.2 (Mail-Actor pattern):
//!
//! 1. A bounded `mpsc` channel serves as the mailbox
//! 2. The actor loop runs on a dedicated Tokio task
//! 3. CPU-intensive Block-STM execution is offloaded via `spawn_blocking`
//! 4. Callers receive results through `oneshot` reply channels
//!
//! # Architecture
//!
//! ```text
//! Consensus ──→ submit_batch() ──→ [mpsc mailbox] ──→ Actor Loop
//!                                                        │
//!                                         spawn_blocking(block_stm.execute())
//!                                                        │
//!                                                  ◀── oneshot reply
//! ```
//!
//! # Thread Model
//!
//! - **Actor loop**: Tokio async worker (I/O bound, event-driven)
//! - **Block-STM**: offloaded to `spawn_blocking` (CPU bound, rayon parallelism)
//! - **State reads**: via `StateView` trait (sync; bridged inside `spawn_blocking`)

use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, info};

use crate::block_stm::BlockStmExecutor;
use crate::error::{ExecutionError, ExecutionResult};
use crate::metrics::ExecutionMetrics;
use crate::traits::StateView;
use crate::types::{BlockExecutionResult, SignedTransaction};
use nexus_config::ExecutionConfig;
use nexus_consensus::CommittedBatch;
use nexus_primitives::{CommitSequence, ShardId};

// ── Message type ────────────────────────────────────────────────────────

/// Messages processed by the [`ExecutionService`] actor.
#[derive(Debug)]
pub enum ExecutionMessage {
    /// Submit a committed batch for execution.
    SubmitBatch {
        /// The batch to execute.
        batch: CommittedBatch,
        /// Transactions extracted from the batch (caller resolves cert → tx).
        transactions: Vec<SignedTransaction>,
        /// Reply channel for the execution result.
        reply: oneshot::Sender<ExecutionResult<BlockExecutionResult>>,
    },

    /// Query the latest completed commit sequence.
    QueryLatestSequence {
        /// Reply channel.
        reply: oneshot::Sender<Option<CommitSequence>>,
    },

    /// Graceful shutdown signal.
    Shutdown,
}

// ── Handle (client-facing API) ──────────────────────────────────────────

/// Default mailbox capacity (bounded channel for backpressure).
const DEFAULT_MAILBOX_CAPACITY: usize = 256;

/// Client handle to the [`ExecutionService`] actor.
///
/// Cheaply cloneable (wraps an `mpsc::Sender`).
/// All interaction with the actor goes through this handle.
#[derive(Clone)]
pub struct ExecutionServiceHandle {
    tx: mpsc::Sender<ExecutionMessage>,
}

impl ExecutionServiceHandle {
    /// Submit a committed batch for execution and await the result.
    ///
    /// Transactions must be pre-extracted by the caller (e.g. from
    /// a mempool indexed by certificate digests).
    pub async fn submit_batch(
        &self,
        batch: CommittedBatch,
        transactions: Vec<SignedTransaction>,
    ) -> ExecutionResult<BlockExecutionResult> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(ExecutionMessage::SubmitBatch {
                batch,
                transactions,
                reply: reply_tx,
            })
            .await
            .map_err(|_| ExecutionError::Storage("execution service unavailable".into()))?;
        reply_rx
            .await
            .map_err(|_| ExecutionError::Storage("execution service dropped reply".into()))?
    }

    /// Query the latest completed commit sequence.
    pub async fn query_latest_sequence(&self) -> ExecutionResult<Option<CommitSequence>> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(ExecutionMessage::QueryLatestSequence { reply: reply_tx })
            .await
            .map_err(|_| ExecutionError::Storage("execution service unavailable".into()))?;
        reply_rx
            .await
            .map_err(|_| ExecutionError::Storage("execution service dropped reply".into()))
    }

    /// Send a graceful shutdown signal.
    ///
    /// The actor will finish processing its current message and then exit.
    pub async fn shutdown(&self) -> ExecutionResult<()> {
        self.tx
            .send(ExecutionMessage::Shutdown)
            .await
            .map_err(|_| ExecutionError::Storage("execution service already stopped".into()))
    }
}

// ── Actor (internal event loop) ─────────────────────────────────────────

/// The ExecutionService actor — owns the executor and processes messages.
///
/// Not directly constructed; use [`spawn_execution_service`] to start.
struct ExecutionActor {
    /// Message inbox.
    rx: mpsc::Receiver<ExecutionMessage>,
    /// Shard this actor processes.
    shard_id: ShardId,
    /// Execution configuration.
    config: ExecutionConfig,
    /// State view for reading pre-execution state.
    state: Arc<dyn StateView>,
    /// Last completed commit sequence.
    last_sequence: Option<CommitSequence>,
    /// Per-shard metrics handle.
    metrics: ExecutionMetrics,
}

impl ExecutionActor {
    /// Run the actor event loop until Shutdown or channel close.
    async fn run(mut self) {
        info!(shard = ?self.shard_id, "ExecutionService actor started");

        while let Some(msg) = self.rx.recv().await {
            match msg {
                ExecutionMessage::SubmitBatch {
                    batch,
                    transactions,
                    reply,
                } => {
                    self.metrics.inc_active_blocks();
                    let result = self.execute_batch(batch, transactions).await;
                    self.metrics.dec_active_blocks();
                    if let Err(ref e) = result {
                        self.metrics.record_error(e.metric_label());
                    }
                    // Ignore send errors — caller may have dropped the receiver.
                    let _ = reply.send(result);
                }
                ExecutionMessage::QueryLatestSequence { reply } => {
                    let _ = reply.send(self.last_sequence);
                }
                ExecutionMessage::Shutdown => {
                    info!(shard = ?self.shard_id, "ExecutionService actor shutting down");
                    break;
                }
            }
        }

        info!(shard = ?self.shard_id, "ExecutionService actor stopped");
    }

    /// Execute a single committed batch.
    ///
    /// Block-STM is CPU-intensive so we offload to `spawn_blocking`.
    #[tracing::instrument(
        skip(self, transactions),
        fields(
            shard = ?self.shard_id,
            sequence = batch.sequence.0,
            num_certs = batch.certificates.len(),
            num_txs = transactions.len(),
        )
    )]
    async fn execute_batch(
        &mut self,
        batch: CommittedBatch,
        transactions: Vec<SignedTransaction>,
    ) -> ExecutionResult<BlockExecutionResult> {
        let sequence = batch.sequence;
        let committed_at = batch.committed_at;
        let shard_id = self.shard_id;
        let max_retries = self.config.block_stm_max_retries as u32;
        let max_workers = self.config.block_stm_threads;
        let state = Arc::clone(&self.state);

        debug!(
            num_txs = transactions.len(),
            "Submitting batch to Block-STM"
        );

        let start = std::time::Instant::now();

        // Offload CPU-intensive execution to the blocking thread pool.
        let result = tokio::task::spawn_blocking(move || {
            let executor = BlockStmExecutor::with_config(
                shard_id,
                sequence,
                committed_at,
                max_retries,
                max_workers,
            );
            executor.execute(&transactions, state.as_ref())
        })
        .await
        .map_err(|e| ExecutionError::Storage(format!("spawn_blocking join error: {e}")))?;

        let elapsed = start.elapsed();

        match &result {
            Ok(block_result) => {
                self.last_sequence = Some(sequence);
                info!(
                    sequence = sequence.0,
                    gas_total = block_result.gas_used_total,
                    receipts = block_result.receipts.len(),
                    elapsed_ms = elapsed.as_millis() as u32,
                    "Batch executed successfully"
                );
            }
            Err(e) => {
                error!(
                    sequence = sequence.0,
                    error = %e,
                    "Batch execution failed"
                );
            }
        }

        result
    }
}

// ── Spawner ─────────────────────────────────────────────────────────────

/// Spawn the [`ExecutionService`] actor and return a client handle.
///
/// The actor runs on a dedicated Tokio task and processes
/// [`ExecutionMessage`]s from the returned handle.
///
/// # Arguments
///
/// - `config` — execution configuration (thread count, retry limits, etc.)
/// - `shard_id` — shard this service processes
/// - `state` — state view for reading pre-execution state
///
/// # Example
///
/// ```rust,no_run
/// # use nexus_execution::service::spawn_execution_service;
/// # use nexus_config::ExecutionConfig;
/// # use nexus_primitives::ShardId;
/// # use std::sync::Arc;
/// # async fn example(state: Arc<dyn nexus_execution::StateView>) {
/// let handle = spawn_execution_service(
///     ExecutionConfig::for_testing(),
///     ShardId(0),
///     state,
/// );
/// # }
/// ```
pub fn spawn_execution_service(
    config: ExecutionConfig,
    shard_id: ShardId,
    state: Arc<dyn StateView>,
) -> ExecutionServiceHandle {
    spawn_execution_service_with_capacity(config, shard_id, state, DEFAULT_MAILBOX_CAPACITY)
}

/// Spawn the execution service with a custom mailbox capacity.
///
/// Useful for testing (small capacity to test backpressure) or
/// high-throughput deployments (larger buffer).
pub fn spawn_execution_service_with_capacity(
    config: ExecutionConfig,
    shard_id: ShardId,
    state: Arc<dyn StateView>,
    capacity: usize,
) -> ExecutionServiceHandle {
    let (tx, rx) = mpsc::channel(capacity);

    let actor = ExecutionActor {
        rx,
        shard_id,
        config,
        state,
        last_sequence: None,
        metrics: ExecutionMetrics::new(shard_id),
    };

    tokio::spawn(actor.run());

    ExecutionServiceHandle { tx }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        compute_tx_digest, ExecutionStatus, TransactionBody, TransactionPayload, TX_DOMAIN,
    };
    use nexus_crypto::{DilithiumSigner, DilithiumSigningKey, DilithiumVerifyKey, Signer};
    use nexus_primitives::{Amount, Blake3Digest, EpochNumber, TimestampMs, TokenId};
    use std::collections::HashMap;

    /// In-memory StateView for testing.
    struct MemState {
        data: HashMap<(nexus_primitives::AccountAddress, Vec<u8>), Vec<u8>>,
    }

    impl MemState {
        fn new() -> Self {
            Self {
                data: HashMap::new(),
            }
        }

        fn set_balance(&mut self, addr: nexus_primitives::AccountAddress, balance: u64) {
            self.data
                .insert((addr, b"balance".to_vec()), balance.to_le_bytes().to_vec());
        }
    }

    impl StateView for MemState {
        fn get(
            &self,
            account: &nexus_primitives::AccountAddress,
            key: &[u8],
        ) -> ExecutionResult<Option<Vec<u8>>> {
            Ok(self.data.get(&(*account, key.to_vec())).cloned())
        }
    }

    struct TestAccount {
        sk: DilithiumSigningKey,
        pk: DilithiumVerifyKey,
        address: nexus_primitives::AccountAddress,
    }

    impl TestAccount {
        fn random() -> Self {
            let (sk, pk) = DilithiumSigner::generate_keypair();
            let address = nexus_primitives::AccountAddress::from_dilithium_pubkey(pk.as_bytes());
            Self { sk, pk, address }
        }
    }

    fn make_transfer(
        sender: &TestAccount,
        recipient: nexus_primitives::AccountAddress,
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
        let digest = compute_tx_digest(&body).expect("digest");
        let sig = DilithiumSigner::sign(&sender.sk, TX_DOMAIN, digest.as_bytes());
        SignedTransaction {
            body,
            signature: sig,
            sender_pk: sender.pk.clone(),
            digest,
        }
    }

    fn make_batch(seq: u64) -> CommittedBatch {
        CommittedBatch {
            anchor: Blake3Digest([seq as u8; 32]),
            certificates: vec![Blake3Digest([seq as u8; 32])],
            sequence: CommitSequence(seq),
            committed_at: TimestampMs(1_000_000 + seq),
        }
    }

    #[tokio::test]
    async fn submit_empty_batch() {
        let state = Arc::new(MemState::new());
        let handle = spawn_execution_service(ExecutionConfig::for_testing(), ShardId(0), state);

        let batch = make_batch(1);
        let result = handle.submit_batch(batch, vec![]).await.unwrap();
        assert_eq!(result.receipts.len(), 0);
        assert_eq!(result.gas_used_total, 0);

        handle.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn submit_single_transfer() {
        let sender = TestAccount::random();
        let recipient = nexus_primitives::AccountAddress([0xBB; 32]);
        let mut state = MemState::new();
        state.set_balance(sender.address, 1_000_000);

        let state = Arc::new(state);
        let handle = spawn_execution_service(ExecutionConfig::for_testing(), ShardId(0), state);

        let tx = make_transfer(&sender, recipient, 100, 0);
        let batch = make_batch(1);
        let result = handle.submit_batch(batch, vec![tx]).await.unwrap();

        assert_eq!(result.receipts.len(), 1);
        assert_eq!(result.receipts[0].status, ExecutionStatus::Success);
        assert!(result.gas_used_total > 0);

        handle.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn sequential_batches_update_sequence() {
        let state = Arc::new(MemState::new());
        let handle = spawn_execution_service(ExecutionConfig::for_testing(), ShardId(0), state);

        // Initially no sequence.
        let seq = handle.query_latest_sequence().await.unwrap();
        assert_eq!(seq, None);

        // Execute batch 1.
        let batch = make_batch(1);
        let _ = handle.submit_batch(batch, vec![]).await.unwrap();
        let seq = handle.query_latest_sequence().await.unwrap();
        assert_eq!(seq, Some(CommitSequence(1)));

        // Execute batch 2.
        let batch = make_batch(2);
        let _ = handle.submit_batch(batch, vec![]).await.unwrap();
        let seq = handle.query_latest_sequence().await.unwrap();
        assert_eq!(seq, Some(CommitSequence(2)));

        handle.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn shutdown_is_idempotent() {
        let state = Arc::new(MemState::new());
        let handle = spawn_execution_service(ExecutionConfig::for_testing(), ShardId(0), state);

        handle.shutdown().await.unwrap();

        // Second shutdown should return an error (channel closed).
        // Small delay to let the actor process the first shutdown.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let result = handle.shutdown().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn multiple_transfers_in_batch() {
        let alice = TestAccount::random();
        let bob = TestAccount::random();
        let charlie = nexus_primitives::AccountAddress([0xCC; 32]);
        let mut state = MemState::new();
        state.set_balance(alice.address, 10_000_000);
        state.set_balance(bob.address, 5_000_000);

        let state = Arc::new(state);
        let handle = spawn_execution_service(ExecutionConfig::for_testing(), ShardId(0), state);

        let txs = vec![
            make_transfer(&alice, bob.address, 1000, 0),
            make_transfer(&bob, charlie, 500, 0),
        ];
        let batch = make_batch(1);
        let result = handle.submit_batch(batch, txs).await.unwrap();

        assert_eq!(result.receipts.len(), 2);
        assert!(result.gas_used_total > 0);

        handle.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn handle_is_clone() {
        let state = Arc::new(MemState::new());
        let handle = spawn_execution_service(ExecutionConfig::for_testing(), ShardId(0), state);

        let handle2 = handle.clone();

        // Both handles can submit.
        let batch = make_batch(1);
        let _ = handle.submit_batch(batch, vec![]).await.unwrap();

        let batch = make_batch(2);
        let _ = handle2.submit_batch(batch, vec![]).await.unwrap();

        handle.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn dropped_handle_stops_actor() {
        let state = Arc::new(MemState::new());
        let handle = spawn_execution_service(ExecutionConfig::for_testing(), ShardId(0), state);

        drop(handle);
        // Actor should stop when all senders are dropped.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        // If we get here without hanging, the actor exited cleanly.
    }
}
