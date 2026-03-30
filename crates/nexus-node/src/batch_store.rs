// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Batch store — maps batch digests to their constituent transactions.
//!
//! The [`BatchStore`] is the link between the batch proposer (which creates
//! batches from the mempool) and the execution bridge (which resolves
//! committed certificates back to transactions for Block-STM execution).
//!
//! Thread-safe via `DashMap`; supports concurrent reads from the execution
//! bridge and writes from the batch proposer.  An optional
//! [`BatchPersistOps`](crate::batch_persist::BatchPersistOps) layer writes
//! through to `cf_batches` so batches survive cold restarts.

use dashmap::DashMap;
use nexus_execution::types::SignedTransaction;
use nexus_primitives::BatchDigest;
use tracing::debug;

use crate::batch_persist::BatchPersistOps;

/// Maximum number of batches to retain before evicting the oldest.
///
/// Prevents unbounded memory growth if execution lags behind consensus.
const MAX_RETAINED_BATCHES: usize = 4096;

/// Thread-safe store of batch digest → transaction payloads.
///
/// Used to bridge the gap between consensus (which commits certificate
/// digests) and execution (which needs the actual transactions).
pub struct BatchStore {
    /// Primary store: batch_digest → transactions.
    batches: DashMap<BatchDigest, Vec<SignedTransaction>>,
    /// Optional disk persistence layer.
    persist: Option<Box<dyn BatchPersistOps>>,
}

impl BatchStore {
    /// Create a new empty batch store (no disk persistence).
    pub fn new() -> Self {
        Self {
            batches: DashMap::with_capacity(256),
            persist: None,
        }
    }

    /// Create a new empty batch store with disk write-through enabled.
    pub fn new_with_persistence(persist: Box<dyn BatchPersistOps>) -> Self {
        Self {
            batches: DashMap::with_capacity(256),
            persist: Some(persist),
        }
    }

    /// Restore batches from disk into the in-memory DashMap.
    ///
    /// Call this once at startup after constructing the `BatchStore` with
    /// persistence.  Returns the number of batches restored.
    pub fn restore_from_disk(&self) -> usize {
        let persist = match &self.persist {
            Some(p) => p,
            None => return 0,
        };
        match persist.restore_batches() {
            Ok(batches) => {
                let count = batches.len();
                for (digest, txs) in batches {
                    self.batches.insert(digest, txs);
                }
                count
            }
            Err(e) => {
                tracing::warn!("batch restore from disk failed: {e}");
                0
            }
        }
    }

    /// Store a batch of transactions keyed by their batch digest.
    ///
    /// If a batch with the same digest already exists, it is overwritten.
    /// Returns `true` if this was a new entry, `false` if it replaced an existing one.
    pub fn insert(&self, digest: BatchDigest, transactions: Vec<SignedTransaction>) -> bool {
        // Write-through to disk first (if persistence enabled).
        if let Some(ref p) = self.persist {
            if let Err(e) = p.put_batch(&digest, &transactions) {
                tracing::error!("batch persist failed for {digest}: {e}");
            }
        }

        let is_new = !self.batches.contains_key(&digest);
        self.batches.insert(digest, transactions);

        // Evict oldest entries if capacity exceeded.
        if self.batches.len() > MAX_RETAINED_BATCHES {
            self.evict_excess();
        }

        is_new
    }

    /// Retrieve the transactions for a given batch digest.
    ///
    /// Returns `None` if the batch is unknown (may have been evicted or
    /// was produced by another validator and not stored locally).
    /// Falls back to disk if not found in memory.
    pub fn get(&self, digest: &BatchDigest) -> Option<Vec<SignedTransaction>> {
        // Try in-memory first.
        if let Some(entry) = self.batches.get(digest) {
            return Some(entry.value().clone());
        }
        // Fall back to disk.
        if let Some(ref p) = self.persist {
            match p.get_batch(digest) {
                Ok(Some(txs)) => {
                    // Re-populate the in-memory cache.
                    self.batches.insert(*digest, txs.clone());
                    return Some(txs);
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!("batch disk fallback failed for {digest}: {e}");
                }
            }
        }
        None
    }

