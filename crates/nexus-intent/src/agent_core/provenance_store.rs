//! In-memory provenance store with indexed views and activity feed.
//!
//! Provides filtered queries by agent, session, capability token,
//! and time range. Supports pagination via cursor.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use nexus_primitives::{AccountAddress, Blake3Digest, TokenId};

use crate::agent_core::provenance::{
    ProvenanceQueryParams, ProvenanceQueryResult, ProvenanceRecord, ProvenanceStatus,
};

// ── ProvenanceStore ────────────────────────────────────────────────────

/// In-memory provenance store with secondary indexes.
pub struct ProvenanceStore {
    inner: Mutex<StoreInner>,
}

struct StoreInner {
    /// Primary store: provenance_id → record.
    records: HashMap<Blake3Digest, ProvenanceRecord>,
    /// Chronological order for pagination (VecDeque for O(1) front eviction).
    ordered: VecDeque<Blake3Digest>,
    /// Index: agent_id → provenance_ids.
    by_agent: HashMap<AccountAddress, Vec<Blake3Digest>>,
    /// Index: session_id → provenance_ids.
    by_session: HashMap<Blake3Digest, Vec<Blake3Digest>>,
    /// Index: capability token → provenance_ids.
    by_capability: HashMap<TokenId, Vec<Blake3Digest>>,
    /// Maximum number of records (0 = unlimited).
    max_records: usize,
}

impl ProvenanceStore {
    /// Create a new empty provenance store with a default capacity of
    /// 100 000 records.
    pub fn new() -> Self {
        Self::with_max_records(100_000)
    }

    /// Create a provenance store with an explicit record cap.
    ///
    /// When the cap is reached, the oldest record is evicted on each
    /// new insertion.  A value of `0` disables the cap.
    pub fn with_max_records(max_records: usize) -> Self {
        Self {
            inner: Mutex::new(StoreInner {
                records: HashMap::new(),
                ordered: VecDeque::new(),
                by_agent: HashMap::new(),
                by_session: HashMap::new(),
                by_capability: HashMap::new(),
                max_records,
            }),
        }
    }

