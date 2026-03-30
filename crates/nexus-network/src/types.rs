// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Core network types: peer identity, connection state, topics, wire protocol.
//!
//! These types form the vocabulary shared by all network subsystems.
//! Wire-format constants are **FROZEN-3** — changes require a hard fork.

use std::fmt;
use std::time::Instant;

use nexus_primitives::Blake3Digest;
use serde::{Deserialize, Serialize};

// ── Peer Identity ─────────────────────────────────────────────────────────────

/// Post-quantum peer identity derived from an ML-DSA public key.
///
/// ```text
/// PeerId = BLAKE3(b"nexus::p2p::peer_id::v1" ‖ dilithium_pubkey_bytes)
/// ```
///
/// This is a content-addressed identifier — deterministic from the public key,
/// no randomness involved.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PeerId(Blake3Digest);

impl PeerId {
    /// Derive a `PeerId` from a raw ML-DSA public key.
    pub fn from_public_key(dilithium_pk_bytes: &[u8]) -> Self {
        let digest =
            nexus_crypto::Blake3Hasher::digest(b"nexus::p2p::peer_id::v1", dilithium_pk_bytes);
        Self(digest)
    }

    /// View the underlying 32-byte digest.
    pub fn as_bytes(&self) -> &[u8; 32] {
        self.0.as_bytes()
    }

    /// Wrap raw digest bytes as a `PeerId` (e.g. when deserializing).
    pub fn from_digest(digest: Blake3Digest) -> Self {
        Self(digest)
    }

    /// Return the inner `Blake3Digest`.
    pub fn digest(&self) -> &Blake3Digest {
        &self.0
    }

    /// Create a `PeerId` from a libp2p `PeerId` by hashing its bytes.
    ///
    /// This maps the libp2p identity into the Nexus PeerId space.
    pub fn from_libp2p(libp2p_peer: &libp2p::PeerId) -> Self {
        let digest = nexus_crypto::Blake3Hasher::digest(
            b"nexus::p2p::peer_id::v1",
            libp2p_peer.to_bytes().as_slice(),
        );
        Self(digest)
    }
}

impl fmt::Debug for PeerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hex = self.0.to_hex();
        write!(f, "PeerId({}…{})", &hex[..8], &hex[56..])
    }
}

impl fmt::Display for PeerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hex = self.0.to_hex();
        write!(f, "{}…{}", &hex[..8], &hex[56..])
    }
}

// ── Connection State ──────────────────────────────────────────────────────────

/// Connection state for a peer.
#[derive(Debug, Clone)]
pub enum ConnectionState {
    /// Actively connected with measured latency.
    Connected {
        /// Round-trip latency in milliseconds.
        latency_ms: u32,
    },
    /// Connection attempt in progress.
    Connecting,
    /// Not connected, idle.
    Disconnected,
    /// Banned due to misbehaviour. Will not accept reconnection until expiry.
    Banned {
        /// Time at which the ban expires.
        until: Instant,
    },
}

impl ConnectionState {
    /// Whether the peer is currently usable for sending messages.
    pub fn is_connected(&self) -> bool {
        matches!(self, Self::Connected { .. })
    }
}

// ── GossipSub Topics ─────────────────────────────────────────────────────────

/// GossipSub topics for P2P message routing.
///
/// The original four global topics (`Consensus`, `Transaction`, `Intent`,
/// `StateSync`) are **FROZEN-3** and remain the canonical topics for
/// single-shard mode.
///
/// The `ShardedTransaction` and `ShardedCertificate` variants extend the
/// topic space for multi-shard networks: each shard has its own gossip
/// topic so nodes only receive messages for the shards they are responsible
/// for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Topic {
    /// Narwhal batch & certificate dissemination (global / shard-0 legacy).
    Consensus,
    /// Client transaction broadcast (global / shard-0 legacy).
    Transaction,
    /// User intent broadcast (global).
    Intent,
    /// State synchronization messages (global).
    StateSync,
    /// Per-shard transaction broadcast.
    ShardedTransaction(u16),
    /// Per-shard certificate dissemination.
    ShardedCertificate(u16),
}

