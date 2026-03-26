// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! In-memory transaction pool for pending transactions.
//!
//! The [`Mempool`] stores validated [`SignedTransaction`]s awaiting inclusion
//! in the next block proposal. It provides:
//!
//! - **Shard-partitioned storage**: transactions are bucketed by `target_shard`.
//! - **Deduplication**: transactions are indexed by their BLAKE3 digest.
//! - **Capacity limit**: oldest-insertion-order eviction when full.
//! - **TTL expiry**: transactions past their `expiry_epoch` are pruned.
//! - **Thread safety**: interior `Mutex` for shared access from gossip + RPC.
//! - **Shard validation**: transactions targeting a shard `>= num_shards` are rejected.

use std::collections::{HashMap, VecDeque};

use parking_lot::Mutex;

use nexus_execution::types::SignedTransaction;
use nexus_primitives::{EpochNumber, ShardId, TxDigest};

/// Configuration for the transaction mempool.
#[derive(Debug, Clone)]
pub struct MempoolConfig {
    /// Maximum number of transactions held in the pool (across all shards).
    pub capacity: usize,
    /// Number of active shards. Transactions targeting a shard `>= num_shards`
    /// are rejected at insertion time.
    pub num_shards: u16,
}

impl Default for MempoolConfig {
    fn default() -> Self {
        Self {
            capacity: 10_000,
            num_shards: 1,
        }
    }
}

/// Result of attempting to insert a transaction.
#[derive(Debug, PartialEq, Eq)]
pub enum InsertResult {
    /// Transaction accepted into the pool.
    Accepted,
    /// Transaction already exists (duplicate digest).
    Duplicate,
    /// Pool is at capacity; transaction was rejected.
    PoolFull,
    /// The transaction's `target_shard` is not a valid shard index.
    InvalidShard,
}

/// Thread-safe in-memory transaction pool.
///
/// Transactions are partitioned by [`ShardId`]. When the pool reaches
/// capacity after an `evict_expired` pass, new transactions are rejected.
pub struct Mempool {
    inner: Mutex<MempoolInner>,
    capacity: usize,
    num_shards: u16,
}

/// Internal (non-Sync) mempool state behind the Mutex.
struct MempoolInner {
    /// Per-shard insertion-ordered queues of (digest, transaction).
    shard_queues: HashMap<ShardId, VecDeque<(TxDigest, SignedTransaction)>>,
    /// Global digest dedup set.
    digests: std::collections::HashSet<TxDigest>,
    /// Total transaction count across all shards (maintained for O(1) len).
    total_count: usize,
}

impl Mempool {
    /// Create a new mempool with the given configuration.
    pub fn new(config: &MempoolConfig) -> Self {
        let num_shards = config.num_shards.max(1);
        let mut shard_queues = HashMap::with_capacity(num_shards as usize);
        for i in 0..num_shards {
            shard_queues.insert(
                ShardId(i),
                VecDeque::with_capacity((config.capacity / num_shards as usize).min(1024)),
            );
        }
        Self {
            inner: Mutex::new(MempoolInner {
                shard_queues,
                digests: std::collections::HashSet::with_capacity(config.capacity.min(1024)),
                total_count: 0,
            }),
            capacity: config.capacity,
            num_shards,
        }
    }

    /// The number of shards this mempool is configured for.
    pub fn num_shards(&self) -> u16 {
        self.num_shards
    }

    /// Resolve the effective shard for a transaction.
    /// Returns `ShardId(0)` when `target_shard` is `None`.
    fn effective_shard(target: Option<ShardId>) -> ShardId {
        target.unwrap_or(ShardId(0))
    }

    /// Try to insert a transaction into the pool.
    ///
    /// Returns [`InsertResult::Duplicate`] if the digest already exists,
    /// [`InsertResult::InvalidShard`] if `target_shard >= num_shards`,
    /// [`InsertResult::PoolFull`] if at capacity, or [`InsertResult::Accepted`].
    pub fn insert(&self, tx: SignedTransaction) -> InsertResult {
        let shard = Self::effective_shard(tx.body.target_shard);
        if shard.0 >= self.num_shards {
            return InsertResult::InvalidShard;
        }

        let mut inner = self.inner.lock();
        if inner.digests.contains(&tx.digest) {
            return InsertResult::Duplicate;
        }
        if inner.total_count >= self.capacity {
            return InsertResult::PoolFull;
        }
        inner.digests.insert(tx.digest);
        inner
            .shard_queues
            .entry(shard)
            .or_default()
            .push_back((tx.digest, tx));
        inner.total_count += 1;
        InsertResult::Accepted
    }

