// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! [`MemoryStore`] — in-memory implementation of [`StateStorage`] for testing.
//!
//! Backed by `BTreeMap` for deterministic iteration order.
//! Snapshot produces a deep clone with correct point-in-time semantics.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use crate::error::StorageError;
use crate::traits::{StateStorage, WriteBatchOps};

// ── MemoryWriteBatch ─────────────────────────────────────────────────────────

/// Write batch for [`MemoryStore`].
pub struct MemoryWriteBatch {
    ops: Vec<(String, Vec<u8>, Option<Vec<u8>>)>,
}

impl MemoryWriteBatch {
    fn new() -> Self {
        Self { ops: Vec::new() }
    }
}

impl WriteBatchOps for MemoryWriteBatch {
    fn put(&mut self, key: Vec<u8>, value: Vec<u8>) -> &mut Self {
        self.ops.push(("cf_state".to_owned(), key, Some(value)));
        self
    }

    fn delete(&mut self, key: Vec<u8>) -> &mut Self {
        self.ops.push(("cf_state".to_owned(), key, None));
        self
    }

    fn put_cf(&mut self, cf: &str, key: Vec<u8>, value: Vec<u8>) -> &mut Self {
        self.ops.push((cf.to_owned(), key, Some(value)));
        self
    }

    fn delete_cf(&mut self, cf: &str, key: Vec<u8>) -> &mut Self {
        self.ops.push((cf.to_owned(), key, None));
        self
    }

    fn size_hint(&self) -> usize {
        self.ops.len()
    }
}

// ── MemoryStore ──────────────────────────────────────────────────────────────

/// In-memory storage backend for unit and integration tests.
///
/// Uses a `BTreeMap<(cf, key), value>` under an `RwLock` for thread-safe
/// concurrent access. Iteration order is lexicographic, matching RocksDB.
///
/// [`snapshot()`](StateStorage::snapshot) produces a deep clone with correct
/// point-in-time isolation — subsequent writes to the original store are
/// invisible through the snapshot.
#[derive(Clone)]
pub struct MemoryStore {
    #[allow(clippy::type_complexity)]
    data: Arc<RwLock<BTreeMap<(String, Vec<u8>), Vec<u8>>>>,
}

