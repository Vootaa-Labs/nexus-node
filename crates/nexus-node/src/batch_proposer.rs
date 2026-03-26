// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Batch proposer — periodically drains the mempool, builds Narwhal batches,
//! stores transaction payloads in the [`BatchStore`], and submits proposals
//! to the [`CertAggregator`](crate::cert_aggregator) for multi-validator
//! certificate construction.
//!
//! The proposer is the "worker" role in a simplified Narwhal design:
//! 1. Drain pending transactions from the mempool
//! 2. BCS-encode each transaction and assemble a batch
//! 3. Store the batch→transactions mapping in [`BatchStore`]
//! 4. Submit a [`LocalProposal`] to the cert aggregator
//!
//! The cert aggregator handles signature collection, certificate building,
//! and broadcast.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use nexus_consensus::{CertificateDag, ConsensusEngine, ValidatorRegistry};
use nexus_crypto::Blake3Hasher;
use nexus_primitives::{EpochNumber, RoundNumber, ValidatorIndex};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::batch_store::BatchStore;
use crate::cert_aggregator::LocalProposal;
use crate::mempool::Mempool;
use crate::node_metrics;

/// Default interval between batch proposal attempts.
const DEFAULT_PROPOSAL_INTERVAL: Duration = Duration::from_millis(200);

/// Maximum number of transactions per batch.
const DEFAULT_MAX_BATCH_TRANSACTIONS: usize = 512;

/// Domain tag for computing batch digests (matches consensus types).
const BATCH_DOMAIN: &[u8] = b"nexus::narwhal::batch::v1";

/// Configuration for the batch proposer.
#[derive(Debug, Clone)]
pub struct BatchProposerConfig {
    /// How often to attempt batch proposal (default: 200ms).
    pub proposal_interval: Duration,
    /// Maximum transactions per batch (default: 512).
    pub max_batch_transactions: usize,
    /// How often to propose when the mempool is empty (default: same as
    /// `proposal_interval`).  A larger value prevents the DAG from racing
    /// far ahead of the Shoal commit anchor during idle periods, which
    /// improves lifecycle-tracking latency for benchmarks.
    pub empty_proposal_interval: Duration,
}

fn next_follow_up_state(force_empty_round: bool, tx_count: usize) -> (bool, bool) {
    if tx_count > 0 {
        return (true, true);
    }

    if force_empty_round {
        return (true, false);
    }

    // Always propose — even empty batches — to maintain DAG liveness.
    // In Narwhal, all validators must propose in every round so the DAG
    // keeps growing and Shoal can find anchors and commit sub-DAGs.
    (true, false)
}

impl Default for BatchProposerConfig {
    fn default() -> Self {
        Self {
            proposal_interval: DEFAULT_PROPOSAL_INTERVAL,
            max_batch_transactions: DEFAULT_MAX_BATCH_TRANSACTIONS,
            empty_proposal_interval: DEFAULT_PROPOSAL_INTERVAL,
        }
    }
}

