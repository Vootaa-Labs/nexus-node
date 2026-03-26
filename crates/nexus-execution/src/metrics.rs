// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Execution layer metrics (MVO Rule C — DEV-05 §3).
//!
//! Naming convention: `nexus_execution_<metric>_<unit>`.
//!
//! # Metric Catalogue
//!
//! | Name | Type | Description |
//! |------|------|-------------|
//! | `nexus_execution_transactions_processed_total` | Counter | Successful txs executed |
//! | `nexus_execution_transactions_failed_total` | Counter | Txs that failed / aborted |
//! | `nexus_execution_gas_used_total` | Counter | Cumulative gas consumed |
//! | `nexus_execution_retries_total` | Counter | Block-STM re-executions |
//! | `nexus_execution_errors_total` | Counter | Business-logic errors |
//! | `nexus_execution_batches_processed_total` | Counter | Committed batches executed |
//! | `nexus_execution_block_latency_seconds` | Histogram | Batch execution duration |
//! | `nexus_execution_active_blocks` | Gauge | Concurrent blocks in flight |
//! | `nexus_execution_conflict_rate` | Gauge | Recent conflict fraction (0–1) |
//! | `nexus_execution_queue_depth` | Gauge | Actor mailbox backlog |

use nexus_primitives::ShardId;

/// Per-shard execution metrics handle.
///
/// All metric operations are cheap (label set is pre-allocated).
/// The struct is `Clone` + `Send` + `Sync` via the global `metrics` recorder.
#[derive(Clone)]
pub struct ExecutionMetrics {
    shard: String,
}

impl ExecutionMetrics {
    /// Create a new metrics handle scoped to the given shard.
    pub fn new(shard_id: ShardId) -> Self {
        Self {
            shard: shard_id.0.to_string(),
        }
    }

    // ── Counters ────────────────────────────────────────────────────

    /// Record a completed batch: counters for tx success/fail/gas + latency histogram.
    pub fn record_batch(&self, total_txs: u64, failed_txs: u64, gas_used: u64, elapsed_secs: f64) {
        let labels = [("shard", self.shard.clone())];
        metrics::counter!("nexus_execution_batches_processed_total", &labels).increment(1);
        metrics::counter!("nexus_execution_transactions_processed_total", &labels)
            .increment(total_txs.saturating_sub(failed_txs));
        metrics::counter!("nexus_execution_transactions_failed_total", &labels)
            .increment(failed_txs);
        metrics::counter!("nexus_execution_gas_used_total", &labels).increment(gas_used);
        metrics::histogram!("nexus_execution_block_latency_seconds", &labels).record(elapsed_secs);
    }

    /// Record Block-STM conflicts after the validation phase.
    pub fn record_conflicts(&self, conflict_count: u32, conflict_rate: f64) {
        let labels = [("shard", self.shard.clone())];
        metrics::counter!("nexus_execution_retries_total", &labels)
            .increment(u64::from(conflict_count));
        metrics::gauge!("nexus_execution_conflict_rate", &labels).set(conflict_rate);
    }

    /// Increment the error counter with a category label.
    pub fn record_error(&self, error_type: &str) {
        let labels = [
            ("shard", self.shard.clone()),
            ("type", error_type.to_owned()),
        ];
        metrics::counter!("nexus_execution_errors_total", &labels).increment(1);
    }

    // ── Gauges ──────────────────────────────────────────────────────

    /// Increment the in-flight block gauge (before execution starts).
    pub fn inc_active_blocks(&self) {
        let labels = [("shard", self.shard.clone())];
        metrics::gauge!("nexus_execution_active_blocks", &labels).increment(1.0);
    }

    /// Decrement the in-flight block gauge (after execution completes).
    pub fn dec_active_blocks(&self) {
        let labels = [("shard", self.shard.clone())];
        metrics::gauge!("nexus_execution_active_blocks", &labels).decrement(1.0);
    }

    /// Set the actor mailbox queue depth.
    pub fn set_queue_depth(&self, depth: usize) {
        let labels = [("shard", self.shard.clone())];
        metrics::gauge!("nexus_execution_queue_depth", &labels).set(depth as f64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_handle_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ExecutionMetrics>();
    }

    #[test]
    fn metrics_handle_is_clone() {
        let m = ExecutionMetrics::new(ShardId(0));
        let _m2 = m.clone();
    }

    #[test]
    fn record_batch_does_not_panic_without_recorder() {
        // When no global recorder is installed, metrics macros are no-ops.
        let m = ExecutionMetrics::new(ShardId(42));
        m.record_batch(100, 3, 500_000, 0.045);
        m.record_conflicts(5, 0.1);
        m.record_error("OutOfGas");
        m.inc_active_blocks();
        m.dec_active_blocks();
        m.set_queue_depth(10);
    }
}
