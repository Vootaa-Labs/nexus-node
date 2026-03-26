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

fn run_pruning(store: &RocksStore, config: &StorageMaintenanceConfig, chain_height: &AtomicU64) {
    let height = chain_height.load(Ordering::Relaxed);
    if height <= config.pruning_retention_blocks {
        debug!(
            height,
            retention = config.pruning_retention_blocks,
            "not enough blocks to prune"
        );
        return;
    }
    let retain_from = height - config.pruning_retention_blocks;
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