/// Spawn the batch proposer background task.
///
/// The proposer runs a periodic loop:
/// 1. Drain up to `max_batch_transactions` from the mempool
/// 2. If non-empty, build a NarwhalBatch
/// 3. Store tx mapping in `BatchStore`
/// 4. Submit a `LocalProposal` to the cert aggregator
///
/// Returns a `JoinHandle` for lifecycle management.
pub fn spawn_batch_proposer(
    config: BatchProposerConfig,
    mempool: Arc<Mempool>,
    batch_store: Arc<BatchStore>,
    engine: Arc<Mutex<ConsensusEngine>>,
    validator_index: ValidatorIndex,
    current_epoch: Arc<std::sync::atomic::AtomicU64>,
    proposal_tx: mpsc::Sender<LocalProposal>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        // Round 0 is reserved for synthetic genesis certificates seeded at node startup.
        // Real proposals begin at round 1 and reference those anchors as parents.
        let mut round = RoundNumber(1);
        let mut force_empty_round = false;

        debug!(
            validator = validator_index.0,
            interval_ms = config.proposal_interval.as_millis() as u64,
            empty_interval_ms = config.empty_proposal_interval.as_millis() as u64,
            max_txs = config.max_batch_transactions,
            "batch proposer started"
        );

        let mut last_was_empty = false;

        loop {
            let sleep_dur = if last_was_empty {
                config.empty_proposal_interval
            } else {
                config.proposal_interval
            };
            tokio::time::sleep(sleep_dur).await;

            // 1. Drain mempool
            let transactions = mempool.drain_batch(config.max_batch_transactions);
            let tx_count = transactions.len();
            node_metrics::mempool_pending(mempool.len());
            if tx_count > 0 {
                node_metrics::mempool_dequeue(tx_count as u64);
            }
            let (should_propose, next_force_empty_round) =
                next_follow_up_state(force_empty_round, tx_count);
            force_empty_round = next_force_empty_round;

            if !should_propose {
                continue;
            }

            let epoch = EpochNumber(current_epoch.load(std::sync::atomic::Ordering::Relaxed));

            // 2. BCS-encode each transaction for the batch payload
            let tx_bytes: Vec<Vec<u8>> = transactions
                .iter()
                .filter_map(|tx| bcs::to_bytes(tx).ok())
                .collect();

            if tx_bytes.len() != tx_count {
                warn!(
                    expected = tx_count,
                    encoded = tx_bytes.len(),
                    "batch proposer: some transactions failed BCS encoding"
                );
            }

            // 3. Compute batch digest
            let batch_payload = match bcs::to_bytes(&(validator_index, round, &tx_bytes)) {
                Ok(p) => p,
                Err(e) => {
                    warn!(error = %e, "batch proposer: failed to serialize batch payload");
                    continue;
                }
            };
            let batch_digest = Blake3Hasher::digest(BATCH_DOMAIN, &batch_payload);

            // 4. Store transaction mapping for execution bridge
            batch_store.insert(batch_digest, transactions);

            // 5. Get parents and committee info from engine
            let (parents, num_validators, quorum_threshold) = {
                let eng = match engine.lock() {
                    Ok(g) => g,
                    Err(_) => {
                        warn!("batch proposer: engine lock poisoned");
                        continue;
                    }
                };
                let parents = if round.0 == 0 {
                    vec![]
                } else {
                    eng.dag()
                        .round_certificates(RoundNumber(round.0 - 1))
                        .iter()
                        .map(|c| c.cert_digest)
                        .collect()
                };
                let nv = eng.committee().active_validators().len() as u32;
                let qt = eng.committee().quorum_threshold();
                (parents, nv, qt)
            };

            // 6. Submit proposal to cert aggregator for signature collection
            let proposal = LocalProposal {
                epoch,
                batch_digest,
                batch_payload: tx_bytes.clone(),
                origin: validator_index,
                round,
                parents,
                num_validators,
                quorum_threshold,
            };

            if let Err(e) = proposal_tx.send(proposal).await {
                warn!(error = %e, "batch proposer: failed to send proposal to cert aggregator");
                continue;
            }

            // Advance round
            round = RoundNumber(round.0 + 1);
            // Track whether this proposal was empty for next sleep decision.
            last_was_empty = tx_count == 0;
            node_metrics::batch_proposed(round.0, tx_count as u64);

            info!(
                round = round.0,
                txs = tx_count,
                batch = %batch_digest,
                "batch proposer: proposed batch"
            );
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_crypto::{DilithiumSigner, Signer};
    use nexus_execution::types::{
        compute_tx_digest, SignedTransaction, TransactionBody, TransactionPayload, TX_DOMAIN,
    };
    use nexus_primitives::{AccountAddress, Amount, EpochNumber, ShardId, TokenId};

    fn make_tx(seq: u64) -> SignedTransaction {
        let (sk, vk) = DilithiumSigner::generate_keypair();
        let body = TransactionBody {
            sender: AccountAddress([1u8; 32]),
            sequence_number: seq,
            expiry_epoch: EpochNumber(100),
            gas_limit: 10_000,
            gas_price: 1,
            target_shard: Some(ShardId(0)),
            payload: TransactionPayload::Transfer {
                recipient: AccountAddress([2u8; 32]),
                amount: Amount(100),
                token: TokenId::Native,
            },
            chain_id: 1,
        };
        let digest = compute_tx_digest(&body).expect("digest");
        let body_bytes = bcs::to_bytes(&body).expect("bcs");
        let sig = DilithiumSigner::sign(&sk, TX_DOMAIN, &body_bytes);
        SignedTransaction {
            body,
            signature: sig,
            sender_pk: vk,
            digest,
        }
    }

    #[test]
    fn batch_proposer_config_defaults() {
        let config = BatchProposerConfig::default();
        assert_eq!(config.proposal_interval, Duration::from_millis(200));
        assert_eq!(config.max_batch_transactions, 512);
        assert_eq!(config.empty_proposal_interval, Duration::from_millis(200));
    }

    #[test]
    fn batch_digest_deterministic() {
        let vi = ValidatorIndex(0);
        let round = RoundNumber(0);
        let tx = make_tx(1);
        let tx_bytes = vec![bcs::to_bytes(&tx).unwrap()];

        let payload1 = bcs::to_bytes(&(vi, round, &tx_bytes)).unwrap();
        let d1 = Blake3Hasher::digest(BATCH_DOMAIN, &payload1);

        let payload2 = bcs::to_bytes(&(vi, round, &tx_bytes)).unwrap();
        let d2 = Blake3Hasher::digest(BATCH_DOMAIN, &payload2);

        assert_eq!(d1, d2, "same input should produce same digest");
    }

    #[test]
    fn batch_digest_differs_by_round() {
        let vi = ValidatorIndex(0);
        let tx_bytes: Vec<Vec<u8>> = vec![vec![1, 2, 3]];

        let p1 = bcs::to_bytes(&(vi, RoundNumber(0), &tx_bytes)).unwrap();
        let d1 = Blake3Hasher::digest(BATCH_DOMAIN, &p1);

        let p2 = bcs::to_bytes(&(vi, RoundNumber(1), &tx_bytes)).unwrap();
        let d2 = Blake3Hasher::digest(BATCH_DOMAIN, &p2);

        assert_ne!(d1, d2, "different rounds should produce different digests");
    }

    #[test]
    fn follow_up_state_schedules_empty_round_after_non_empty_batch() {
        assert_eq!(next_follow_up_state(false, 1), (true, true));
        assert_eq!(next_follow_up_state(true, 0), (true, false));
    }

    #[test]
    fn follow_up_state_skips_idle_ticks_without_pending_follow_up() {
        // Even with no pending follow-up and no txs, we still propose
        // empty batches to maintain DAG liveness.
        assert_eq!(next_follow_up_state(false, 0), (true, false));
    }

    #[test]
    fn batch_store_integration() {
        let store = BatchStore::new();
        let txs = vec![make_tx(1), make_tx(2)];
        let vi = ValidatorIndex(0);
        let round = RoundNumber(0);

        let tx_bytes: Vec<Vec<u8>> = txs.iter().map(|t| bcs::to_bytes(t).unwrap()).collect();
        let payload = bcs::to_bytes(&(vi, round, &tx_bytes)).unwrap();
        let digest = Blake3Hasher::digest(BATCH_DOMAIN, &payload);

        store.insert(digest, txs.clone());
        let retrieved = store.get(&digest).unwrap();
        assert_eq!(retrieved.len(), 2);
    }
}
