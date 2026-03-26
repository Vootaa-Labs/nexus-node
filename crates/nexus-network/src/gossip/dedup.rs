// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Message deduplication — LRU cache with Blake3 fingerprints.
//!
//! Prevents processing the same GossipSub message twice. Messages are
//! identified by their Blake3 hash; the cache evicts least-recently-seen
//! entries when full.

use std::collections::HashSet;
use std::collections::VecDeque;

use nexus_primitives::Blake3Digest;
use tracing::trace;

use crate::config::NetworkConfig;

/// Default maximum number of message fingerprints to retain.
const DEFAULT_CACHE_SIZE: usize = 65_536;

/// LRU-based message deduplication cache.
///
/// Uses Blake3 hashes as fingerprints. When the cache is full, the oldest
/// entries are evicted (FIFO order).
pub struct MessageDedup {
    seen: HashSet<Blake3Digest>,
    order: VecDeque<Blake3Digest>,
    max_size: usize,
}

impl MessageDedup {
    /// Create a new dedup cache with the default size.
    pub fn new(_config: &NetworkConfig) -> Self {
        Self::with_capacity(DEFAULT_CACHE_SIZE)
    }

    /// Create a dedup cache with a specific capacity.
    pub fn with_capacity(max_size: usize) -> Self {
        Self {
            seen: HashSet::with_capacity(max_size),
            order: VecDeque::with_capacity(max_size),
            max_size,
        }
    }

    /// Check if a message is new (not seen before).
    ///
    /// If new, the message fingerprint is inserted into the cache and `true`
    /// is returned. If already seen, returns `false`.
    pub fn is_new(&mut self, data: &[u8]) -> bool {
        let digest = nexus_crypto::Blake3Hasher::digest(b"nexus::gossip::dedup::v1", data);

        if self.seen.contains(&digest) {
            trace!("duplicate message detected");
            return false;
        }

        // Evict oldest if full
        if self.seen.len() >= self.max_size {
            if let Some(old) = self.order.pop_front() {
                self.seen.remove(&old);
            }
        }

        self.seen.insert(digest);
        self.order.push_back(digest);
        true
    }

    /// Number of tracked fingerprints.
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }

    /// Clear all tracked fingerprints.
    pub fn clear(&mut self) {
        self.seen.clear();
        self.order.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_message_returns_true() {
        let mut dedup = MessageDedup::with_capacity(100);
        assert!(dedup.is_new(b"hello"));
    }

    #[test]
    fn duplicate_returns_false() {
        let mut dedup = MessageDedup::with_capacity(100);
        assert!(dedup.is_new(b"hello"));
        assert!(!dedup.is_new(b"hello"));
    }

    #[test]
    fn different_messages_are_new() {
        let mut dedup = MessageDedup::with_capacity(100);
        assert!(dedup.is_new(b"msg-1"));
        assert!(dedup.is_new(b"msg-2"));
        assert!(dedup.is_new(b"msg-3"));
        assert_eq!(dedup.len(), 3);
    }

    #[test]
    fn eviction_after_capacity() {
        let mut dedup = MessageDedup::with_capacity(3);
        assert!(dedup.is_new(b"a"));
        assert!(dedup.is_new(b"b"));
        assert!(dedup.is_new(b"c"));
        assert_eq!(dedup.len(), 3);

        // This should evict "a"
        assert!(dedup.is_new(b"d"));
        assert_eq!(dedup.len(), 3);

        // "a" was evicted, so it's "new" again
        assert!(dedup.is_new(b"a"));
        // "b" was evicted by "a"
        assert!(dedup.is_new(b"b"));
    }

    #[test]
    fn clear_resets_cache() {
        let mut dedup = MessageDedup::with_capacity(100);
        dedup.is_new(b"hello");
        assert_eq!(dedup.len(), 1);

        dedup.clear();
        assert!(dedup.is_empty());
        assert!(dedup.is_new(b"hello")); // should be "new" again
    }

    #[test]
    fn empty_data_is_valid() {
        let mut dedup = MessageDedup::with_capacity(100);
        assert!(dedup.is_new(b""));
        assert!(!dedup.is_new(b""));
    }

    #[test]
    fn capacity_one() {
        let mut dedup = MessageDedup::with_capacity(1);
        assert!(dedup.is_new(b"first"));
        assert!(dedup.is_new(b"second")); // evicts "first"
        assert!(dedup.is_new(b"first")); // "first" was evicted
        assert!(!dedup.is_new(b"first")); // now "first" is cached
    }
}