    /// Remove and return a specific transaction by digest.
    pub fn remove(&self, digest: &TxDigest) -> Option<SignedTransaction> {
        let mut inner = self.inner.lock();
        if !inner.digests.remove(digest) {
            return None;
        }
        for queue in inner.shard_queues.values_mut() {
            if let Some(pos) = queue.iter().position(|(d, _)| d == digest) {
                if let Some((_, tx)) = queue.remove(pos) {
                    inner.total_count -= 1;
                    return Some(tx);
                }
            }
        }
        None
    }

    /// Check if a transaction with the given digest exists.
    pub fn contains(&self, digest: &TxDigest) -> bool {
        let inner = self.inner.lock();
        inner.digests.contains(digest)
    }

    /// Current number of transactions in the pool (all shards combined).
    pub fn len(&self) -> usize {
        let inner = self.inner.lock();
        inner.total_count
    }

    /// Number of transactions pending for a specific shard.
    pub fn len_for_shard(&self, shard: ShardId) -> usize {
        let inner = self.inner.lock();
        inner.shard_queues.get(&shard).map_or(0, |q| q.len())
    }

    /// Whether the pool is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Drain up to `max` transactions from the front (oldest first) for block
    /// proposal. Round-robins across shards in ascending shard order so that
    /// no single shard starves others.
    pub fn drain_batch(&self, max: usize) -> Vec<SignedTransaction> {
        let mut inner = self.inner.lock();
        let mut batch = Vec::with_capacity(max.min(inner.total_count));

        // Sorted shard IDs for deterministic ordering.
        let mut shard_ids: Vec<ShardId> = inner.shard_queues.keys().copied().collect();
        shard_ids.sort_by_key(|s| s.0);

        // Round-robin: take one from each shard per round until max is reached.
        let mut drained = true;
        while batch.len() < max && drained {
            drained = false;
            for sid in &shard_ids {
                if batch.len() >= max {
                    break;
                }
                if let Some(queue) = inner.shard_queues.get_mut(sid) {
                    if let Some((digest, tx)) = queue.pop_front() {
                        inner.digests.remove(&digest);
                        inner.total_count -= 1;
                        batch.push(tx);
                        drained = true;
                    }
                }
            }
        }
        batch
    }

    /// Drain up to `max` transactions for a specific shard.
    pub fn drain_batch_for_shard(&self, shard: ShardId, max: usize) -> Vec<SignedTransaction> {
        let mut inner = self.inner.lock();
        // Collect the digests to drain first, then remove from digests set
        // after releasing the queue borrow.
        let queue = match inner.shard_queues.get_mut(&shard) {
            Some(q) => q,
            None => return Vec::new(),
        };
        let count = max.min(queue.len());
        let mut batch = Vec::with_capacity(count);
        let mut drained_digests = Vec::with_capacity(count);
        for _ in 0..count {
            if let Some((digest, tx)) = queue.pop_front() {
                drained_digests.push(digest);
                batch.push(tx);
            }
        }
        // Now we can safely borrow inner.digests again.
        for d in &drained_digests {
            inner.digests.remove(d);
        }
        inner.total_count -= batch.len();
        batch
    }

