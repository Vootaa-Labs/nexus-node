//! Intent lifecycle tracker.
//!
//! Tracks intents from submission through to on-chain confirmation,
//! providing status queries via `GET /v2/intent/{id}/status`.
//!
//! The tracker maintains:
//! - A forward index: `IntentId → IntentRecord`
//! - A reverse index: `TxDigest → IntentId` (for matching execution receipts)

use std::collections::HashMap;

use nexus_intent::types::IntentStatus;
use nexus_primitives::{IntentId, TimestampMs, TxDigest};
use parking_lot::RwLock;

/// Maximum number of tracked intents before oldest are evicted.
const MAX_TRACKED_INTENTS: usize = 10_000;

/// A tracked intent record.
#[derive(Debug, Clone)]
pub struct IntentRecord {
    /// Unique intent identifier.
    pub intent_id: IntentId,
    /// Current lifecycle status.
    pub status: IntentStatus,
    /// Transaction digests produced by compilation.
    pub tx_hashes: Vec<TxDigest>,
    /// Timestamp when the intent was submitted.
    pub submitted_at: TimestampMs,
    /// Timestamp of the last status update.
    pub updated_at: TimestampMs,
    /// Total gas consumed (populated once all steps are executed).
    pub gas_used: u64,
    /// Number of transactions that have been confirmed on-chain.
    confirmed_count: usize,
    /// Number of transactions that failed on-chain.
    failed_count: usize,
}

/// Thread-safe intent lifecycle tracker.
///
/// Used by the RPC layer to register new intents and by the execution
/// bridge watcher to update status as transactions are confirmed.
pub struct IntentTracker {
    /// Forward index: intent_id → record.
    intents: RwLock<HashMap<IntentId, IntentRecord>>,
    /// Reverse index: tx_digest → intent_id.
    tx_to_intent: RwLock<HashMap<TxDigest, IntentId>>,
    /// Insertion-order tracking for eviction (oldest first).
    insertion_order: RwLock<Vec<IntentId>>,
}

impl IntentTracker {
    /// Create an empty tracker.
    pub fn new() -> Self {
        Self {
            intents: RwLock::new(HashMap::new()),
            tx_to_intent: RwLock::new(HashMap::new()),
            insertion_order: RwLock::new(Vec::new()),
        }
    }

    /// Register a newly submitted intent.
    ///
    /// Called by the `submit_intent` RPC handler after successful
    /// compilation and broadcast.
    pub fn register(&self, intent_id: IntentId, tx_hashes: Vec<TxDigest>) {
        let now = TimestampMs::now();

        // Build reverse index entries
        {
            let mut rev = self.tx_to_intent.write();
            for h in &tx_hashes {
                rev.insert(*h, intent_id);
            }
        }

        let steps = tx_hashes.len();
        let record = IntentRecord {
            intent_id,
            status: IntentStatus::Submitted { steps },
            tx_hashes,
            submitted_at: now,
            updated_at: now,
            gas_used: 0,
            confirmed_count: 0,
            failed_count: 0,
        };

        {
            let mut fwd = self.intents.write();
            fwd.insert(intent_id, record);
        }
        {
            let mut order = self.insertion_order.write();
            order.push(intent_id);
        }

        // Evict oldest if at capacity
        self.maybe_evict();
    }

    /// Notify the tracker that a transaction was executed on-chain.
    ///
    /// Returns the `(IntentId, IntentStatus)` if the tx belongs to a tracked
    /// intent, or `None` if untracked.
    pub fn on_tx_executed(
        &self,
        tx_digest: &TxDigest,
        gas_used: u64,
    ) -> Option<(IntentId, IntentStatus)> {
        let intent_id = {
            let rev = self.tx_to_intent.read();
            rev.get(tx_digest).copied()?
        };

        let mut fwd = self.intents.write();
        let record = fwd.get_mut(&intent_id)?;

        // Only update if still in Submitted state
        if !matches!(record.status, IntentStatus::Submitted { .. }) {
            return Some((intent_id, record.status.clone()));
        }

        record.confirmed_count += 1;
        record.gas_used += gas_used;
        record.updated_at = TimestampMs::now();

        // All steps confirmed → Completed
        if record.confirmed_count >= record.tx_hashes.len() {
            record.status = IntentStatus::Completed {
                gas_used: record.gas_used,
            };
        }

        Some((intent_id, record.status.clone()))
    }

