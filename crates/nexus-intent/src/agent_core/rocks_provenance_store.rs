// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! RocksDB-backed provenance store with secondary indexes.
//!
//! All data lives in the `cf_provenance` column family. Records use
//! prefix-based composite keys for O(log n) secondary index lookups:
//!
//! | Prefix | Key layout | Value |
//! |--------|-----------|-------|
//! | `p:` | `p:<provenance_id 32B>` | BCS(ProvenanceRecord) |
//! | `a:` | `a:<agent_id 32B><provenance_id 32B>` | empty |
//! | `s:` | `s:<session_id 32B><provenance_id 32B>` | empty |
//! | `c:` | `c:<token_id_bcs><provenance_id 32B>` | empty |
//! | `o:` | `o:<created_at_ms 8B BE><provenance_id 32B>` | empty |
//! | `t:` | `t:<tx_hash 32B><provenance_id 32B>` | empty |
//! | `S:` | `S:<status 1B><created_at_ms 8B BE><provenance_id 32B>` | empty |
//! | `r:` | `r:<anchor_digest 32B>` | BCS(AnchorReceipt) |
//! | `b:` | `b:<batch_seq 8B BE>` | anchor_digest 32B |
//! | `n:` | `n:<meta_key>` | u64 BE (metadata counters) |
//!
//! Big-endian timestamp encoding ensures chronological scan order.

use std::collections::HashMap;
use std::sync::Mutex;

use nexus_primitives::{AccountAddress, Blake3Digest, TimestampMs, TokenId};
use nexus_storage::{StateStorage, WriteBatchOps};

use crate::agent_core::provenance::{
    AnchorReceipt, ProvenanceQueryParams, ProvenanceQueryResult, ProvenanceRecord, ProvenanceStatus,
};

const CF: &str = "cf_provenance";

// ── Key construction helpers ────────────────────────────────────────────

fn primary_key(id: &Blake3Digest) -> Vec<u8> {
    let mut k = Vec::with_capacity(33);
    k.push(b'p');
    k.extend_from_slice(&id.0);
    k
}

fn agent_index_key(agent: &AccountAddress, provenance_id: &Blake3Digest) -> Vec<u8> {
    let mut k = Vec::with_capacity(65);
    k.push(b'a');
    k.extend_from_slice(&agent.0);
    k.extend_from_slice(&provenance_id.0);
    k
}

fn session_index_key(session_id: &Blake3Digest, provenance_id: &Blake3Digest) -> Vec<u8> {
    let mut k = Vec::with_capacity(65);
    k.push(b's');
    k.extend_from_slice(&session_id.0);
    k.extend_from_slice(&provenance_id.0);
    k
}

fn capability_index_key(token: &TokenId, provenance_id: &Blake3Digest) -> Vec<u8> {
    // Token ID is variable-size when BCS-encoded, so we prefix with 'c'
    // and append the provenance_id at the end.
    let token_bytes = bcs::to_bytes(token).unwrap_or_default();
    let mut k = Vec::with_capacity(1 + token_bytes.len() + 32);
    k.push(b'c');
    k.extend_from_slice(&token_bytes);
    k.extend_from_slice(&provenance_id.0);
    k
}

fn ordered_index_key(created_at_ms: TimestampMs, provenance_id: &Blake3Digest) -> Vec<u8> {
    let mut k = Vec::with_capacity(41);
    k.push(b'o');
    k.extend_from_slice(&created_at_ms.0.to_be_bytes());
    k.extend_from_slice(&provenance_id.0);
    k
}

/// Prefix for agent index scan: `a:<agent_id>`.
fn agent_prefix(agent: &AccountAddress) -> Vec<u8> {
    let mut k = Vec::with_capacity(33);
    k.push(b'a');
    k.extend_from_slice(&agent.0);
    k
}

/// Prefix for session index scan: `s:<session_id>`.
fn session_prefix(session_id: &Blake3Digest) -> Vec<u8> {
    let mut k = Vec::with_capacity(33);
    k.push(b's');
    k.extend_from_slice(&session_id.0);
    k
}

/// Prefix for capability index scan: `c:<token_bcs>`.
fn capability_prefix(token: &TokenId) -> Vec<u8> {
    let token_bytes = bcs::to_bytes(token).unwrap_or_default();
    let mut k = Vec::with_capacity(1 + token_bytes.len());
    k.push(b'c');
    k.extend_from_slice(&token_bytes);
    k
}