    /// Record a new provenance entry.
    ///
    /// Indexes are updated automatically.
    pub fn record(&self, record: ProvenanceRecord) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());

        // Evict oldest record when at capacity.
        if inner.max_records > 0 && inner.records.len() >= inner.max_records {
            if let Some(oldest_id) = inner.ordered.pop_front() {
                if let Some(old) = inner.records.remove(&oldest_id) {
                    // Clean up secondary indexes.
                    if let Some(v) = inner.by_agent.get_mut(&old.agent_id) {
                        v.retain(|id| *id != oldest_id);
                    }
                    if let Some(v) = inner.by_session.get_mut(&old.session_id) {
                        v.retain(|id| *id != oldest_id);
                    }
                    if let Some(tok) = &old.capability_token_id {
                        if let Some(v) = inner.by_capability.get_mut(tok) {
                            v.retain(|id| *id != oldest_id);
                        }
                    }
                }
            }
        }

        let id = record.provenance_id;

        // Update indexes.
        inner.by_agent.entry(record.agent_id).or_default().push(id);
        inner
            .by_session
            .entry(record.session_id)
            .or_default()
            .push(id);
        if let Some(token) = &record.capability_token_id {
            inner.by_capability.entry(*token).or_default().push(id);
        }

        inner.ordered.push_back(id);
        inner.records.insert(id, record);
    }

    /// Update the status of an existing record.
    pub fn update_status(&self, provenance_id: &Blake3Digest, status: ProvenanceStatus) -> bool {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(record) = inner.records.get_mut(provenance_id) {
            record.status = status;
            true
        } else {
            false
        }
    }

    /// Update the tx_hash of an existing record.
    pub fn set_tx_hash(&self, provenance_id: &Blake3Digest, tx_hash: Blake3Digest) -> bool {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(record) = inner.records.get_mut(provenance_id) {
            record.tx_hash = Some(tx_hash);
            true
        } else {
            false
        }
    }

    /// Query by agent ID with pagination.
    pub fn query_by_agent(
        &self,
        agent: &AccountAddress,
        params: &ProvenanceQueryParams,
    ) -> ProvenanceQueryResult {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let ids = inner.by_agent.get(agent).map(Vec::as_slice).unwrap_or(&[]);
        Self::paginate(&inner.records, ids, params)
    }

    /// Query by session ID with pagination.
    pub fn query_by_session(
        &self,
        session_id: &Blake3Digest,
        params: &ProvenanceQueryParams,
    ) -> ProvenanceQueryResult {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let ids = inner
            .by_session
            .get(session_id)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        Self::paginate(&inner.records, ids, params)
    }

    /// Query by capability token with pagination.
    pub fn query_by_capability(
        &self,
        token: &TokenId,
        params: &ProvenanceQueryParams,
    ) -> ProvenanceQueryResult {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let ids = inner
            .by_capability
            .get(token)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        Self::paginate(&inner.records, ids, params)
    }

    /// Get full chronological activity feed with pagination.
    pub fn activity_feed(&self, params: &ProvenanceQueryParams) -> ProvenanceQueryResult {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let ids: Vec<Blake3Digest> = inner.ordered.iter().copied().collect();
        Self::paginate(&inner.records, &ids, params)
    }

    /// Lookup a single record by provenance_id.
    pub fn get(&self, provenance_id: &Blake3Digest) -> Option<ProvenanceRecord> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.records.get(provenance_id).cloned()
    }

    /// Total record count.
    pub fn len(&self) -> usize {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.records.len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Internal pagination helper.
    fn paginate(
        records: &HashMap<Blake3Digest, ProvenanceRecord>,
        ids: &[Blake3Digest],
        params: &ProvenanceQueryParams,
    ) -> ProvenanceQueryResult {
        // Find cursor position.  If the cursor is provided but not found,
        // start from the beginning rather than silently repeating results.
        let start = if let Some(ref cursor) = params.cursor {
            match ids.iter().position(|id| id == cursor) {
                Some(pos) => pos + 1,
                None => {
                    // Cursor deleted or stale — return empty result so caller
                    // can re-paginate from the beginning.
                    return ProvenanceQueryResult {
                        records: vec![],
                        total_count: 0,
                        cursor: None,
                    };
                }
            }
        } else {
            0
        };

        let filtered: Vec<ProvenanceRecord> = ids[start..]
            .iter()
            .filter_map(|id| records.get(id))
            .filter(|r| {
                if let Some(after) = params.after_ms {
                    if r.created_at_ms.0 < after.0 {
                        return false;
                    }
                }
                if let Some(before) = params.before_ms {
                    if r.created_at_ms.0 > before.0 {
                        return false;
                    }
                }
                true
            })
            .take(params.limit as usize)
            .cloned()
            .collect();

        let total_count = ids
            .iter()
            .filter_map(|id| records.get(id))
            .filter(|r| {
                params
                    .after_ms
                    .is_none_or(|after| r.created_at_ms.0 >= after.0)
                    && params
                        .before_ms
                        .is_none_or(|before| r.created_at_ms.0 <= before.0)
            })
            .count() as u64;

        let cursor = filtered.last().map(|r| r.provenance_id);

        ProvenanceQueryResult {
            records: filtered,
            total_count,
            cursor,
        }
    }
}

impl Default for ProvenanceStore {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_primitives::TimestampMs;

    fn addr(b: u8) -> AccountAddress {
        AccountAddress([b; 32])
    }

    fn make_record(idx: u8, agent: u8, session: u8, time: u64) -> ProvenanceRecord {
        ProvenanceRecord {
            provenance_id: Blake3Digest([idx; 32]),
            session_id: Blake3Digest([session; 32]),
            request_id: Blake3Digest([idx; 32]),
            agent_id: addr(agent),
            parent_agent_id: None,
            capability_token_id: None,
            intent_hash: Blake3Digest([0x00; 32]),
            plan_hash: Blake3Digest([0x00; 32]),
            confirmation_ref: None,
            tx_hash: None,
            status: ProvenanceStatus::Pending,
            created_at_ms: TimestampMs(time),
        }
    }

    fn default_params() -> ProvenanceQueryParams {
        ProvenanceQueryParams {
            limit: 100,
            cursor: None,
            after_ms: None,
            before_ms: None,
        }
    }