    /// Notify the tracker that a transaction failed on-chain.
    ///
    /// Any single failure marks the entire intent as Failed.
    pub fn on_tx_failed(
        &self,
        tx_digest: &TxDigest,
        reason: String,
    ) -> Option<(IntentId, IntentStatus)> {
        let intent_id = {
            let rev = self.tx_to_intent.read();
            rev.get(tx_digest).copied()?
        };

        let mut fwd = self.intents.write();
        let record = fwd.get_mut(&intent_id)?;

        // Only update if still in Submitted state
        if !matches!(record.status, IntentStatus::Submitted { .. }) {
            return Some((intent_id, record.status.clone()));
        }

        record.failed_count += 1;
        record.updated_at = TimestampMs::now();
        record.status = IntentStatus::Failed { reason };

        Some((intent_id, record.status.clone()))
    }

    /// Look up the current status of a tracked intent.
    pub fn status(&self, intent_id: &IntentId) -> Option<IntentRecord> {
        let fwd = self.intents.read();
        fwd.get(intent_id).cloned()
    }

    /// Evict the oldest entries if tracking capacity is exceeded.
    fn maybe_evict(&self) {
        let mut order = self.insertion_order.write();
        if order.len() <= MAX_TRACKED_INTENTS {
            return;
        }

        let to_evict = order.len() - MAX_TRACKED_INTENTS;
        let evict_ids: Vec<IntentId> = order.drain(..to_evict).collect();

        let mut fwd = self.intents.write();
        let mut rev = self.tx_to_intent.write();
        for id in evict_ids {
            if let Some(record) = fwd.remove(&id) {
                for h in &record.tx_hashes {
                    rev.remove(h);
                }
            }
        }
    }
}

impl Default for IntentTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_primitives::Blake3Digest;

    fn make_intent_id(seed: u8) -> IntentId {
        Blake3Digest([seed; 32])
    }

    fn make_tx_digest(seed: u8) -> TxDigest {
        TxDigest::from_bytes([seed; 32])
    }

    #[test]
    fn register_and_query_status() {
        let tracker = IntentTracker::new();
        let id = make_intent_id(0xAA);
        let hashes = vec![make_tx_digest(0x01), make_tx_digest(0x02)];

        tracker.register(id, hashes.clone());

        let record = tracker.status(&id).unwrap();
        assert_eq!(record.intent_id, id);
        assert_eq!(record.tx_hashes, hashes);
        assert!(matches!(
            record.status,
            IntentStatus::Submitted { steps: 2 }
        ));
    }

    #[test]
    fn on_tx_executed_transitions_to_completed() {
        let tracker = IntentTracker::new();
        let id = make_intent_id(0xBB);
        let h1 = make_tx_digest(0x01);
        let h2 = make_tx_digest(0x02);

        tracker.register(id, vec![h1, h2]);

        // First tx executed — still Submitted
        let (_id, status) = tracker.on_tx_executed(&h1, 500).unwrap();
        assert!(matches!(status, IntentStatus::Submitted { .. }));

        // Second tx executed — now Completed
        let (_id, status) = tracker.on_tx_executed(&h2, 300).unwrap();
        assert!(matches!(status, IntentStatus::Completed { gas_used: 800 }));
    }

    #[test]
    fn on_tx_failed_transitions_to_failed() {
        let tracker = IntentTracker::new();
        let id = make_intent_id(0xCC);
        let h1 = make_tx_digest(0x01);
        let h2 = make_tx_digest(0x02);

        tracker.register(id, vec![h1, h2]);

        let (_id, status) = tracker.on_tx_failed(&h1, "out of gas".into()).unwrap();
        assert!(matches!(status, IntentStatus::Failed { .. }));

        // Further execution doesn't change Failed status
        let (_id, status) = tracker.on_tx_executed(&h2, 100).unwrap();
        assert!(matches!(status, IntentStatus::Failed { .. }));
    }

    #[test]
    fn untracked_tx_returns_none() {
        let tracker = IntentTracker::new();
        let unknown = make_tx_digest(0xFF);

        assert!(tracker.on_tx_executed(&unknown, 100).is_none());
        assert!(tracker.on_tx_failed(&unknown, "err".into()).is_none());
    }

    #[test]
    fn untracked_intent_returns_none() {
        let tracker = IntentTracker::new();
        let unknown = make_intent_id(0xFF);

        assert!(tracker.status(&unknown).is_none());
    }
}