impl Topic {
    /// Protocol-level topic string used in GossipSub subscriptions.
    ///
    /// For the four global topics the string is a `&'static str`.
    /// For sharded topics a dynamic string is returned via
    /// [`topic_string`](Self::topic_string).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Consensus => "/nexus/consensus/1.0",
            Self::Transaction => "/nexus/tx/1.0",
            Self::Intent => "/nexus/intent/1.0",
            Self::StateSync => "/nexus/sync/1.0",
            // Sharded topics use dynamic strings — callers needing an owned
            // String should use `topic_string()` instead.
            Self::ShardedTransaction(_) => "/nexus/tx/shard",
            Self::ShardedCertificate(_) => "/nexus/cert/shard",
        }
    }

    /// Full topic string including the shard identifier for sharded topics.
    ///
    /// For global topics this returns the same value as `as_str()`.
    pub fn topic_string(&self) -> String {
        match self {
            Self::Consensus => "/nexus/consensus/1.0".to_owned(),
            Self::Transaction => "/nexus/tx/1.0".to_owned(),
            Self::Intent => "/nexus/intent/1.0".to_owned(),
            Self::StateSync => "/nexus/sync/1.0".to_owned(),
            Self::ShardedTransaction(shard) => format!("/nexus/tx/1.0/shard/{shard}"),
            Self::ShardedCertificate(shard) => format!("/nexus/cert/1.0/shard/{shard}"),
        }
    }

    /// Convenience constructors for per-shard transaction topics.
    pub fn sharded_tx(shard_id: u16) -> Self {
        Self::ShardedTransaction(shard_id)
    }

    /// Convenience constructor for per-shard certificate topics.
    pub fn sharded_cert(shard_id: u16) -> Self {
        Self::ShardedCertificate(shard_id)
    }

    /// Whether this topic is shard-specific (as opposed to global).
    pub fn is_sharded(&self) -> bool {
        matches!(
            self,
            Self::ShardedTransaction(_) | Self::ShardedCertificate(_)
        )
    }

    /// Return the shard id if this is a sharded topic.
    pub fn shard_id(&self) -> Option<u16> {
        match self {
            Self::ShardedTransaction(s) | Self::ShardedCertificate(s) => Some(*s),
            _ => None,
        }
    }

    /// Return all global (non-sharded) topics.
    pub fn global_topics() -> &'static [Topic] {
        &[
            Topic::Consensus,
            Topic::Transaction,
            Topic::Intent,
            Topic::StateSync,
        ]
    }

    /// Generate the set of shard-local topics for a given shard count.
    pub fn shard_topics(num_shards: u16) -> Vec<Topic> {
        let mut topics = Vec::with_capacity(num_shards as usize * 2);
        for s in 0..num_shards {
            topics.push(Topic::ShardedTransaction(s));
            topics.push(Topic::ShardedCertificate(s));
        }
        topics
    }
}

impl fmt::Display for Topic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.topic_string())
    }
}

// ── Wire Protocol Constants ──────────────────────────────────────────────────

/// Wire protocol magic bytes: `"NEXU"`. **FROZEN-3**.
pub const WIRE_MAGIC: [u8; 4] = [0x4E, 0x45, 0x58, 0x55];

/// Current wire protocol version. **FROZEN-3**.
pub const WIRE_VERSION: u8 = 1;

/// Message type discriminator in the wire header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum MessageType {
    /// P2P handshake / identity exchange.
    Handshake = 0,
    /// Narwhal consensus payload (batch or certificate).
    Consensus = 1,
    /// Client transaction.
    Transaction = 2,
    /// User intent.
    Intent = 3,
    /// State synchronization data.
    StateSync = 4,
}

impl MessageType {
    /// Decode from a raw byte.
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            0 => Some(Self::Handshake),
            1 => Some(Self::Consensus),
            2 => Some(Self::Transaction),
            3 => Some(Self::Intent),
            4 => Some(Self::StateSync),
            _ => None,
        }
    }

    /// Maximum allowed payload size in bytes for this message type.
    pub fn max_payload_size(&self) -> usize {
        match self {
            Self::Handshake => 8 * 1024,        // 8 KB
            Self::Consensus => 4 * 1024 * 1024, // 4 MB (PQ-signed certificate batches)
            Self::Transaction => 64 * 1024,     // 64 KB
            Self::Intent => 64 * 1024,          // 64 KB
            Self::StateSync => 1024 * 1024,     // 1 MB (state chunks)
        }
    }
}

/// Wire header size in bytes: `MAGIC(4) + VERSION(1) + TYPE(1) = 6`.
pub const WIRE_HEADER_SIZE: usize = 6;

// ── Peer Score ───────────────────────────────────────────────────────────────

/// Peer reputation score used for GossipSub relay decisions.
///
/// Scores range from -100.0 (fully distrusted) to +100.0 (fully trusted).
/// Peers below the `PEER_SCORE_THRESHOLD` are deprioritized or disconnected.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct PeerScore(f64);

/// Threshold below which a peer is considered misbehaving.
pub const PEER_SCORE_THRESHOLD: f64 = -10.0;

impl PeerScore {
    /// Create a new score, clamping to `[-100.0, 100.0]`.
    pub fn new(value: f64) -> Self {
        Self(value.clamp(-100.0, 100.0))
    }

    /// Initial default score for new peers.
    pub fn default_score() -> Self {
        Self(0.0)
    }

    /// Underlying value.
    pub fn value(&self) -> f64 {
        self.0
    }

