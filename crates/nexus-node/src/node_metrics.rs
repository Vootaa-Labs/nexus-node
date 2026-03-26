//! Node-level pipeline metrics.
//!
//! Captures the operational health of the assembly layer: batch proposer,
//! consensus bridge, execution bridge, and mempool depth.
//!
//! # Metric Catalogue
//!
//! | Name | Type | Description |
//! |------|------|-------------|
//! | `nexus_mempool_pending_transactions` | Gauge | Current mempool depth |
//! | `nexus_mempool_enqueue_total` | Counter | Transactions entering mempool |
//! | `nexus_mempool_dequeue_total` | Counter | Transactions drained into batches |
//! | `nexus_batch_proposals_total` | Counter | Batch proposals submitted |
//! | `nexus_batch_proposal_round` | Gauge | Current proposal round number |
//! | `nexus_batch_proposal_txs_total` | Counter | Total transactions proposed in batches |
//! | `nexus_consensus_certificates_accepted_total` | Counter | Certs inserted into DAG |
//! | `nexus_consensus_certificates_committed_total` | Counter | Certs that triggered commits |
//! | `nexus_consensus_certificates_rejected_total` | Counter | Certs rejected |
//! | `nexus_consensus_current_round` | Gauge | Highest observed consensus round |
//! | `nexus_bridge_batches_executed_total` | Counter | Committed batches passed to execution |
//! | `nexus_bridge_execution_latency_seconds` | Histogram | Time to execute a committed batch |
//! | `nexus_bridge_persist_latency_seconds` | Histogram | Time to persist results to storage |
//! | `nexus_provenance_active_sessions` | Gauge | Currently active agent sessions |
//! | `nexus_provenance_records_total` | Counter | Provenance records created |
//! | `nexus_provenance_anchors_total` | Counter | Anchor batches submitted |

// ── Mempool ─────────────────────────────────────────────────────────────

pub fn mempool_pending(depth: usize) {
    metrics::gauge!("nexus_mempool_pending_transactions").set(depth as f64);
}

pub fn mempool_enqueue(count: u64) {
    metrics::counter!("nexus_mempool_enqueue_total").increment(count);
}

pub fn mempool_dequeue(count: u64) {
    metrics::counter!("nexus_mempool_dequeue_total").increment(count);
}

// ── Batch Proposer ──────────────────────────────────────────────────────

pub fn batch_proposed(round: u64, tx_count: u64) {
    metrics::counter!("nexus_batch_proposals_total").increment(1);
    metrics::gauge!("nexus_batch_proposal_round").set(round as f64);
    metrics::counter!("nexus_batch_proposal_txs_total").increment(tx_count);
}

// ── Consensus Bridge ────────────────────────────────────────────────────

pub fn consensus_cert_accepted(round: u64) {
    metrics::counter!("nexus_consensus_certificates_accepted_total").increment(1);
    metrics::gauge!("nexus_consensus_current_round").set(round as f64);
}

pub fn consensus_cert_committed() {
    metrics::counter!("nexus_consensus_certificates_committed_total").increment(1);
}

pub fn consensus_cert_rejected() {
    metrics::counter!("nexus_consensus_certificates_rejected_total").increment(1);
}

// ── Execution Bridge ────────────────────────────────────────────────────

pub fn bridge_batch_executed(latency_secs: f64) {
    metrics::counter!("nexus_bridge_batches_executed_total").increment(1);
    metrics::histogram!("nexus_bridge_execution_latency_seconds").record(latency_secs);
}

pub fn bridge_persist_latency(latency_secs: f64) {
    metrics::histogram!("nexus_bridge_persist_latency_seconds").record(latency_secs);
}

// ── Session / Provenance ────────────────────────────────────────────────

pub fn provenance_active_sessions(count: usize) {
    metrics::gauge!("nexus_provenance_active_sessions").set(count as f64);
}

pub fn provenance_record_created() {
    metrics::counter!("nexus_provenance_records_total").increment(1);
}

pub fn provenance_anchor_submitted() {
    metrics::counter!("nexus_provenance_anchors_total").increment(1);
}

// ── Storage (P5-3) ──────────────────────────────────────────────────────

pub fn storage_cf_sst_size(cf: &str, bytes: u64) {
    metrics::gauge!("nexus_storage_sst_file_size_bytes", "cf" => cf.to_owned()).set(bytes as f64);
}

pub fn storage_cf_memtable_size(cf: &str, bytes: u64) {
    metrics::gauge!("nexus_storage_memtable_size_bytes", "cf" => cf.to_owned()).set(bytes as f64);
}

pub fn storage_cf_estimated_keys(cf: &str, keys: u64) {
    metrics::gauge!("nexus_storage_estimated_keys", "cf" => cf.to_owned()).set(keys as f64);
}

pub fn storage_pruned(blocks: u64, txs: u64, receipts: u64) {
    metrics::counter!("nexus_storage_blocks_pruned_total").increment(blocks);
    metrics::counter!("nexus_storage_transactions_pruned_total").increment(txs);
    metrics::counter!("nexus_storage_receipts_pruned_total").increment(receipts);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_metrics_safe_without_recorder() {
        mempool_pending(42);
        mempool_enqueue(10);
        mempool_dequeue(5);
        batch_proposed(1, 3);
        consensus_cert_accepted(10);
        consensus_cert_committed();
        consensus_cert_rejected();
        bridge_batch_executed(0.05);
        bridge_persist_latency(0.01);
        provenance_active_sessions(3);
        provenance_record_created();
        provenance_anchor_submitted();
        storage_cf_sst_size("cf_state", 1024);
        storage_cf_memtable_size("cf_state", 512);
        storage_cf_estimated_keys("cf_state", 100);
        storage_pruned(5, 10, 15);
    }
}