impl MemoryStore {
    /// Create an empty in-memory store.
    pub fn new() -> Self {
        Self {
            data: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl StateStorage for MemoryStore {
    type WriteBatch = MemoryWriteBatch;

    async fn get(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
        let data = self
            .data
            .read()
            .map_err(|e| StorageError::Snapshot(format!("lock poisoned: {e}")))?;
        Ok(data.get(&(cf.to_owned(), key.to_vec())).cloned())
    }

    fn scan(
        &self,
        cf: &str,
        start: &[u8],
        end: &[u8],
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StorageError> {
        let data = self
            .data
            .read()
            .map_err(|e| StorageError::Snapshot(format!("lock poisoned: {e}")))?;

        let cf_name = cf.to_owned();
        let range_start = (cf_name.clone(), start.to_vec());
        let range_end = (cf_name, end.to_vec());

        let results = data
            .range(range_start..range_end)
            .map(|((_, k), v)| (k.clone(), v.clone()))
            .collect();

        Ok(results)
    }

    async fn write_batch(&self, batch: MemoryWriteBatch) -> Result<(), StorageError> {
        let mut data = self
            .data
            .write()
            .map_err(|e| StorageError::Snapshot(format!("lock poisoned: {e}")))?;
        for (cf, key, value) in batch.ops {
            match value {
                Some(v) => {
                    data.insert((cf, key), v);
                }
                None => {
                    data.remove(&(cf, key));
                }
            }
        }
        Ok(())
    }

    fn new_batch(&self) -> MemoryWriteBatch {
        MemoryWriteBatch::new()
    }

    fn snapshot(&self) -> impl StateStorage<WriteBatch = Self::WriteBatch> {
        // Deep clone for true point-in-time isolation.
        let data = self.data.read().expect("lock not poisoned in snapshot");
        MemoryStore {
            data: Arc::new(RwLock::new(data.clone())),
        }
    }

    fn get_sync(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
        let data = self
            .data
            .read()
            .map_err(|e| StorageError::Snapshot(format!("lock poisoned: {e}")))?;
        Ok(data.get(&(cf.to_owned(), key.to_vec())).cloned())
    }

    fn put_sync(&self, cf: &str, key: Vec<u8>, value: Vec<u8>) -> Result<(), StorageError> {
        let mut data = self
            .data
            .write()
            .map_err(|e| StorageError::Snapshot(format!("lock poisoned: {e}")))?;
        data.insert((cf.to_owned(), key), value);
        Ok(())
    }
}

impl std::fmt::Debug for MemoryStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self.data.read().map(|d| d.len()).unwrap_or(0);
        f.debug_struct("MemoryStore")
            .field("entries", &count)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::WriteBatchOps;
    use crate::ColumnFamily;

    const CF: &str = "cf_state";

    #[tokio::test]
    async fn get_nonexistent_returns_none() {
        let store = MemoryStore::new();
        assert!(store.get(CF, b"missing").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn put_and_get() {
        let store = MemoryStore::new();
        let mut batch = store.new_batch();
        batch.put(b"key1".to_vec(), b"val1".to_vec());
        store.write_batch(batch).await.unwrap();

        let val = store.get(CF, b"key1").await.unwrap();
        assert_eq!(val, Some(b"val1".to_vec()));
    }

    #[tokio::test]
    async fn delete_removes_key() {
        let store = MemoryStore::new();
        let mut batch = store.new_batch();
        batch.put(b"key1".to_vec(), b"val1".to_vec());
        store.write_batch(batch).await.unwrap();

        let mut batch = store.new_batch();
        batch.delete(b"key1".to_vec());
        store.write_batch(batch).await.unwrap();

        assert!(store.get(CF, b"key1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn scan_range() {
        let store = MemoryStore::new();
        let mut batch = store.new_batch();
        batch.put(b"a".to_vec(), b"1".to_vec());
        batch.put(b"b".to_vec(), b"2".to_vec());
        batch.put(b"c".to_vec(), b"3".to_vec());
        batch.put(b"d".to_vec(), b"4".to_vec());
        store.write_batch(batch).await.unwrap();

        let results = store.scan(CF, b"b", b"d").unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0], (b"b".to_vec(), b"2".to_vec()));
        assert_eq!(results[1], (b"c".to_vec(), b"3".to_vec()));
    }

    #[tokio::test]
    async fn snapshot_isolation() {
        let store = MemoryStore::new();
        let mut batch = store.new_batch();
        batch.put(b"key1".to_vec(), b"before".to_vec());
        store.write_batch(batch).await.unwrap();

        // Take snapshot.
        let snap = store.snapshot();

        // Write after snapshot.
        let mut batch = store.new_batch();
        batch.put(b"key1".to_vec(), b"after".to_vec());
        batch.put(b"key2".to_vec(), b"new".to_vec());
        store.write_batch(batch).await.unwrap();

        // Snapshot sees old value.
        assert_eq!(
            snap.get(CF, b"key1").await.unwrap(),
            Some(b"before".to_vec())
        );
        // Snapshot does not see new key.
        assert!(snap.get(CF, b"key2").await.unwrap().is_none());

        // Original sees new values.
        assert_eq!(
            store.get(CF, b"key1").await.unwrap(),
            Some(b"after".to_vec())
        );
    }

    #[tokio::test]
    async fn batch_size_hint() {
        let store = MemoryStore::new();
        let mut batch = store.new_batch();
        assert_eq!(batch.size_hint(), 0);
        batch.put(b"k1".to_vec(), b"v1".to_vec());
        batch.put(b"k2".to_vec(), b"v2".to_vec());
        batch.delete(b"k3".to_vec());
        assert_eq!(batch.size_hint(), 3);
    }

    #[tokio::test]
    async fn put_cf_explicit() {
        let store = MemoryStore::new();
        let mut batch = store.new_batch();
        batch.put_cf("cf_blocks", b"blk1".to_vec(), b"data1".to_vec());
        store.write_batch(batch).await.unwrap();

        let val = store.get("cf_blocks", b"blk1").await.unwrap();
        assert_eq!(val, Some(b"data1".to_vec()));

        // Not visible under default CF.
        assert!(store.get(CF, b"blk1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn multiple_cfs_isolation() {
        let store = MemoryStore::new();
        let mut batch = store.new_batch();
        batch.put_cf("cf_blocks", b"key".to_vec(), b"blocks_val".to_vec());
        batch.put_cf("cf_transactions", b"key".to_vec(), b"tx_val".to_vec());
        store.write_batch(batch).await.unwrap();

        assert_eq!(
            store.get("cf_blocks", b"key").await.unwrap(),
            Some(b"blocks_val".to_vec())
        );
        assert_eq!(
            store.get("cf_transactions", b"key").await.unwrap(),
            Some(b"tx_val".to_vec())
        );
    }

    #[tokio::test]
    async fn overwrite_value() {
        let store = MemoryStore::new();
        let mut batch = store.new_batch();
        batch.put(b"key".to_vec(), b"v1".to_vec());
        store.write_batch(batch).await.unwrap();

        let mut batch = store.new_batch();
        batch.put(b"key".to_vec(), b"v2".to_vec());
        store.write_batch(batch).await.unwrap();

        assert_eq!(store.get(CF, b"key").await.unwrap(), Some(b"v2".to_vec()));
    }

    #[tokio::test]
    async fn empty_scan_returns_empty() {
        let store = MemoryStore::new();
        let results = store.scan(CF, b"a", b"z").unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn block_tx_index_scan_orders_by_commit_sequence() {
        let store = MemoryStore::new();
        let cf = ColumnFamily::BlockTxIndex.as_str();

        let mut batch = store.new_batch();
        batch.put_cf(cf, 10u64.to_be_bytes().to_vec(), b"tx10".to_vec());
        batch.put_cf(cf, 2u64.to_be_bytes().to_vec(), b"tx2".to_vec());
        batch.put_cf(cf, 1u64.to_be_bytes().to_vec(), b"tx1".to_vec());
        store.write_batch(batch).await.unwrap();

        // Scan [0, 11) so keys 1,2,10 are all included.
        let start = 0u64.to_be_bytes();
        let end = 11u64.to_be_bytes();
        let rows = store.scan(cf, &start, &end).unwrap();

        assert_eq!(rows.len(), 3);
        let seqs: Vec<u64> = rows
            .iter()
            .map(|(k, _)| u64::from_be_bytes(k.as_slice().try_into().unwrap()))
            .collect();
        assert_eq!(seqs, vec![1, 2, 10]);
    }

    #[tokio::test]
    async fn block_tx_index_cf_isolated_from_state_cf() {
        let store = MemoryStore::new();
        let mut batch = store.new_batch();

        let seq_key = 1u64.to_be_bytes().to_vec();
        batch.put_cf(
            ColumnFamily::BlockTxIndex.as_str(),
            seq_key.clone(),
            b"tx-index".to_vec(),
        );
        batch.put_cf(
            ColumnFamily::State.as_str(),
            seq_key.clone(),
            b"state".to_vec(),
        );
        store.write_batch(batch).await.unwrap();

        let block_idx = store
            .get(ColumnFamily::BlockTxIndex.as_str(), &seq_key)
            .await
            .unwrap();
        let state_val = store
            .get(ColumnFamily::State.as_str(), &seq_key)
            .await
            .unwrap();
        assert_eq!(block_idx, Some(b"tx-index".to_vec()));
        assert_eq!(state_val, Some(b"state".to_vec()));
    }
}