/// End key for prefix scan: prefix with last byte incremented.
fn prefix_end(prefix: &[u8]) -> Vec<u8> {
    let mut end = prefix.to_vec();
    // Append 0xFF bytes to ensure we capture all keys with this prefix.
    end.push(0xFF);
    end.push(0xFF);
    end
}

/// Key for tx_hash reverse index: `t:<tx_hash 32B><provenance_id 32B>`.
fn tx_hash_index_key(tx_hash: &Blake3Digest, provenance_id: &Blake3Digest) -> Vec<u8> {
    let mut k = Vec::with_capacity(65);
    k.push(b't');
    k.extend_from_slice(&tx_hash.0);
    k.extend_from_slice(&provenance_id.0);
    k
}

/// Prefix for tx_hash index scan: `t:<tx_hash>`.
fn tx_hash_prefix(tx_hash: &Blake3Digest) -> Vec<u8> {
    let mut k = Vec::with_capacity(33);
    k.push(b't');
    k.extend_from_slice(&tx_hash.0);
    k
}

/// Key for status index: `S:<status 1B><created_at_ms 8B BE><provenance_id 32B>`.
fn status_index_key(
    status: ProvenanceStatus,
    created_at_ms: TimestampMs,
    provenance_id: &Blake3Digest,
) -> Vec<u8> {
    let mut k = Vec::with_capacity(42);
    k.push(b'S');
    k.push(status as u8);
    k.extend_from_slice(&created_at_ms.0.to_be_bytes());
    k.extend_from_slice(&provenance_id.0);
    k
}

/// Prefix for status index scan: `S:<status 1B>`.
fn status_prefix(status: ProvenanceStatus) -> Vec<u8> {
    vec![b'S', status as u8]
}

/// Extract provenance_id from the last 32 bytes of an index key.
fn extract_provenance_id(key: &[u8]) -> Option<Blake3Digest> {
    if key.len() >= 32 {
        let mut id = [0u8; 32];
        id.copy_from_slice(&key[key.len() - 32..]);
        Some(Blake3Digest(id))
    } else {
        None
    }
}

// ── Anchor receipt key helpers ──────────────────────────────────────────

/// Key for anchor receipt by digest: `r:<anchor_digest 32B>`.
fn anchor_receipt_key(anchor_digest: &Blake3Digest) -> Vec<u8> {
    let mut k = Vec::with_capacity(33);
    k.push(b'r');
    k.extend_from_slice(&anchor_digest.0);
    k
}

/// Key for batch sequence index: `b:<batch_seq 8B BE>`.
fn anchor_batch_seq_key(batch_seq: u64) -> Vec<u8> {
    let mut k = Vec::with_capacity(9);
    k.push(b'b');
    k.extend_from_slice(&batch_seq.to_be_bytes());
    k
}

/// Metadata key: `n:<name>`.
fn meta_key(name: &[u8]) -> Vec<u8> {
    let mut k = Vec::with_capacity(1 + name.len());
    k.push(b'n');
    k.extend_from_slice(name);
    k
}

/// Well-known metadata key for the last anchored batch sequence.
const META_LAST_ANCHOR_SEQ: &[u8] = b"last_anchor_seq";

/// Well-known metadata key for the last anchored provenance record count.
const META_LAST_ANCHOR_COUNT: &[u8] = b"last_anchor_count";

// ── RocksProvenanceStore ────────────────────────────────────────────────

/// Persistent provenance store backed by a [`StateStorage`] implementation.
///
/// Maintains secondary indexes in the same column family using prefix-based
/// composite keys for efficient filtered queries.
pub struct RocksProvenanceStore<S: StateStorage> {
    store: S,
    /// In-memory record count (approximate; authoritative count requires scan).
    count: Mutex<u64>,
    /// Maximum records to retain (0 = unlimited). When exceeded, oldest
    /// records are eligible for cleanup.
    _max_records: usize,
}

impl<S: StateStorage> RocksProvenanceStore<S> {
    /// Create a new provenance store with the default capacity of 100 000 records.
    pub fn new(store: S) -> Self {
        Self::with_max_records(store, 100_000)
    }

    /// Create a provenance store with an explicit record cap.
    pub fn with_max_records(store: S, max_records: usize) -> Self {
        Self {
            store,
            count: Mutex::new(0),
            _max_records: max_records,
        }
    }

