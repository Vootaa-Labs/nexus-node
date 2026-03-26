//! Per-peer, per-topic token-bucket rate limiter.
//!
//! Uses the [`governor`] crate (Generic Cell Rate Algorithm / GCRA) to enforce
//! configurable message-per-second limits. Each peer+topic combination gets its
//! own independent bucket so that a chatty peer on one topic cannot starve
//! traffic on another.

use std::num::NonZeroU32;

use dashmap::DashMap;
use governor::clock::DefaultClock;
use governor::state::{InMemoryState, NotKeyed};
use governor::{Quota, RateLimiter as GovLimiter};

use crate::types::{PeerId, Topic};

/// Token-bucket rate limiter key: (peer, topic).
type BucketKey = (PeerId, Topic);

/// A single-cell GCRA bucket from governor.
type Bucket = GovLimiter<NotKeyed, InMemoryState, DefaultClock>;

/// Per-peer, per-topic rate limiter.
///
/// Thread-safe thanks to [`DashMap`] — concurrent readers and writers
/// across peer/topic combos do not block each other.
pub struct PeerRateLimiter {
    buckets: DashMap<BucketKey, Bucket>,
    /// Messages allowed per second per peer per topic.
    rps: NonZeroU32,
    /// Maximum number of tracked buckets (memory bound).
    max_buckets: usize,
}

/// Default maximum tracked rate-limit buckets.
const DEFAULT_MAX_BUCKETS: usize = 50_000;

impl PeerRateLimiter {
    /// Create a new limiter with the given per-peer-per-topic rate.
    pub fn new(messages_per_second: u32) -> Self {
        let rps = NonZeroU32::new(messages_per_second.max(1)).expect("max(1) ensures non-zero");
        Self {
            buckets: DashMap::new(),
            rps,
            max_buckets: DEFAULT_MAX_BUCKETS,
        }
    }

    /// Check whether a message from `peer` on `topic` is allowed.
    ///
    /// Returns `true` if the message passes the rate limit, `false` if it
    /// should be dropped / rejected.
    /// Create a limiter with a custom bucket cap (for testing).
    #[cfg(test)]
    fn with_max_buckets(messages_per_second: u32, max_buckets: usize) -> Self {
        let rps = NonZeroU32::new(messages_per_second.max(1)).expect("max(1) ensures non-zero");
        Self {
            buckets: DashMap::new(),
            rps,
            max_buckets,
        }
    }

    /// Returns `true` if the peer is allowed to send on the given topic.
    pub fn check(&self, peer: &PeerId, topic: Topic) -> bool {
        let key = (*peer, topic);

        // Bound memory: if at capacity and the peer+topic is unknown,
        // reject (fail-closed) — SEC-M14.
        if self.buckets.len() >= self.max_buckets && !self.buckets.contains_key(&key) {
            return false;
        }

        let bucket = self.buckets.entry(key).or_insert_with(|| {
            let quota = Quota::per_second(self.rps);
            GovLimiter::direct(quota)
        });
        bucket.check().is_ok()
    }

    /// Remove all buckets for a given peer (e.g. after disconnect).
    pub fn remove_peer(&self, peer: &PeerId) {
        self.buckets.retain(|(p, _), _| p != peer);
    }

    /// Number of active buckets (for diagnostics).
    pub fn active_buckets(&self) -> usize {
        self.buckets.len()
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_peer() -> PeerId {
        PeerId::from_public_key(b"rate-limit-test-peer")
    }

    #[test]
    fn first_message_always_passes() {
        let limiter = PeerRateLimiter::new(10);
        assert!(limiter.check(&test_peer(), Topic::Transaction));
    }

    #[test]
    fn limiter_creates_buckets_on_demand() {
        let limiter = PeerRateLimiter::new(100);
        assert_eq!(limiter.active_buckets(), 0);

        limiter.check(&test_peer(), Topic::Transaction);
        assert_eq!(limiter.active_buckets(), 1);

        limiter.check(&test_peer(), Topic::Consensus);
        assert_eq!(limiter.active_buckets(), 2);
    }

    #[test]
    fn different_peers_have_independent_buckets() {
        let limiter = PeerRateLimiter::new(1);
        let peer_a = PeerId::from_public_key(b"peer-a");
        let peer_b = PeerId::from_public_key(b"peer-b");

        // Drain peer_a's bucket
        assert!(limiter.check(&peer_a, Topic::Transaction));
        // peer_a is now at limit

        // peer_b should still be fine
        assert!(limiter.check(&peer_b, Topic::Transaction));
    }

    #[test]
    fn different_topics_have_independent_buckets() {
        let limiter = PeerRateLimiter::new(1);
        let peer = test_peer();

        assert!(limiter.check(&peer, Topic::Transaction));
        // TX bucket at limit, but Consensus should be fine
        assert!(limiter.check(&peer, Topic::Consensus));
    }

    #[test]
    fn remove_peer_clears_all_topic_buckets() {
        let limiter = PeerRateLimiter::new(100);
        let peer = test_peer();

        limiter.check(&peer, Topic::Transaction);
        limiter.check(&peer, Topic::Consensus);
        limiter.check(&peer, Topic::Intent);
        assert_eq!(limiter.active_buckets(), 3);

        limiter.remove_peer(&peer);
        assert_eq!(limiter.active_buckets(), 0);
    }

    #[test]
    fn zero_rps_treated_as_one() {
        // Constructor clamps to max(1)
        let limiter = PeerRateLimiter::new(0);
        assert!(limiter.check(&test_peer(), Topic::Transaction));
    }

    #[test]
    fn high_rate_allows_burst() {
        let limiter = PeerRateLimiter::new(1000);
        let peer = test_peer();

        // All rapid calls should pass at 1000/s
        for _ in 0..50 {
            assert!(limiter.check(&peer, Topic::Transaction));
        }
    }

    #[test]
    fn network_rate_limiter_should_fail_closed_when_bucket_table_is_full() {
        // Use a tiny cap so we can fill it quickly.
        let limiter = PeerRateLimiter::with_max_buckets(100, 3);

        let p1 = PeerId::from_public_key(b"peer-1");
        let p2 = PeerId::from_public_key(b"peer-2");
        let p3 = PeerId::from_public_key(b"peer-3");

        // Fill 3 slots (one bucket per peer+topic combo).
        assert!(limiter.check(&p1, Topic::Transaction));
        assert!(limiter.check(&p2, Topic::Transaction));
        assert!(limiter.check(&p3, Topic::Transaction));
        assert_eq!(limiter.active_buckets(), 3);

        // Table is full — a new peer must be rejected (fail-closed).
        let p4 = PeerId::from_public_key(b"peer-4");
        assert!(
            !limiter.check(&p4, Topic::Transaction),
            "should fail-closed when bucket table is full"
        );

        // Existing tracked peer should still be served from its bucket.
        assert!(limiter.check(&p1, Topic::Transaction));
    }
}
