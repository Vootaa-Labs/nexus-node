// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Storage maintenance — periodic metrics collection and data pruning (P5-2 / P5-3).
//!
//! Spawns a background task that:
//! 1. Periodically collects per-CF storage statistics and publishes them as
//!    Prometheus gauges via `node_metrics`.
//! 2. Optionally prunes historical Blocks/Transactions/Receipts older than a
//!    configurable retention window.

use nexus_storage::{RocksStore, StorageConfig};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::node_metrics;

/// Configuration for the storage maintenance task.
#[derive(Debug, Clone)]
pub struct StorageMaintenanceConfig {
    /// Interval between metrics collection runs.
    pub metrics_interval: Duration,
    /// Whether pruning is enabled.
    pub pruning_enabled: bool,
    /// Number of most recent blocks to retain.
    pub pruning_retention_blocks: u64,
    /// Interval between pruning runs.
    pub pruning_interval: Duration,
}

impl StorageMaintenanceConfig {
    /// Build from the storage config section.
    pub fn from_storage_config(cfg: &StorageConfig) -> Self {
        Self {
            metrics_interval: Duration::from_secs(30),
            pruning_enabled: cfg.pruning_enabled,
            pruning_retention_blocks: cfg.pruning_retention_blocks,
            pruning_interval: Duration::from_secs(cfg.pruning_interval_secs),
        }
    }
}

impl Default for StorageMaintenanceConfig {
    fn default() -> Self {
        Self {
            metrics_interval: Duration::from_secs(30),
            pruning_enabled: false,
            pruning_retention_blocks: 100_000,
            pruning_interval: Duration::from_secs(3600),
        }
    }
}

/// Spawn the storage maintenance background task.
///
/// `chain_height` is a shared counter updated by the execution bridge
/// so that the pruning task knows the current block height.
pub fn spawn_storage_maintenance(
    config: StorageMaintenanceConfig,
    store: RocksStore,
    chain_height: Arc<AtomicU64>,
    readiness_handle: crate::readiness::SubsystemHandle,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        info!("storage maintenance task started");
        let mut metrics_tick = tokio::time::interval(config.metrics_interval);
        let mut prune_tick = tokio::time::interval(config.pruning_interval);

        loop {
            tokio::select! {
                _ = metrics_tick.tick() => {
                    collect_metrics(&store);
                    readiness_handle.report_progress();
                }
                _ = prune_tick.tick() => {
                    if config.pruning_enabled {
                        run_pruning(&store, &config, &chain_height);
                    }
                    readiness_handle.report_progress();
                }
            }
        }
    })
}

fn collect_metrics(store: &RocksStore) {
    match store.storage_stats() {
        Ok(stats) => {
            for s in &stats {
                let cf_name = s.cf.as_str();
                node_metrics::storage_cf_sst_size(cf_name, s.sst_file_size_bytes);
                node_metrics::storage_cf_memtable_size(cf_name, s.memtable_size_bytes);
                node_metrics::storage_cf_estimated_keys(cf_name, s.estimated_num_keys);
            }
            debug!(cfs = stats.len(), "storage metrics collected");
        }
        Err(e) => {
            warn!(error = %e, "failed to collect storage metrics");
        }
    }
}

fn prune_retain_from(config: &StorageMaintenanceConfig, chain_height: u64) -> Option<u64> {
    if chain_height > config.pruning_retention_blocks {
        Some(chain_height - config.pruning_retention_blocks)
    } else {
        None
    }
}