    /// Whether this peer is above the misbehaviour threshold.
    pub fn is_trusted(&self) -> bool {
        self.0 > PEER_SCORE_THRESHOLD
    }
}

// ── Routing Health ───────────────────────────────────────────────────────────

/// DHT routing table health snapshot.
#[derive(Debug, Clone)]
pub struct RoutingHealth {
    /// Number of distinct peers in the routing table.
    pub known_peers: usize,
    /// Number of non-empty Kademlia buckets.
    pub filled_buckets: usize,
    /// Total Kademlia buckets (typically 256 for 256-bit key space).
    pub total_buckets: usize,
}

impl RoutingHealth {
    /// Whether the routing table is healthy enough for DHT operations.
    ///
    /// Heuristic: at least 3 filled buckets and 6 known peers.
    pub fn is_healthy(&self) -> bool {
        self.filled_buckets >= 3 && self.known_peers >= 6
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_id_deterministic() {
        let pk = b"fake-dilithium-public-key-bytes-for-testing";
        let id1 = PeerId::from_public_key(pk);
        let id2 = PeerId::from_public_key(pk);
        assert_eq!(id1, id2);
    }

    #[test]
    fn peer_id_different_keys_differ() {
        let id1 = PeerId::from_public_key(b"key-a");
        let id2 = PeerId::from_public_key(b"key-b");
        assert_ne!(id1, id2);
    }

    #[test]
    fn peer_id_display_format() {
        let id = PeerId::from_public_key(b"test");
        let s = format!("{}", id);
        assert!(s.contains('…'), "display must use abbreviated hex");
    }

    #[test]
    fn topic_strings_are_stable() {
        // FROZEN-3: these strings must not change
        assert_eq!(Topic::Consensus.as_str(), "/nexus/consensus/1.0");
        assert_eq!(Topic::Transaction.as_str(), "/nexus/tx/1.0");
        assert_eq!(Topic::Intent.as_str(), "/nexus/intent/1.0");
        assert_eq!(Topic::StateSync.as_str(), "/nexus/sync/1.0");
    }

    #[test]
    fn message_type_roundtrip() {
        for byte in 0..=4u8 {
            let mt = MessageType::from_byte(byte).unwrap();
            assert_eq!(mt as u8, byte);
        }
        assert!(MessageType::from_byte(5).is_none());
        assert!(MessageType::from_byte(255).is_none());
    }

    #[test]
    fn message_size_limits() {
        assert_eq!(MessageType::Handshake.max_payload_size(), 8 * 1024);
        assert_eq!(MessageType::Consensus.max_payload_size(), 4 * 1024 * 1024);
        assert_eq!(MessageType::StateSync.max_payload_size(), 1024 * 1024);
    }

    #[test]
    fn peer_score_clamping() {
        let high = PeerScore::new(200.0);
        assert_eq!(high.value(), 100.0);
        let low = PeerScore::new(-500.0);
        assert_eq!(low.value(), -100.0);
        assert!(PeerScore::default_score().is_trusted());
    }

    #[test]
    fn peer_score_trust_threshold() {
        assert!(PeerScore::new(0.0).is_trusted());
        assert!(PeerScore::new(-9.9).is_trusted());
        assert!(!PeerScore::new(-10.0).is_trusted());
        assert!(!PeerScore::new(-50.0).is_trusted());
    }

    #[test]
    fn routing_health_check() {
        let healthy = RoutingHealth {
            known_peers: 10,
            filled_buckets: 5,
            total_buckets: 256,
        };
        assert!(healthy.is_healthy());

        let unhealthy = RoutingHealth {
            known_peers: 2,
            filled_buckets: 1,
            total_buckets: 256,
        };
        assert!(!unhealthy.is_healthy());
    }

    #[test]
    fn connection_state_is_connected() {
        assert!(ConnectionState::Connected { latency_ms: 10 }.is_connected());
        assert!(!ConnectionState::Connecting.is_connected());
        assert!(!ConnectionState::Disconnected.is_connected());
    }

    // ── W-1: Shard-aware topic tests ─────────────────────────────────────

    #[test]
    fn sharded_topic_strings_include_shard_id() {
        let tx0 = Topic::ShardedTransaction(0);
        assert_eq!(tx0.topic_string(), "/nexus/tx/1.0/shard/0");
        let tx3 = Topic::ShardedTransaction(3);
        assert_eq!(tx3.topic_string(), "/nexus/tx/1.0/shard/3");

        let cert1 = Topic::ShardedCertificate(1);
        assert_eq!(cert1.topic_string(), "/nexus/cert/1.0/shard/1");
    }

    #[test]
    fn global_topics_not_sharded() {
        assert!(!Topic::Consensus.is_sharded());
        assert!(!Topic::Transaction.is_sharded());
        assert!(!Topic::Intent.is_sharded());
        assert!(!Topic::StateSync.is_sharded());
    }

    #[test]
    fn sharded_topics_are_sharded() {
        assert!(Topic::ShardedTransaction(0).is_sharded());
        assert!(Topic::ShardedCertificate(2).is_sharded());
    }

    #[test]
    fn shard_id_extraction() {
        assert_eq!(Topic::ShardedTransaction(5).shard_id(), Some(5));
        assert_eq!(Topic::ShardedCertificate(0).shard_id(), Some(0));
        assert_eq!(Topic::Consensus.shard_id(), None);
    }

    #[test]
    fn shard_topics_generation() {
        let topics = Topic::shard_topics(2);
        assert_eq!(topics.len(), 4);
        assert!(topics.contains(&Topic::ShardedTransaction(0)));
        assert!(topics.contains(&Topic::ShardedTransaction(1)));
        assert!(topics.contains(&Topic::ShardedCertificate(0)));
        assert!(topics.contains(&Topic::ShardedCertificate(1)));
    }

    #[test]
    fn sharded_topics_distinct_hashes() {
        let t0 = Topic::ShardedTransaction(0);
        let t1 = Topic::ShardedTransaction(1);
        assert_ne!(t0.topic_string(), t1.topic_string());
        assert_ne!(t0, t1);
    }

    #[test]
    fn global_topic_strings_backward_compatible() {
        // Ensure global topic topic_string() matches as_str()
        assert_eq!(Topic::Consensus.topic_string(), "/nexus/consensus/1.0");
        assert_eq!(Topic::Transaction.topic_string(), "/nexus/tx/1.0");
        assert_eq!(Topic::Intent.topic_string(), "/nexus/intent/1.0");
        assert_eq!(Topic::StateSync.topic_string(), "/nexus/sync/1.0");
    }

    #[test]
    fn convenience_constructors() {
        assert_eq!(Topic::sharded_tx(3), Topic::ShardedTransaction(3));
        assert_eq!(Topic::sharded_cert(7), Topic::ShardedCertificate(7));
    }

    // ── Additional coverage: PeerId from_libp2p / from_digest / Display ──

    #[test]
    fn peer_id_from_libp2p_is_deterministic() {
        let libp2p_id = libp2p::PeerId::random();
        let a = PeerId::from_libp2p(&libp2p_id);
        let b = PeerId::from_libp2p(&libp2p_id);
        assert_eq!(a, b);
    }

    #[test]
    fn peer_id_from_libp2p_differs_per_peer() {
        let id1 = PeerId::from_libp2p(&libp2p::PeerId::random());
        let id2 = PeerId::from_libp2p(&libp2p::PeerId::random());
        assert_ne!(id1, id2);
    }

    #[test]
    fn peer_id_from_digest_roundtrip() {
        let pk = b"roundtrip-key";
        let original = PeerId::from_public_key(pk);
        let rebuilt = PeerId::from_digest(*original.digest());
        assert_eq!(original, rebuilt);
        assert_eq!(original.as_bytes(), rebuilt.as_bytes());
    }

    #[test]
    fn peer_id_debug_format() {
        let id = PeerId::from_public_key(b"debug");
        let dbg = format!("{:?}", id);
        assert!(dbg.starts_with("PeerId("), "debug: {dbg}");
        assert!(dbg.contains('…'));
    }

    #[test]
    fn topic_display_matches_topic_string() {
        for topic in Topic::global_topics() {
            assert_eq!(format!("{}", topic), topic.topic_string());
        }
        let sharded = Topic::ShardedTransaction(9);
        assert_eq!(format!("{}", sharded), sharded.topic_string());
    }

    #[test]
    fn connection_state_banned_is_not_connected() {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
        assert!(!ConnectionState::Banned { until }.is_connected());
    }

    #[test]
    fn routing_health_boundary() {
        // Exactly at threshold: 3 buckets, 6 peers → healthy
        assert!(RoutingHealth { known_peers: 6, filled_buckets: 3, total_buckets: 256 }.is_healthy());
        // Just below: 3 buckets, 5 peers → not healthy
        assert!(!RoutingHealth { known_peers: 5, filled_buckets: 3, total_buckets: 256 }.is_healthy());
        // 2 buckets, 6 peers → not healthy
        assert!(!RoutingHealth { known_peers: 6, filled_buckets: 2, total_buckets: 256 }.is_healthy());
    }

    #[test]
    fn global_topics_count() {
        assert_eq!(Topic::global_topics().len(), 4);
    }

    #[test]
    fn shard_topics_zero_shards() {
        assert!(Topic::shard_topics(0).is_empty());
    }

    #[test]
    fn message_type_unknown_bytes() {
        for b in 5..=10 {
            assert!(MessageType::from_byte(b).is_none());
        }
    }
}
