// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Lightweight per-transaction lifecycle tracing for internal benchmarking.
//!
//! Tracks the first observed timestamp for each major pipeline stage so
//! benchmark tooling can distinguish submit, mempool, consensus, and receipt
//! visibility delays instead of treating `tx status` as a single black box.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use nexus_primitives::{TimestampMs, TxDigest};

use crate::dto::TxLifecycleDto;

#[derive(Clone, Debug, Default)]
struct TxLifecycleRecord {
    submit_accepted_at: Option<TimestampMs>,
    mempool_admitted_at: Option<TimestampMs>,
    consensus_included_at: Option<TimestampMs>,
    receipt_visible_at: Option<TimestampMs>,
}

#[derive(Default)]
struct LifecycleState {
    order: VecDeque<TxDigest>,
    records: HashMap<TxDigest, TxLifecycleRecord>,
}

/// Shared in-memory lifecycle tracker keyed by transaction digest.
pub struct TxLifecycleRegistry {
    max_entries: usize,
    state: Mutex<LifecycleState>,
}

impl TxLifecycleRegistry {
    /// Create a tracker with a bounded number of retained digests.
    pub fn new(max_entries: usize) -> Self {
        Self {
            max_entries: max_entries.max(1),
            state: Mutex::new(LifecycleState::default()),
        }
    }

    /// Record that the RPC submit path accepted the transaction.
    pub fn record_submit_accepted(&self, digest: TxDigest) {
        self.update(digest, |record| {
            record
                .submit_accepted_at
                .get_or_insert_with(TimestampMs::now);
        });
    }

    /// Record that the local node admitted the transaction into its mempool.
    pub fn record_mempool_admitted(&self, digest: TxDigest) {
        self.update(digest, |record| {
            record
                .mempool_admitted_at
                .get_or_insert_with(TimestampMs::now);
        });
    }

    /// Record that the transaction was included in a committed consensus batch.
    pub fn record_consensus_included(&self, digest: TxDigest) {
        self.update(digest, |record| {
            record
                .consensus_included_at
                .get_or_insert_with(TimestampMs::now);
        });
    }

    /// Record that the receipt is visible via the local query path.
    pub fn record_receipt_visible(&self, digest: TxDigest) {
        self.update(digest, |record| {
            record
                .receipt_visible_at
                .get_or_insert_with(TimestampMs::now);
        });
    }

    /// Return a snapshot for a single transaction, if retained.
    pub fn snapshot(&self, digest: &TxDigest) -> Option<TxLifecycleDto> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.records.get(digest).map(|record| TxLifecycleDto {
            tx_digest: *digest,
            submit_accepted_at: record.submit_accepted_at,
            mempool_admitted_at: record.mempool_admitted_at,
            consensus_included_at: record.consensus_included_at,
            receipt_visible_at: record.receipt_visible_at,
        })
    }

    fn update(&self, digest: TxDigest, apply: impl FnOnce(&mut TxLifecycleRecord)) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let is_new = !state.records.contains_key(&digest);
        let record = state.records.entry(digest).or_default();
        apply(record);

        if is_new {
            state.order.push_back(digest);
            while state.records.len() > self.max_entries {
                if let Some(oldest) = state.order.pop_front() {
                    state.records.remove(&oldest);
                } else {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(seed: u8) -> TxDigest {
        TxDigest::from_bytes([seed; 32])
    }

    #[test]
    fn first_stage_timestamp_wins() {
        let tracker = TxLifecycleRegistry::new(16);
        let tx = digest(1);

        tracker.record_submit_accepted(tx);
        let first = tracker.snapshot(&tx).unwrap().submit_accepted_at;
        tracker.record_submit_accepted(tx);
        let second = tracker.snapshot(&tx).unwrap().submit_accepted_at;

        assert_eq!(first, second);
    }

    #[test]
    fn oldest_entries_are_pruned() {
        let tracker = TxLifecycleRegistry::new(2);
        let tx1 = digest(1);
        let tx2 = digest(2);
        let tx3 = digest(3);

        tracker.record_submit_accepted(tx1);
        tracker.record_submit_accepted(tx2);
        tracker.record_submit_accepted(tx3);

        assert!(tracker.snapshot(&tx1).is_none());
        assert!(tracker.snapshot(&tx2).is_some());
        assert!(tracker.snapshot(&tx3).is_some());
    }
}