fn run_pruning(store: &RocksStore, config: &StorageMaintenanceConfig, chain_height: &AtomicU64) {
    let height = chain_height.load(Ordering::Relaxed);
    let Some(retain_from) = prune_retain_from(config, height) else {
        debug!(
            height,
            retention = config.pruning_retention_blocks,
            "not enough blocks to prune"
        );
        return;
    };

    match store.prune_before(retain_from) {
        Ok(result) => {
            if result.blocks_pruned > 0
                || result.transactions_pruned > 0
                || result.receipts_pruned > 0
            {
                info!(
                    blocks = result.blocks_pruned,
                    txs = result.transactions_pruned,
                    receipts = result.receipts_pruned,
                    retain_from,
                    "storage pruned"
                );
                node_metrics::storage_pruned(
                    result.blocks_pruned,
                    result.transactions_pruned,
                    result.receipts_pruned,
                );
            }
        }
        Err(e) => {
            warn!(error = %e, "storage pruning failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_storage::traits::{StateStorage, WriteBatchOps};
    use nexus_storage::ColumnFamily;
    use tempfile::TempDir;

    #[test]
    fn storage_maintenance_config_maps_storage_config_fields() {
        let mut storage_cfg =
            StorageConfig::for_testing(std::path::PathBuf::from("/tmp/nexus-node-maint"));
        storage_cfg.pruning_enabled = true;
        storage_cfg.pruning_retention_blocks = 4321;
        storage_cfg.pruning_interval_secs = 17;

        let cfg = StorageMaintenanceConfig::from_storage_config(&storage_cfg);
        assert_eq!(cfg.metrics_interval, Duration::from_secs(30));
        assert!(cfg.pruning_enabled);
        assert_eq!(cfg.pruning_retention_blocks, 4321);
        assert_eq!(cfg.pruning_interval, Duration::from_secs(17));
    }

    #[test]
    fn prune_retain_from_respects_boundary_conditions() {
        let cfg = StorageMaintenanceConfig {
            metrics_interval: Duration::from_secs(30),
            pruning_enabled: true,
            pruning_retention_blocks: 100,
            pruning_interval: Duration::from_secs(60),
        };

        assert_eq!(prune_retain_from(&cfg, 0), None);
        assert_eq!(prune_retain_from(&cfg, 100), None);
        assert_eq!(prune_retain_from(&cfg, 101), Some(1));
        assert_eq!(prune_retain_from(&cfg, 250), Some(150));
    }

    #[tokio::test]
    async fn collect_metrics_is_safe_on_live_rocks_store() {
        let tmp = TempDir::new().unwrap();
        let store = RocksStore::open(&StorageConfig::for_testing(tmp.path().join("db"))).unwrap();

        collect_metrics(&store);
    }

    #[tokio::test]
    async fn run_pruning_skips_when_chain_height_is_within_retention() {
        let tmp = TempDir::new().unwrap();
        let store = RocksStore::open(&StorageConfig::for_testing(tmp.path().join("db"))).unwrap();

        let mut batch = store.new_batch();
        for seq in 0u64..3 {
            let key = seq.to_be_bytes().to_vec();
            batch.put_cf(
                ColumnFamily::Blocks.as_str(),
                key.clone(),
                b"block".to_vec(),
            );
            batch.put_cf(
                ColumnFamily::Transactions.as_str(),
                key.clone(),
                b"tx".to_vec(),
            );
            batch.put_cf(ColumnFamily::Receipts.as_str(), key, b"receipt".to_vec());
        }
        store.write_batch(batch).await.unwrap();

        let cfg = StorageMaintenanceConfig {
            metrics_interval: Duration::from_secs(30),
            pruning_enabled: true,
            pruning_retention_blocks: 10,
            pruning_interval: Duration::from_secs(60),
        };
        let chain_height = AtomicU64::new(3);

        run_pruning(&store, &cfg, &chain_height);

        for seq in 0u64..3 {
            let key = seq.to_be_bytes();
            assert!(store
                .get(ColumnFamily::Blocks.as_str(), &key)
                .await
                .unwrap()
                .is_some());
        }
    }

    #[tokio::test]
    async fn run_pruning_removes_entries_older_than_retention_window() {
        let tmp = TempDir::new().unwrap();
        let store = RocksStore::open(&StorageConfig::for_testing(tmp.path().join("db"))).unwrap();

        let mut batch = store.new_batch();
        for seq in 0u64..10 {
            let key = seq.to_be_bytes().to_vec();
            batch.put_cf(
                ColumnFamily::Blocks.as_str(),
                key.clone(),
                b"block".to_vec(),
            );
            batch.put_cf(
                ColumnFamily::Transactions.as_str(),
                key.clone(),
                b"tx".to_vec(),
            );
            batch.put_cf(ColumnFamily::Receipts.as_str(), key, b"receipt".to_vec());
        }
        store.write_batch(batch).await.unwrap();

        let cfg = StorageMaintenanceConfig {
            metrics_interval: Duration::from_secs(30),
            pruning_enabled: true,
            pruning_retention_blocks: 4,
            pruning_interval: Duration::from_secs(60),
        };
        let chain_height = AtomicU64::new(10);

        run_pruning(&store, &cfg, &chain_height);

        for seq in 0u64..6 {
            let key = seq.to_be_bytes();
            assert!(store
                .get(ColumnFamily::Blocks.as_str(), &key)
                .await
                .unwrap()
                .is_none());
        }
        for seq in 6u64..10 {
            let key = seq.to_be_bytes();
            assert!(store
                .get(ColumnFamily::Blocks.as_str(), &key)
                .await
                .unwrap()
                .is_some());
        }
    }
}