    #[test]
    fn record_and_retrieve() {
        let store = ProvenanceStore::new();
        let rec = make_record(0x01, 0xAA, 0x10, 1_000);
        store.record(rec.clone());
        assert_eq!(store.get(&Blake3Digest([0x01; 32])), Some(rec));
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn query_by_agent() {
        let store = ProvenanceStore::new();
        store.record(make_record(0x01, 0xAA, 0x10, 1_000));
        store.record(make_record(0x02, 0xBB, 0x20, 2_000));
        store.record(make_record(0x03, 0xAA, 0x30, 3_000));

        let result = store.query_by_agent(&addr(0xAA), &default_params());
        assert_eq!(result.records.len(), 2);
        assert_eq!(result.total_count, 2);
    }

    #[test]
    fn query_by_session() {
        let store = ProvenanceStore::new();
        store.record(make_record(0x01, 0xAA, 0x10, 1_000));
        store.record(make_record(0x02, 0xBB, 0x10, 2_000));

        let result = store.query_by_session(&Blake3Digest([0x10; 32]), &default_params());
        assert_eq!(result.records.len(), 2);
    }

    #[test]
    fn query_by_capability() {
        let store = ProvenanceStore::new();
        let mut rec = make_record(0x01, 0xAA, 0x10, 1_000);
        rec.capability_token_id = Some(TokenId::Native);
        store.record(rec);
        store.record(make_record(0x02, 0xBB, 0x20, 2_000)); // no token

        let result = store.query_by_capability(&TokenId::Native, &default_params());
        assert_eq!(result.records.len(), 1);
    }

    #[test]
    fn activity_feed_chronological() {
        let store = ProvenanceStore::new();
        store.record(make_record(0x01, 0xAA, 0x10, 1_000));
        store.record(make_record(0x02, 0xBB, 0x20, 2_000));
        store.record(make_record(0x03, 0xCC, 0x30, 3_000));

        let result = store.activity_feed(&default_params());
        assert_eq!(result.records.len(), 3);
        assert_eq!(result.records[0].created_at_ms, TimestampMs(1_000));
        assert_eq!(result.records[2].created_at_ms, TimestampMs(3_000));
    }

    #[test]
    fn pagination_with_cursor() {
        let store = ProvenanceStore::new();
        store.record(make_record(0x01, 0xAA, 0x10, 1_000));
        store.record(make_record(0x02, 0xAA, 0x20, 2_000));
        store.record(make_record(0x03, 0xAA, 0x30, 3_000));

        // Page 1: limit 2.
        let mut params = default_params();
        params.limit = 2;
        let page1 = store.activity_feed(&params);
        assert_eq!(page1.records.len(), 2);
        assert!(page1.cursor.is_some());

        // Page 2: from cursor.
        params.cursor = page1.cursor;
        let page2 = store.activity_feed(&params);
        assert_eq!(page2.records.len(), 1);
    }

    #[test]
    fn time_range_filter() {
        let store = ProvenanceStore::new();
        store.record(make_record(0x01, 0xAA, 0x10, 1_000));
        store.record(make_record(0x02, 0xAA, 0x20, 2_000));
        store.record(make_record(0x03, 0xAA, 0x30, 3_000));

        let params = ProvenanceQueryParams {
            limit: 100,
            cursor: None,
            after_ms: Some(TimestampMs(1_500)),
            before_ms: Some(TimestampMs(2_500)),
        };
        let result = store.activity_feed(&params);
        assert_eq!(result.records.len(), 1);
        assert_eq!(result.records[0].created_at_ms, TimestampMs(2_000));
    }

    #[test]
    fn update_status() {
        let store = ProvenanceStore::new();
        store.record(make_record(0x01, 0xAA, 0x10, 1_000));

        assert!(store.update_status(&Blake3Digest([0x01; 32]), ProvenanceStatus::Committed));
        let rec = store.get(&Blake3Digest([0x01; 32])).unwrap();
        assert_eq!(rec.status, ProvenanceStatus::Committed);
    }

    #[test]
    fn set_tx_hash() {
        let store = ProvenanceStore::new();
        store.record(make_record(0x01, 0xAA, 0x10, 1_000));

        let tx = Blake3Digest([0xFF; 32]);
        assert!(store.set_tx_hash(&Blake3Digest([0x01; 32]), tx));
        let rec = store.get(&Blake3Digest([0x01; 32])).unwrap();
        assert_eq!(rec.tx_hash, Some(tx));
    }

    #[test]
    fn empty_store_queries_work() {
        let store = ProvenanceStore::new();
        assert!(store.is_empty());
        let result = store.activity_feed(&default_params());
        assert_eq!(result.records.len(), 0);
        assert_eq!(result.total_count, 0);
    }

    #[test]
    fn update_nonexistent_returns_false() {
        let store = ProvenanceStore::new();
        assert!(!store.update_status(&Blake3Digest([0x99; 32]), ProvenanceStatus::Failed));
        assert!(!store.set_tx_hash(&Blake3Digest([0x99; 32]), Blake3Digest([0xFF; 32])));
    }

    #[test]
    fn stale_cursor_returns_empty() {
        let store = ProvenanceStore::new();
        store.record(make_record(0x01, 0xAA, 0x10, 1_000));
        store.record(make_record(0x02, 0xAA, 0x20, 2_000));

        // Use a cursor that does not exist in the store.
        let params = ProvenanceQueryParams {
            limit: 100,
            cursor: Some(Blake3Digest([0xFF; 32])),
            after_ms: None,
            before_ms: None,
        };
        let result = store.activity_feed(&params);
        assert_eq!(result.records.len(), 0);
        assert_eq!(result.total_count, 0);
        assert!(result.cursor.is_none());
    }
}