    /// Evict all transactions whose `expiry_epoch` is less than or equal to `current_epoch`.
    ///
    /// Returns the number of evicted transactions.
    pub fn evict_expired(&self, current_epoch: EpochNumber) -> usize {
        let mut inner = self.inner.lock();
        // First pass: collect expired digests from all shard queues.
        let mut expired_digests: Vec<TxDigest> = Vec::new();
        for queue in inner.shard_queues.values() {
            for (d, tx) in queue.iter() {
                if tx.body.expiry_epoch <= current_epoch {
                    expired_digests.push(*d);
                }
            }
        }
        // Second pass: remove expired from queues and digest set.
        let expired_set: std::collections::HashSet<TxDigest> =
            expired_digests.iter().copied().collect();
        for queue in inner.shard_queues.values_mut() {
            queue.retain(|(_, tx)| tx.body.expiry_epoch > current_epoch);
        }
        for d in &expired_digests {
            inner.digests.remove(d);
        }
        inner.total_count -= expired_set.len();
        expired_set.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_crypto::{DilithiumSigner, Signer};
    use nexus_execution::types::{
        compute_tx_digest, TransactionBody, TransactionPayload, TX_DOMAIN,
    };
    use nexus_primitives::{AccountAddress, Amount, ShardId, TokenId};

    /// Helper: create a signed transaction with the given sequence number.
    fn make_tx(seq: u64, expiry_epoch: u64) -> SignedTransaction {
        make_tx_for_shard(seq, expiry_epoch, ShardId(0))
    }

    /// Helper: create a signed transaction targeting a specific shard.
    fn make_tx_for_shard(seq: u64, expiry_epoch: u64, shard: ShardId) -> SignedTransaction {
        let (sk, vk) = DilithiumSigner::generate_keypair();
        let body = TransactionBody {
            sender: AccountAddress([1u8; 32]),
            sequence_number: seq,
            expiry_epoch: EpochNumber(expiry_epoch),
            gas_limit: 10_000,
            gas_price: 1,
            target_shard: Some(shard),
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

    #[test]
    fn insert_and_check() {
        let pool = Mempool::new(&MempoolConfig {
            capacity: 100,
            num_shards: 1,
        });
        let tx = make_tx(1, 100);
        let digest = tx.digest;

        assert_eq!(pool.insert(tx), InsertResult::Accepted);
        assert!(pool.contains(&digest));
        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn deduplication() {
        let pool = Mempool::new(&MempoolConfig {
            capacity: 100,
            num_shards: 1,
        });
        let tx = make_tx(1, 100);
        let tx_clone = tx.clone();

        assert_eq!(pool.insert(tx), InsertResult::Accepted);
        assert_eq!(pool.insert(tx_clone), InsertResult::Duplicate);
        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn capacity_limit() {
        let pool = Mempool::new(&MempoolConfig {
            capacity: 2,
            num_shards: 1,
        });
        assert_eq!(pool.insert(make_tx(1, 100)), InsertResult::Accepted);
        assert_eq!(pool.insert(make_tx(2, 100)), InsertResult::Accepted);
        assert_eq!(pool.insert(make_tx(3, 100)), InsertResult::PoolFull);
        assert_eq!(pool.len(), 2);
    }

    #[test]
    fn remove_by_digest() {
        let pool = Mempool::new(&MempoolConfig {
            capacity: 100,
            num_shards: 1,
        });
        let tx = make_tx(1, 100);
        let digest = tx.digest;

        pool.insert(tx);
        let removed = pool.remove(&digest);
        assert!(removed.is_some());
        assert!(!pool.contains(&digest));
        assert_eq!(pool.len(), 0);
    }

    #[test]
    fn drain_batch_respects_max() {
        let pool = Mempool::new(&MempoolConfig {
            capacity: 100,
            num_shards: 1,
        });
        for i in 0..5 {
            pool.insert(make_tx(i, 100));
        }

        let batch = pool.drain_batch(3);
        assert_eq!(batch.len(), 3);
        assert_eq!(pool.len(), 2);

        // Drain remaining
        let rest = pool.drain_batch(10);
        assert_eq!(rest.len(), 2);
        assert!(pool.is_empty());
    }

    #[test]
    fn evict_expired_removes_old() {
        let pool = Mempool::new(&MempoolConfig {
            capacity: 100,
            num_shards: 1,
        });
        pool.insert(make_tx(1, 5)); // expires at epoch 5
        pool.insert(make_tx(2, 10)); // expires at epoch 10
        pool.insert(make_tx(3, 3)); // expires at epoch 3

        let evicted = pool.evict_expired(EpochNumber(5));
        assert_eq!(evicted, 2); // epochs 3 and 5 evicted
        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn empty_pool_operations() {
        let pool = Mempool::new(&MempoolConfig::default());
        assert!(pool.is_empty());
        assert_eq!(pool.len(), 0);
        assert_eq!(pool.drain_batch(10).len(), 0);
        assert_eq!(pool.evict_expired(EpochNumber(100)), 0);

        let fake_digest = TxDigest::from_bytes([0u8; 32]);
        assert!(!pool.contains(&fake_digest));
        assert!(pool.remove(&fake_digest).is_none());
    }

    #[test]
    fn mempool_is_send_sync() {
        fn assert_bounds<T: Send + Sync>() {}
        assert_bounds::<Mempool>();
    }

    // ── Phase U: shard-partitioned mempool tests ─────────────────────────

    #[test]
    fn shard_partitioned_insert_and_drain_per_shard() {
        let pool = Mempool::new(&MempoolConfig {
            capacity: 100,
            num_shards: 4,
        });
        let tx0 = make_tx_for_shard(1, 100, ShardId(0));
        let tx1 = make_tx_for_shard(2, 100, ShardId(1));
        let tx2 = make_tx_for_shard(3, 100, ShardId(2));
        let tx3 = make_tx_for_shard(4, 100, ShardId(3));

        assert_eq!(pool.insert(tx0), InsertResult::Accepted);
        assert_eq!(pool.insert(tx1), InsertResult::Accepted);
        assert_eq!(pool.insert(tx2), InsertResult::Accepted);
        assert_eq!(pool.insert(tx3), InsertResult::Accepted);
        assert_eq!(pool.len(), 4);

        // Drain from shard 1 only.
        let batch = pool.drain_batch_for_shard(ShardId(1), 10);
        assert_eq!(batch.len(), 1);
        assert_eq!(pool.len(), 3);
        assert_eq!(pool.len_for_shard(ShardId(1)), 0);

        // Drain from shard 2 only.
        let batch2 = pool.drain_batch_for_shard(ShardId(2), 10);
        assert_eq!(batch2.len(), 1);
        assert_eq!(pool.len(), 2);
    }

    #[test]
    fn invalid_shard_rejected() {
        let pool = Mempool::new(&MempoolConfig {
            capacity: 100,
            num_shards: 2,
        });
        let tx = make_tx_for_shard(1, 100, ShardId(5)); // shard 5 >= num_shards 2
        assert_eq!(pool.insert(tx), InsertResult::InvalidShard);
        assert_eq!(pool.len(), 0);
    }

    #[test]
    fn drain_batch_round_robins_across_shards() {
        let pool = Mempool::new(&MempoolConfig {
            capacity: 100,
            num_shards: 2,
        });
        // Insert 3 txs for shard 0, 2 txs for shard 1.
        for i in 0..3 {
            pool.insert(make_tx_for_shard(i, 100, ShardId(0)));
        }
        for i in 10..12 {
            pool.insert(make_tx_for_shard(i, 100, ShardId(1)));
        }
        assert_eq!(pool.len(), 5);

        // Drain 3 → should get round-robin: shard0, shard1, shard0.
        let batch = pool.drain_batch(3);
        assert_eq!(batch.len(), 3);
        assert_eq!(pool.len(), 2);

        // Remaining: 1 in shard 0, 1 in shard 1.
        assert_eq!(pool.len_for_shard(ShardId(0)), 1);
        assert_eq!(pool.len_for_shard(ShardId(1)), 1);
    }

    #[test]
    fn empty_shard_does_not_block_drain() {
        let pool = Mempool::new(&MempoolConfig {
            capacity: 100,
            num_shards: 4,
        });
        // Only insert into shard 2.
        pool.insert(make_tx_for_shard(1, 100, ShardId(2)));
        pool.insert(make_tx_for_shard(2, 100, ShardId(2)));

        let batch = pool.drain_batch(10);
        assert_eq!(batch.len(), 2);
        assert!(pool.is_empty());
    }

    #[test]
    fn shard_evict_expired_works_across_shards() {
        let pool = Mempool::new(&MempoolConfig {
            capacity: 100,
            num_shards: 2,
        });
        pool.insert(make_tx_for_shard(1, 5, ShardId(0))); // expires at 5
        pool.insert(make_tx_for_shard(2, 10, ShardId(1))); // expires at 10
        pool.insert(make_tx_for_shard(3, 3, ShardId(0))); // expires at 3

        let evicted = pool.evict_expired(EpochNumber(5));
        assert_eq!(evicted, 2); // epochs 3 and 5
        assert_eq!(pool.len(), 1);
        assert_eq!(pool.len_for_shard(ShardId(0)), 0);
        assert_eq!(pool.len_for_shard(ShardId(1)), 1);
    }

    #[test]
    fn single_shard_compat_with_v019() {
        // num_shards=1 should behave identically to pre-Phase-U mempool.
        let pool = Mempool::new(&MempoolConfig {
            capacity: 100,
            num_shards: 1,
        });
        for i in 0..5 {
            pool.insert(make_tx(i, 100));
        }
        assert_eq!(pool.len(), 5);
        let batch = pool.drain_batch(3);
        assert_eq!(batch.len(), 3);
        assert_eq!(pool.len(), 2);
    }

    #[test]
    fn num_shards_accessor() {
        let pool = Mempool::new(&MempoolConfig {
            capacity: 100,
            num_shards: 8,
        });
        assert_eq!(pool.num_shards(), 8);
    }

    #[test]
    fn len_for_shard_nonexistent() {
        let pool = Mempool::new(&MempoolConfig {
            capacity: 100,
            num_shards: 2,
        });
        assert_eq!(pool.len_for_shard(ShardId(99)), 0);
    }

    #[test]
    fn target_shard_none_routes_to_shard_zero() {
        let pool = Mempool::new(&MempoolConfig {
            capacity: 100,
            num_shards: 4,
        });
        // Create a tx with target_shard = None (should route to shard 0).
        let (sk, vk) = DilithiumSigner::generate_keypair();
        let body = TransactionBody {
            sender: AccountAddress([1u8; 32]),
            sequence_number: 42,
            expiry_epoch: EpochNumber(100),
            gas_limit: 10_000,
            gas_price: 1,
            target_shard: None,
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
        let tx = SignedTransaction {
            body,
            signature: sig,
            sender_pk: vk,
            digest,
        };
        assert_eq!(pool.insert(tx), InsertResult::Accepted);
        assert_eq!(pool.len_for_shard(ShardId(0)), 1);
    }
}