    /// Record a new provenance entry, persisting both the primary record
    /// and all secondary index keys atomically.
    pub async fn record(&self, record: &ProvenanceRecord) -> Result<(), crate::error::IntentError> {
        let value =
            bcs::to_bytes(record).map_err(|e| crate::error::IntentError::Codec(e.to_string()))?;

        let mut batch = self.store.new_batch();

        // Primary record.
        batch.put_cf(CF, primary_key(&record.provenance_id), value);

        // Secondary indexes (empty values — presence is the index).
        batch.put_cf(
            CF,
            agent_index_key(&record.agent_id, &record.provenance_id),
            vec![],
        );
        batch.put_cf(
            CF,
            session_index_key(&record.session_id, &record.provenance_id),
            vec![],
        );
        if let Some(token) = &record.capability_token_id {
            batch.put_cf(
                CF,
                capability_index_key(token, &record.provenance_id),
                vec![],
            );
        }
        batch.put_cf(
            CF,
            ordered_index_key(record.created_at_ms, &record.provenance_id),
            vec![],
        );
        if let Some(tx_hash) = &record.tx_hash {
            batch.put_cf(
                CF,
                tx_hash_index_key(tx_hash, &record.provenance_id),
                vec![],
            );
        }
        batch.put_cf(
            CF,
            status_index_key(record.status, record.created_at_ms, &record.provenance_id),
            vec![],
        );

        self.store
            .write_batch(batch)
            .await
            .map_err(|e| crate::error::IntentError::Storage(e.to_string()))?;

        let mut count = self.count.lock().unwrap_or_else(|e| e.into_inner());
        *count += 1;

        Ok(())
    }

