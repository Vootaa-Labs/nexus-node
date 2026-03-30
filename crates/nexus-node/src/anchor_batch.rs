// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Anchor batch task — periodically bundles un-anchored provenance records
//! into a `ProvenanceAnchor` transaction and injects it into the mempool.
//!
//! The task runs on a configurable interval. When enough provenance records
//! have accumulated (above `min_records`), it:
//!
//! 1. Queries `RocksProvenanceStore::pending_anchor_record_ids`.
//! 2. Builds an [`AnchorBatch`] with a fresh sequence number.
//! 3. Wraps the batch as a [`SignedTransaction`] with
//!    [`TransactionPayload::ProvenanceAnchor`].
//! 4. Injects the transaction into the local [`Mempool`].

use std::sync::Arc;
use std::time::Duration;

use nexus_crypto::{DilithiumSigner, Signer};
use nexus_execution::types::{
    compute_tx_digest, SignedTransaction, TransactionBody, TransactionPayload, TX_DOMAIN,
};
use nexus_intent::{AnchorBatch, RocksProvenanceStore};
use nexus_primitives::{AccountAddress, EpochNumber, TimestampMs};
use nexus_storage::traits::StateStorage;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::mempool::Mempool;
use crate::node_metrics;

/// Domain tag for deriving the system signing seed from a chain ID.
const SYSTEM_SEED_DOMAIN: &[u8] = b"nexus::system::provenance_anchor::v1";

/// Configuration for the anchor batch task.
#[derive(Debug, Clone)]
pub struct AnchorBatchConfig {
    /// How often to check for pending provenance records.
    pub interval: Duration,
    /// Minimum number of un‐anchored records required to trigger a batch.
    pub min_records: u32,
    /// Maximum number of records per anchor batch.
    pub max_records: u32,
    /// Numeric chain identifier embedded in anchor transactions (SEC-H8).
    pub chain_id: u64,
    /// Chain-ID string used to derive the system signing seed (SEC-H9).
    /// Different chains get different system keypairs.
    pub chain_id_str: String,
}

impl Default for AnchorBatchConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(30),
            min_records: 1,
            max_records: 256,
            chain_id: 1,
            chain_id_str: "nexus-devnet".to_owned(),
        }
    }
}

impl AnchorBatchConfig {
    /// Create a config from a genesis chain-ID string.
    ///
    /// The numeric `chain_id` is derived as the first 8 bytes of
    /// `BLAKE3(chain_id_str)` interpreted as little-endian `u64`.
    pub fn from_chain_id(chain_id_str: &str) -> Self {
        let hash = blake3::hash(chain_id_str.as_bytes());
        let bytes: [u8; 8] = hash.as_bytes()[..8].try_into().expect("8 bytes");
        let chain_id = u64::from_le_bytes(bytes);
        Self {
            chain_id,
            chain_id_str: chain_id_str.to_owned(),
            ..Self::default()
        }
    }
}

/// Derive a deterministic 32-byte seed from the chain ID string.
fn derive_system_seed(chain_id_str: &str) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(SYSTEM_SEED_DOMAIN);
    hasher.update(chain_id_str.as_bytes());
    *hasher.finalize().as_bytes()
}