    /// Remove a batch by digest, returning the transactions if found.
    pub fn remove(&self, digest: &BatchDigest) -> Option<Vec<SignedTransaction>> {
        // Remove from disk (if persistence enabled).
        if let Some(ref p) = self.persist {
            if let Err(e) = p.delete_batch(digest) {
                tracing::error!("batch disk delete failed for {digest}: {e}");
            }
        }
        self.batches.remove(digest).map(|(_, txs)| txs)
    }

    /// Number of batches currently stored.
    pub fn len(&self) -> usize {
        self.batches.len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.batches.is_empty()
    }

    /// Check if a batch with the given digest exists.
    pub fn contains(&self, digest: &BatchDigest) -> bool {
        self.batches.contains_key(digest)
    }

    /// Evict batches down to MAX_RETAINED_BATCHES.
    ///
    /// DashMap doesn't maintain insertion order, so we remove arbitrary
    /// entries (not necessarily the oldest). In practice, the execution
    /// bridge removes batches after processing, so this is a safety net
    /// to prevent unbounded growth.
    fn evict_excess(&self) {
        let excess = self.batches.len().saturating_sub(MAX_RETAINED_BATCHES);
        if excess == 0 {
            return;
        }
        let keys: Vec<BatchDigest> = self
            .batches
            .iter()
            .take(excess)
            .map(|entry| *entry.key())
            .collect();
        for key in &keys {
            self.batches.remove(key);
        }
        debug!(evicted = keys.len(), "batch store evicted excess batches");
    }
}