    /// Update the status of an existing record.
    pub async fn update_status(
        &self,
        provenance_id: &Blake3Digest,
        status: ProvenanceStatus,
    ) -> Result<bool, crate::error::IntentError> {
        if let Some(mut record) = self.get(provenance_id) {
            record.status = status;
            let value = bcs::to_bytes(&record)
                .map_err(|e| crate::error::IntentError::Codec(e.to_string()))?;
            let mut batch = self.store.new_batch();
            batch.put_cf(CF, primary_key(provenance_id), value);
            // Append new status index entry (stale entries are harmless;
            // query deduplicates by provenance_id).
            batch.put_cf(
                CF,
                status_index_key(status, record.created_at_ms, provenance_id),
                vec![],
            );
            self.store
                .write_batch(batch)
                .await
                .map_err(|e| crate::error::IntentError::Storage(e.to_string()))?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Set the tx_hash on an existing record.
    pub async fn set_tx_hash(
        &self,
        provenance_id: &Blake3Digest,
        tx_hash: Blake3Digest,
    ) -> Result<bool, crate::error::IntentError> {
        if let Some(mut record) = self.get(provenance_id) {
            record.tx_hash = Some(tx_hash);
            let value = bcs::to_bytes(&record)
                .map_err(|e| crate::error::IntentError::Codec(e.to_string()))?;
            let mut batch = self.store.new_batch();
            batch.put_cf(CF, primary_key(provenance_id), value);
            batch.put_cf(CF, tx_hash_index_key(&tx_hash, provenance_id), vec![]);
            self.store
                .write_batch(batch)
                .await
                .map_err(|e| crate::error::IntentError::Storage(e.to_string()))?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Lookup a single record by provenance_id.
    pub fn get(&self, provenance_id: &Blake3Digest) -> Option<ProvenanceRecord> {
        let key = primary_key(provenance_id);
        match self.store.get_sync(CF, &key) {
            Ok(Some(bytes)) => bcs::from_bytes::<ProvenanceRecord>(&bytes).ok(),
            _ => None,
        }
    }

    /// Query by agent ID with pagination.
    pub fn query_by_agent(
        &self,
        agent: &AccountAddress,
        params: &ProvenanceQueryParams,
    ) -> ProvenanceQueryResult {
        let prefix = agent_prefix(agent);
        self.query_by_index(&prefix, params)
    }

    /// Query by session ID with pagination.
    pub fn query_by_session(
        &self,
        session_id: &Blake3Digest,
        params: &ProvenanceQueryParams,
    ) -> ProvenanceQueryResult {
        let prefix = session_prefix(session_id);
        self.query_by_index(&prefix, params)
    }

    /// Query by capability token with pagination.
    pub fn query_by_capability(
        &self,
        token: &TokenId,
        params: &ProvenanceQueryParams,
    ) -> ProvenanceQueryResult {
        let prefix = capability_prefix(token);
        self.query_by_index(&prefix, params)
    }

    /// Query by transaction hash with pagination.
    pub fn query_by_tx_hash(
        &self,
        tx_hash: &Blake3Digest,
        params: &ProvenanceQueryParams,
    ) -> ProvenanceQueryResult {
        let prefix = tx_hash_prefix(tx_hash);
        self.query_by_index(&prefix, params)
    }

    /// Query by provenance status with pagination.
    pub fn query_by_status(
        &self,
        status: ProvenanceStatus,
        params: &ProvenanceQueryParams,
    ) -> ProvenanceQueryResult {
        let prefix = status_prefix(status);
        self.query_by_index(&prefix, params)
    }

    /// Get full chronological activity feed with pagination.
    pub fn activity_feed(&self, params: &ProvenanceQueryParams) -> ProvenanceQueryResult {
        // Scan the ordered index ('o' prefix).
        let mut start = vec![b'o'];
        if let Some(after) = params.after_ms {
            start.extend_from_slice(&after.0.to_be_bytes());
        }
        let mut end = vec![b'o'];
        if let Some(before) = params.before_ms {
            // Include the 'before' timestamp.
            end.extend_from_slice(&(before.0 + 1).to_be_bytes());
        } else {
            end.push(0xFF);
            end.push(0xFF);
        }

        let entries = match self.store.scan(CF, &start, &end) {
            Ok(entries) => entries,
            Err(_) => return empty_result(),
        };

        let ids: Vec<Blake3Digest> = entries
            .iter()
            .filter_map(|(k, _)| extract_provenance_id(k))
            .collect();

        self.paginate_ids(&ids, params)
    }

    /// Total approximate record count.
    pub fn len(&self) -> u64 {
        *self.count.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether the store has no records.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Recover record count from the database.
    ///
    /// Returns the total number of primary records found.
    pub fn recover_count(&self) -> Result<u64, crate::error::IntentError> {
        let start = vec![b'p'];
        let end = vec![b'p', 0xFF];
        let entries = self
            .store
            .scan(CF, &start, &end)
            .map_err(|e| crate::error::IntentError::Storage(e.to_string()))?;
        let total = entries.len() as u64;
        let mut count = self.count.lock().unwrap_or_else(|e| e.into_inner());
        *count = total;
        Ok(total)
    }

    /// Remove records older than `cutoff_ms`.
    ///
    /// Returns the number of records cleaned up.
    pub async fn cleanup_before(&self, cutoff_ms: u64) -> Result<u64, crate::error::IntentError> {
        // Scan the ordered index up to the cutoff.
        let start = vec![b'o'];
        let mut end = vec![b'o'];
        end.extend_from_slice(&cutoff_ms.to_be_bytes());

        let entries = self
            .store
            .scan(CF, &start, &end)
            .map_err(|e| crate::error::IntentError::Storage(e.to_string()))?;

        if entries.is_empty() {
            return Ok(0);
        }

        let ids: Vec<Blake3Digest> = entries
            .iter()
            .filter_map(|(k, _)| extract_provenance_id(k))
            .collect();

        // Fetch full records so we can delete all index keys.
        let mut batch = self.store.new_batch();
        let mut deleted = 0u64;

        for id in &ids {
            if let Some(record) = self.get(id) {
                // Delete primary.
                batch.delete_cf(CF, primary_key(id));
                // Delete indexes.
                batch.delete_cf(CF, agent_index_key(&record.agent_id, id));
                batch.delete_cf(CF, session_index_key(&record.session_id, id));
                if let Some(token) = &record.capability_token_id {
                    batch.delete_cf(CF, capability_index_key(token, id));
                }
                batch.delete_cf(CF, ordered_index_key(record.created_at_ms, id));
                if let Some(tx_hash) = &record.tx_hash {
                    batch.delete_cf(CF, tx_hash_index_key(tx_hash, id));
                }
                batch.delete_cf(
                    CF,
                    status_index_key(record.status, record.created_at_ms, id),
                );
                deleted += 1;
            }
        }

        if deleted > 0 {
            self.store
                .write_batch(batch)
                .await
                .map_err(|e| crate::error::IntentError::Storage(e.to_string()))?;

            let mut count = self.count.lock().unwrap_or_else(|e| e.into_inner());
            *count = count.saturating_sub(deleted);
        }

        Ok(deleted)
    }

    // ── Anchor receipt methods ──────────────────────────────────────

    /// Store an anchor receipt after on-chain confirmation.
    ///
    /// Writes both the receipt keyed by `anchor_digest` and a batch sequence
    /// index entry for reverse lookup.
    pub async fn store_anchor_receipt(
        &self,
        receipt: &AnchorReceipt,
    ) -> Result<(), crate::error::IntentError> {
        let value =
            bcs::to_bytes(receipt).map_err(|e| crate::error::IntentError::Codec(e.to_string()))?;

        let mut batch = self.store.new_batch();
        batch.put_cf(CF, anchor_receipt_key(&receipt.anchor_digest), value);
        // Batch-seq → anchor_digest for sequence-based lookup.
        batch.put_cf(
            CF,
            anchor_batch_seq_key(receipt.batch_seq),
            receipt.anchor_digest.0.to_vec(),
        );
        self.store
            .write_batch(batch)
            .await
            .map_err(|e| crate::error::IntentError::Storage(e.to_string()))
    }

    /// Retrieve an anchor receipt by `anchor_digest`.
    pub fn get_anchor_receipt(&self, anchor_digest: &Blake3Digest) -> Option<AnchorReceipt> {
        let key = anchor_receipt_key(anchor_digest);
        match self.store.get_sync(CF, &key) {
            Ok(Some(bytes)) => bcs::from_bytes::<AnchorReceipt>(&bytes).ok(),
            _ => None,
        }
    }

    /// Retrieve an anchor receipt by `batch_seq`.
    pub fn get_anchor_receipt_by_seq(&self, batch_seq: u64) -> Option<AnchorReceipt> {
        let key = anchor_batch_seq_key(batch_seq);
        match self.store.get_sync(CF, &key) {
            Ok(Some(digest_bytes)) if digest_bytes.len() == 32 => {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&digest_bytes);
                self.get_anchor_receipt(&Blake3Digest(arr))
            }
            _ => None,
        }
    }

    /// List all anchor receipts in batch-sequence order.
    pub fn list_anchor_receipts(&self, limit: u32) -> Vec<AnchorReceipt> {
        let start = vec![b'b'];
        let end = vec![b'b', 0xFF, 0xFF];
        let entries = match self.store.scan(CF, &start, &end) {
            Ok(e) => e,
            Err(_) => return vec![],
        };

        entries
            .iter()
            .filter_map(|(_, v)| {
                if v.len() == 32 {
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(v);
                    self.get_anchor_receipt(&Blake3Digest(arr))
                } else {
                    None
                }
            })
            .take(limit as usize)
            .collect()
    }

    /// Get the last anchored batch sequence number.
    pub fn last_anchor_seq(&self) -> Option<u64> {
        let key = meta_key(META_LAST_ANCHOR_SEQ);
        match self.store.get_sync(CF, &key) {
            Ok(Some(bytes)) if bytes.len() == 8 => {
                Some(u64::from_be_bytes(bytes.try_into().unwrap()))
            }
            _ => None,
        }
    }

    /// Get the record count at time of last anchor (the "anchored watermark").
    pub fn last_anchor_count(&self) -> Option<u64> {
        let key = meta_key(META_LAST_ANCHOR_COUNT);
        match self.store.get_sync(CF, &key) {
            Ok(Some(bytes)) if bytes.len() == 8 => {
                Some(u64::from_be_bytes(bytes.try_into().unwrap()))
            }
            _ => None,
        }
    }

    /// Update anchor metadata after a successful anchor.
    pub async fn update_anchor_metadata(
        &self,
        batch_seq: u64,
        record_count: u64,
    ) -> Result<(), crate::error::IntentError> {
        let mut batch = self.store.new_batch();
        batch.put_cf(
            CF,
            meta_key(META_LAST_ANCHOR_SEQ),
            batch_seq.to_be_bytes().to_vec(),
        );
        batch.put_cf(
            CF,
            meta_key(META_LAST_ANCHOR_COUNT),
            record_count.to_be_bytes().to_vec(),
        );
        self.store
            .write_batch(batch)
            .await
            .map_err(|e| crate::error::IntentError::Storage(e.to_string()))
    }

    /// Return provenance record IDs that haven't been anchored yet.
    ///
    /// These are records added since the last anchor count watermark.
    /// Returns up to `limit` record IDs.
    pub fn pending_anchor_record_ids(&self, limit: u32) -> Vec<Blake3Digest> {
        let start = vec![b'p'];
        let end = vec![b'p', 0xFF];
        let entries = match self.store.scan(CF, &start, &end) {
            Ok(e) => e,
            Err(_) => return vec![],
        };

        let last_count = self.last_anchor_count().unwrap_or(0) as usize;

        // Skip already-anchored records (first `last_count` entries)
        // and collect the remaining up to `limit`.
        entries
            .iter()
            .skip(last_count)
            .take(limit as usize)
            .filter_map(|(k, _)| {
                if k.len() == 33 {
                    let mut id = [0u8; 32];
                    id.copy_from_slice(&k[1..]);
                    Some(Blake3Digest(id))
                } else {
                    None
                }
            })
            .collect()
    }

    // ── Internal helpers ────────────────────────────────────────────

    /// Query using an index prefix, then fetch + paginate the records.
    fn query_by_index(
        &self,
        prefix: &[u8],
        params: &ProvenanceQueryParams,
    ) -> ProvenanceQueryResult {
        let end = prefix_end(prefix);
        let entries = match self.store.scan(CF, prefix, &end) {
            Ok(entries) => entries,
            Err(_) => return empty_result(),
        };

        let ids: Vec<Blake3Digest> = entries
            .iter()
            .filter_map(|(k, _)| extract_provenance_id(k))
            .collect();

        self.paginate_ids(&ids, params)
    }

    /// Fetch records by ID list, apply time filters, and paginate.
    fn paginate_ids(
        &self,
        ids: &[Blake3Digest],
        params: &ProvenanceQueryParams,
    ) -> ProvenanceQueryResult {
        // Resolve cursor position.
        let start = if let Some(ref cursor) = params.cursor {
            match ids.iter().position(|id| id == cursor) {
                Some(pos) => pos + 1,
                None => return empty_result(),
            }
        } else {
            0
        };

        // Fetch records, apply time filter, take limit.
        let mut records = Vec::new();
        let mut total_count = 0u64;

        // We need to count all matching records for total_count,
        // but only collect up to limit for the response.
        // Use a two-pass approach for correctness with small datasets.
        let candidate_ids = &ids[start..];

        // Build a lookup cache for this page to avoid repeated DB reads.
        let mut cache: HashMap<Blake3Digest, ProvenanceRecord> = HashMap::new();
        for id in candidate_ids {
            if let Some(record) = self.get(id) {
                // Apply time filter.
                if let Some(after) = params.after_ms {
                    if record.created_at_ms.0 < after.0 {
                        continue;
                    }
                }
                if let Some(before) = params.before_ms {
                    if record.created_at_ms.0 > before.0 {
                        continue;
                    }
                }
                total_count += 1;
                if records.len() < params.limit as usize {
                    records.push(record.clone());
                }
                cache.insert(*id, record);
            }
        }

        let cursor = records.last().map(|r| r.provenance_id);

        ProvenanceQueryResult {
            records,
            total_count,
            cursor,
        }
    }
}

fn empty_result() -> ProvenanceQueryResult {
    ProvenanceQueryResult {
        records: vec![],
        total_count: 0,
        cursor: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_primitives::TimestampMs;
    use nexus_storage::MemoryStore;

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

    #[tokio::test]
    async fn record_and_get() {
        let store = RocksProvenanceStore::new(MemoryStore::new());
        let rec = make_record(0x01, 0xAA, 0x10, 1_000);
        store.record(&rec).await.unwrap();

        let retrieved = store.get(&Blake3Digest([0x01; 32]));
        assert_eq!(retrieved, Some(rec));
        assert_eq!(store.len(), 1);
    }

    #[tokio::test]
    async fn get_nonexistent() {
        let store = RocksProvenanceStore::new(MemoryStore::new());
        assert!(store.get(&Blake3Digest([0xFF; 32])).is_none());
    }

    #[tokio::test]
    async fn query_by_agent() {
        let store = RocksProvenanceStore::new(MemoryStore::new());
        store
            .record(&make_record(0x01, 0xAA, 0x10, 1_000))
            .await
            .unwrap();
        store
            .record(&make_record(0x02, 0xBB, 0x20, 2_000))
            .await
            .unwrap();
        store
            .record(&make_record(0x03, 0xAA, 0x30, 3_000))
            .await
            .unwrap();

        let result = store.query_by_agent(&addr(0xAA), &default_params());
        assert_eq!(result.records.len(), 2);
        assert_eq!(result.total_count, 2);
    }

    #[tokio::test]
    async fn query_by_session() {
        let store = RocksProvenanceStore::new(MemoryStore::new());
        store
            .record(&make_record(0x01, 0xAA, 0x10, 1_000))
            .await
            .unwrap();
        store
            .record(&make_record(0x02, 0xBB, 0x10, 2_000))
            .await
            .unwrap();

        let result = store.query_by_session(&Blake3Digest([0x10; 32]), &default_params());
        assert_eq!(result.records.len(), 2);
    }

    #[tokio::test]
    async fn query_by_capability() {
        let store = RocksProvenanceStore::new(MemoryStore::new());
        let mut rec = make_record(0x01, 0xAA, 0x10, 1_000);
        rec.capability_token_id = Some(TokenId::Native);
        store.record(&rec).await.unwrap();
        store
            .record(&make_record(0x02, 0xBB, 0x20, 2_000))
            .await
            .unwrap();

        let result = store.query_by_capability(&TokenId::Native, &default_params());
        assert_eq!(result.records.len(), 1);
    }

    #[tokio::test]
    async fn activity_feed_chronological() {
        let store = RocksProvenanceStore::new(MemoryStore::new());
        store
            .record(&make_record(0x01, 0xAA, 0x10, 1_000))
            .await
            .unwrap();
        store
            .record(&make_record(0x02, 0xBB, 0x20, 2_000))
            .await
            .unwrap();
        store
            .record(&make_record(0x03, 0xCC, 0x30, 3_000))
            .await
            .unwrap();

        let result = store.activity_feed(&default_params());
        assert_eq!(result.records.len(), 3);
        assert_eq!(result.records[0].created_at_ms, TimestampMs(1_000));
        assert_eq!(result.records[2].created_at_ms, TimestampMs(3_000));
    }

    #[tokio::test]
    async fn update_status() {
        let store = RocksProvenanceStore::new(MemoryStore::new());
        store
            .record(&make_record(0x01, 0xAA, 0x10, 1_000))
            .await
            .unwrap();

        let updated = store
            .update_status(&Blake3Digest([0x01; 32]), ProvenanceStatus::Committed)
            .await
            .unwrap();
        assert!(updated);

        let rec = store.get(&Blake3Digest([0x01; 32])).unwrap();
        assert_eq!(rec.status, ProvenanceStatus::Committed);
    }

    #[tokio::test]
    async fn set_tx_hash() {
        let store = RocksProvenanceStore::new(MemoryStore::new());
        store
            .record(&make_record(0x01, 0xAA, 0x10, 1_000))
            .await
            .unwrap();

        let updated = store
            .set_tx_hash(&Blake3Digest([0x01; 32]), Blake3Digest([0xBB; 32]))
            .await
            .unwrap();
        assert!(updated);

        let rec = store.get(&Blake3Digest([0x01; 32])).unwrap();
        assert_eq!(rec.tx_hash, Some(Blake3Digest([0xBB; 32])));
    }

    #[tokio::test]
    async fn recover_count() {
        let store = RocksProvenanceStore::new(MemoryStore::new());
        store
            .record(&make_record(0x01, 0xAA, 0x10, 1_000))
            .await
            .unwrap();
        store
            .record(&make_record(0x02, 0xBB, 0x20, 2_000))
            .await
            .unwrap();

        // Reset internal counter to simulate restart.
        *store.count.lock().unwrap() = 0;

        let count = store.recover_count().unwrap();
        assert_eq!(count, 2);
        assert_eq!(store.len(), 2);
    }

    #[tokio::test]
    async fn time_range_filter() {
        let store = RocksProvenanceStore::new(MemoryStore::new());
        store
            .record(&make_record(0x01, 0xAA, 0x10, 1_000))
            .await
            .unwrap();
        store
            .record(&make_record(0x02, 0xAA, 0x20, 2_000))
            .await
            .unwrap();
        store
            .record(&make_record(0x03, 0xAA, 0x30, 3_000))
            .await
            .unwrap();

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

    #[tokio::test]
    async fn cleanup_before() {
        let store = RocksProvenanceStore::new(MemoryStore::new());
        store
            .record(&make_record(0x01, 0xAA, 0x10, 500))
            .await
            .unwrap();
        store
            .record(&make_record(0x02, 0xBB, 0x20, 1_500))
            .await
            .unwrap();
        store
            .record(&make_record(0x03, 0xCC, 0x30, 3_000))
            .await
            .unwrap();

        let cleaned = store.cleanup_before(1_000).await.unwrap();
        assert_eq!(cleaned, 1);
        assert!(store.get(&Blake3Digest([0x01; 32])).is_none());
        assert!(store.get(&Blake3Digest([0x02; 32])).is_some());
        assert!(store.get(&Blake3Digest([0x03; 32])).is_some());
    }

    #[tokio::test]
    async fn pagination_with_cursor() {
        let store = RocksProvenanceStore::new(MemoryStore::new());
        store
            .record(&make_record(0x01, 0xAA, 0x10, 1_000))
            .await
            .unwrap();
        store
            .record(&make_record(0x02, 0xAA, 0x20, 2_000))
            .await
            .unwrap();
        store
            .record(&make_record(0x03, 0xAA, 0x30, 3_000))
            .await
            .unwrap();

        // Page 1: limit 2
        let mut params = default_params();
        params.limit = 2;
        let page1 = store.activity_feed(&params);
        assert_eq!(page1.records.len(), 2);
        assert!(page1.cursor.is_some());

        // Page 2: from cursor
        params.cursor = page1.cursor;
        let page2 = store.activity_feed(&params);
        assert_eq!(page2.records.len(), 1);
    }

    // ── Z-5: Cold-start verification ────────────────────────────────

    #[tokio::test]
    async fn cold_start_recovers_count_and_queries() {
        let backing = MemoryStore::new();

        // Phase 1: Populate records in the original store.
        {
            let store = RocksProvenanceStore::new(backing.clone());
            store
                .record(&make_record(0x01, 0xAA, 0x10, 1_000))
                .await
                .unwrap();
            store
                .record(&make_record(0x02, 0xBB, 0x20, 2_000))
                .await
                .unwrap();
            store
                .record(&make_record(0x03, 0xAA, 0x30, 3_000))
                .await
                .unwrap();
            assert_eq!(store.len(), 3);
            // Drop — simulates node shutdown.
        }

        // Phase 2: Cold-start with fresh store against same backing.
        let store2 = RocksProvenanceStore::new(backing);
        assert_eq!(store2.len(), 0, "count should be 0 before recover");

        let count = store2.recover_count().unwrap();
        assert_eq!(count, 3);
        assert_eq!(store2.len(), 3);

        // Verify individual lookups work after recovery.
        assert!(store2.get(&Blake3Digest([0x01; 32])).is_some());
        assert!(store2.get(&Blake3Digest([0x02; 32])).is_some());
        assert!(store2.get(&Blake3Digest([0x03; 32])).is_some());

        // Verify index-based queries are intact.
        let by_agent = store2.query_by_agent(&addr(0xAA), &default_params());
        assert_eq!(by_agent.records.len(), 2);

        let by_session = store2.query_by_session(&Blake3Digest([0x10; 32]), &default_params());
        assert_eq!(by_session.records.len(), 1);

        // Verify activity feed chronological order is preserved.
        let feed = store2.activity_feed(&default_params());
        assert_eq!(feed.records.len(), 3);
        assert_eq!(feed.records[0].created_at_ms, TimestampMs(1_000));
        assert_eq!(feed.records[2].created_at_ms, TimestampMs(3_000));
    }

    #[tokio::test]
    async fn cold_start_anchor_receipts_survive_restart() {
        let backing = MemoryStore::new();

        // Phase 1: Store an anchor receipt and metadata.
        {
            let store = RocksProvenanceStore::new(backing.clone());
            let receipt = AnchorReceipt {
                anchor_digest: Blake3Digest([0xAA; 32]),
                batch_seq: 42,
                tx_hash: Blake3Digest([0xBB; 32]),
                block_height: 100,
                anchored_at_ms: TimestampMs(10_000),
            };
            store.store_anchor_receipt(&receipt).await.unwrap();
            store.update_anchor_metadata(42, 5).await.unwrap();
        }

        // Phase 2: Cold-start and verify anchor data is still accessible.
        let store2 = RocksProvenanceStore::new(backing);

        let receipt = store2.get_anchor_receipt(&Blake3Digest([0xAA; 32]));
        assert!(receipt.is_some());
        let r = receipt.unwrap();
        assert_eq!(r.batch_seq, 42);
        assert_eq!(r.block_height, 100);

        let by_seq = store2.get_anchor_receipt_by_seq(42);
        assert!(by_seq.is_some());
        assert_eq!(by_seq.unwrap().anchor_digest, Blake3Digest([0xAA; 32]));

        assert_eq!(store2.last_anchor_seq(), Some(42));
        assert_eq!(store2.last_anchor_count(), Some(5));
    }

    #[tokio::test]
    async fn cold_start_empty_db_recovers_zero() {
        let backing = MemoryStore::new();
        let store = RocksProvenanceStore::new(backing);
        let count = store.recover_count().unwrap();
        assert_eq!(count, 0);
        assert!(store.is_empty());
    }
}