/// Spawn the anchor batch background task.
///
/// Returns a `JoinHandle` that runs until the runtime shuts down.
pub fn spawn_anchor_batch_task<S: StateStorage + Send + Sync + 'static>(
    config: AnchorBatchConfig,
    provenance_store: Arc<RocksProvenanceStore<S>>,
    mempool: Arc<Mempool>,
) -> JoinHandle<()> {
    // Derive system seed and keypair from chain identity (SEC-H9).
    let system_seed = derive_system_seed(&config.chain_id_str);
    let (system_sk, system_vk) = DilithiumSigner::keypair_from_seed(&system_seed);
    // System account = first 32 bytes of the public key hash.
    let pk_hash = blake3::hash(system_vk.as_bytes());
    let mut system_account_bytes = [0u8; 32];
    system_account_bytes.copy_from_slice(&pk_hash.as_bytes()[..32]);
    let system_account = AccountAddress(system_account_bytes);
    let chain_id = config.chain_id;

    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(config.interval);
        // Skip the first immediate tick.
        ticker.tick().await;

        loop {
            ticker.tick().await;

            let record_ids = provenance_store.pending_anchor_record_ids(config.max_records);

            if (record_ids.len() as u32) < config.min_records {
                debug!(
                    pending = record_ids.len(),
                    min = config.min_records,
                    "anchor batch: not enough pending records, skipping"
                );
                continue;
            }

            let next_seq = provenance_store.last_anchor_seq().map_or(0, |s| s + 1);
            let now = TimestampMs::now();

            let batch = match AnchorBatch::new(next_seq, record_ids, now) {
                Ok(b) => b,
                Err(e) => {
                    warn!(error = %e, "anchor batch: failed to build AnchorBatch");
                    continue;
                }
            };

            let record_count = batch.len() as u32;
            let anchor_digest = batch.anchor_digest;

            let body = TransactionBody {
                sender: system_account,
                sequence_number: next_seq,
                expiry_epoch: EpochNumber(u64::MAX),
                gas_limit: 1_000,
                gas_price: 0,
                target_shard: None,
                payload: TransactionPayload::ProvenanceAnchor {
                    anchor_digest,
                    batch_seq: next_seq,
                    record_count,
                },
                chain_id,
            };

            let digest = match compute_tx_digest(&body) {
                Ok(d) => d,
                Err(e) => {
                    warn!(error = %e, "anchor batch: failed to compute tx digest");
                    continue;
                }
            };

            // Sign with the deterministic system key.
            let body_bytes = match bcs::to_bytes(&body) {
                Ok(b) => b,
                Err(e) => {
                    warn!(error = %e, "anchor batch: body serialization failed");
                    continue;
                }
            };
            let signature = DilithiumSigner::sign(&system_sk, TX_DOMAIN, &body_bytes);

            let tx = SignedTransaction {
                body,
                signature,
                sender_pk: system_vk.clone(),
                digest,
            };

            let result = mempool.insert(tx);
            node_metrics::provenance_anchor_submitted();
            info!(
                batch_seq = next_seq,
                records = record_count,
                anchor = %anchor_digest,
                insert = ?result,
                "anchor batch: submitted ProvenanceAnchor transaction"
            );
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_values() {
        let cfg = AnchorBatchConfig::default();
        assert_eq!(cfg.interval, Duration::from_secs(30));
        assert_eq!(cfg.min_records, 1);
        assert_eq!(cfg.max_records, 256);
        assert_eq!(cfg.chain_id, 1);
    }

    #[test]
    fn from_chain_id_derives_deterministic_values() {
        let cfg1 = AnchorBatchConfig::from_chain_id("nexus-testnet-1");
        let cfg2 = AnchorBatchConfig::from_chain_id("nexus-testnet-1");
        assert_eq!(cfg1.chain_id, cfg2.chain_id);
        assert_ne!(cfg1.chain_id, 0);
        assert_ne!(cfg1.chain_id, 1); // different from default

        let cfg3 = AnchorBatchConfig::from_chain_id("nexus-mainnet-v1");
        assert_ne!(
            cfg1.chain_id, cfg3.chain_id,
            "different chains should have different IDs"
        );
    }

    #[test]
    fn system_seed_differs_per_chain() {
        let s1 = derive_system_seed("chain-a");
        let s2 = derive_system_seed("chain-b");
        assert_ne!(
            s1, s2,
            "different chains should derive different system seeds"
        );
    }

    #[test]
    fn derive_system_seed_is_deterministic() {
        let a = derive_system_seed("nexus-testnet-1");
        let b = derive_system_seed("nexus-testnet-1");
        assert_eq!(a, b);
    }

    #[test]
    fn derive_system_seed_is_32_bytes() {
        let seed = derive_system_seed("anything");
        assert_eq!(seed.len(), 32);
    }

    #[test]
    fn derive_system_seed_domain_separated() {
        // A plain blake3 of the same input without domain tag should differ
        let seed = derive_system_seed("nexus-devnet");
        let plain = *blake3::hash(b"nexus-devnet").as_bytes();
        assert_ne!(
            seed, plain,
            "domain separation must produce different output"
        );
    }
}