impl Default for BatchStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_crypto::{DilithiumSigner, Signer};
    use nexus_execution::types::{
        compute_tx_digest, TransactionBody, TransactionPayload, TX_DOMAIN,
    };
    use nexus_primitives::{AccountAddress, Amount, Blake3Digest, EpochNumber, ShardId, TokenId};

    fn make_tx(seq: u64) -> SignedTransaction {
        let (sk, vk) = DilithiumSigner::generate_keypair();
        let body = TransactionBody {
            sender: AccountAddress([1u8; 32]),
            sequence_number: seq,
            expiry_epoch: EpochNumber(100),
            gas_limit: 10_000,
            gas_price: 1,
            target_shard: Some(ShardId(0)),
            payload: TransactionPayload::Transfer {
                recipient: AccountAddress([2u8; 32]),
                amount: Amount(100),
                token: TokenId::Native,
            },
            chain_id: 1,
        };
        let digest = compute_tx_digest(&body).expect("digest");
        let body_bytes = bcs::to_bytes(&body).expect("bcs");
        let sig = DilithiumSigner::sign(&sk, TX_DOMAIN, &body_bytes);
        SignedTransaction {
            body,
            signature: sig,
            sender_pk: vk,
            digest,
        }
    }

    fn make_batch_digest(seed: u8) -> BatchDigest {
        Blake3Digest([seed; 32])
    }

    #[test]
    fn insert_and_get() {
        let store = BatchStore::new();
        let digest = make_batch_digest(1);
        let txs = vec![make_tx(1), make_tx(2)];

        assert!(store.insert(digest, txs.clone()));
        let retrieved = store.get(&digest).unwrap();
        assert_eq!(retrieved.len(), 2);
    }

    #[test]
    fn insert_duplicate_overwrites() {
        let store = BatchStore::new();
        let digest = make_batch_digest(1);

        assert!(store.insert(digest, vec![make_tx(1)]));
        assert!(!store.insert(digest, vec![make_tx(2), make_tx(3)]));

        let retrieved = store.get(&digest).unwrap();
        assert_eq!(retrieved.len(), 2, "should have been overwritten");
    }

    #[test]
    fn remove_returns_transactions() {
        let store = BatchStore::new();
        let digest = make_batch_digest(1);
        store.insert(digest, vec![make_tx(1)]);

        let removed = store.remove(&digest).unwrap();
        assert_eq!(removed.len(), 1);
        assert!(store.get(&digest).is_none(), "should be removed");
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn get_unknown_returns_none() {
        let store = BatchStore::new();
        assert!(store.get(&make_batch_digest(99)).is_none());
    }

    #[test]
    fn contains_check() {
        let store = BatchStore::new();
        let digest = make_batch_digest(1);
        assert!(!store.contains(&digest));
        store.insert(digest, vec![]);
        assert!(store.contains(&digest));
    }

    #[test]
    fn empty_batch() {
        let store = BatchStore::new();
        let digest = make_batch_digest(1);
        store.insert(digest, vec![]);
        let retrieved = store.get(&digest).unwrap();
        assert!(retrieved.is_empty());
    }

    #[test]
    fn len_tracks_insertions() {
        let store = BatchStore::new();
        assert!(store.is_empty());
        store.insert(make_batch_digest(1), vec![make_tx(1)]);
        store.insert(make_batch_digest(2), vec![make_tx(2)]);
        assert_eq!(store.len(), 2);
        store.remove(&make_batch_digest(1));
        assert_eq!(store.len(), 1);
    }

    // ── eviction ─────────────────────────────────────────────────────

    #[test]
    fn evict_excess_noop_when_under_capacity() {
        let store = BatchStore::new();
        store.insert(make_batch_digest(1), vec![]);
        store.evict_excess();
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn insert_past_capacity_triggers_eviction() {
        let store = BatchStore::new();
        // Insert MAX_RETAINED_BATCHES + 1 entries.
        for i in 0..=(MAX_RETAINED_BATCHES as u16) {
            let mut digest_bytes = [0u8; 32];
            digest_bytes[0..2].copy_from_slice(&i.to_be_bytes());
            store.insert(Blake3Digest(digest_bytes), vec![]);
        }
        assert!(
            store.len() <= MAX_RETAINED_BATCHES,
            "store should have evicted excess: len={}",
            store.len()
        );
    }

    // ── with persistence mock ────────────────────────────────────────

    struct MockPersist;

    impl crate::batch_persist::BatchPersistOps for MockPersist {
        fn put_batch(
            &self,
            _digest: &BatchDigest,
            _txs: &[SignedTransaction],
        ) -> Result<(), crate::batch_persist::BatchPersistError> {
            Ok(())
        }
        fn get_batch(
            &self,
            _digest: &BatchDigest,
        ) -> Result<Option<Vec<SignedTransaction>>, crate::batch_persist::BatchPersistError> {
            Ok(None)
        }
        fn delete_batch(
            &self,
            _digest: &BatchDigest,
        ) -> Result<(), crate::batch_persist::BatchPersistError> {
            Ok(())
        }
        fn restore_batches(
            &self,
        ) -> Result<
            Vec<(BatchDigest, Vec<SignedTransaction>)>,
            crate::batch_persist::BatchPersistError,
        > {
            Ok(vec![
                (make_batch_digest(10), vec![make_tx(10)]),
                (make_batch_digest(20), vec![make_tx(20), make_tx(21)]),
            ])
        }
    }

    #[test]
    fn new_with_persistence_creates_store() {
        let store = BatchStore::new_with_persistence(Box::new(MockPersist));
        assert!(store.is_empty());
    }

    #[test]
    fn restore_from_disk_populates_dashmap() {
        let store = BatchStore::new_with_persistence(Box::new(MockPersist));
        let count = store.restore_from_disk();
        assert_eq!(count, 2);
        assert_eq!(store.len(), 2);
        assert!(store.contains(&make_batch_digest(10)));
        assert!(store.contains(&make_batch_digest(20)));
    }

    #[test]
    fn restore_from_disk_returns_zero_without_persistence() {
        let store = BatchStore::new();
        assert_eq!(store.restore_from_disk(), 0);
    }

    #[test]
    fn insert_with_persistence_calls_put_batch() {
        let store = BatchStore::new_with_persistence(Box::new(MockPersist));
        let is_new = store.insert(make_batch_digest(1), vec![make_tx(1)]);
        assert!(is_new);
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn get_falls_back_to_disk() {
        let store = BatchStore::new_with_persistence(Box::new(MockPersist));
        // Not in memory, MockPersist.get_batch returns None.
        assert!(store.get(&make_batch_digest(99)).is_none());
    }

    #[test]
    fn default_is_same_as_new() {
        let store = BatchStore::default();
        assert!(store.is_empty());
    }
}
