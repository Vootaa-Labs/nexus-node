//! Core network trait contracts — **FROZEN-2** interface definitions.
//!
//! These traits define the stable boundary between the network layer and its
//! consumers (consensus, execution, etc.). Changing signatures requires an RFC
//! and 2-week notice period per FROZEN-2 policy.
//!
//! # Traits
//! | Trait | Stability | Purpose |
//! |-------|-----------|---------|
//! | [`NetworkTransport`] | **FROZEN-2** | Point-to-point and broadcast messaging |
//! | [`GossipNetwork`] | **FROZEN-2** | Topic-based pub/sub over GossipSub 1.1 |
//! | [`DhtDiscovery`] | **FROZEN-2** | S/Kademlia DHT peer discovery |

use std::future::Future;

use serde::Serialize;

use crate::error::NetworkError;
use crate::types::{ConnectionState, PeerId, RoutingHealth, Topic};

// ── NetworkTransport [FROZEN-2] ──────────────────────────────────────────────

/// Point-to-point and broadcast message transport.
///
/// Provides **best-effort delivery** — the application layer is responsible for
/// retransmission and acknowledgement. Implementations must be `Clone` (handle-
/// based; the actual connection pool lives behind an `Arc`).
///
/// # Implementations
/// - `QuicTransport` (production, future T-0005)
/// - `MockTransport` (testing)
pub trait NetworkTransport: Send + Sync + Clone + 'static {
    /// Serializable message type carried by this transport.
    type Message: Send + 'static;

    /// Send `msg` to a specific peer.
    ///
    /// Returns `Ok(())` when the message has been queued (not necessarily
    /// delivered). Returns [`NetworkError::PeerUnreachable`] if no connection
    /// exists and cannot be established.
    fn send_to(
        &self,
        peer: &PeerId,
        msg: Self::Message,
    ) -> impl Future<Output = Result<(), NetworkError>> + Send;

    /// Send `msg` to all connected peers (best-effort fan-out).
    fn broadcast(
        &self,
        msg: Self::Message,
    ) -> impl Future<Output = Result<(), NetworkError>> + Send;

    /// List all currently known peer IDs.
    fn known_peers(&self) -> Vec<PeerId>;

    /// Query the connection state of a specific peer.
    fn connection_state(&self, peer: &PeerId) -> ConnectionState;
}

// ── GossipNetwork [FROZEN-2] ─────────────────────────────────────────────────

/// Topic-based publish / subscribe over GossipSub 1.1.
///
/// Built-in peer scoring determines relay decisions. Messages are broadcast
/// with < 5% redundancy to all topic subscribers.
pub trait GossipNetwork: Send + Sync + Clone + 'static {
    /// Serializable message type.
    type Message: Send + Serialize + 'static;

    /// Subscribe to a topic. Subsequent messages on this topic will appear in
    /// the stream returned by [`Self::topic_stream`].
    fn subscribe(&self, topic: Topic) -> impl Future<Output = Result<(), NetworkError>> + Send;

    /// Unsubscribe from a topic.
    fn unsubscribe(&self, topic: Topic) -> impl Future<Output = Result<(), NetworkError>> + Send;

    /// Publish a message to all peers subscribed to `topic`.
    fn publish(
        &self,
        topic: Topic,
        msg: Self::Message,
    ) -> impl Future<Output = Result<(), NetworkError>> + Send;
}

// ── DhtDiscovery [FROZEN-2] ──────────────────────────────────────────────────

/// S/Kademlia DHT peer discovery with disjoint-path lookup.
///
/// Uses stake-weighted routing validation and 2-path disjoint lookup for
/// Eclipse attack mitigation.
pub trait DhtDiscovery: Send + Sync + Clone + 'static {
    /// Node record type (address, public key, metadata).
    type NodeRecord: Send + Sync + Clone + Serialize + 'static;

    /// Publish this node's record to the DHT.
    fn publish_self(
        &self,
        record: Self::NodeRecord,
    ) -> impl Future<Output = Result<(), NetworkError>> + Send;

    /// Find up to `count` peers closest to `target` in the key space.
    fn find_peers(
        &self,
        target: &PeerId,
        count: usize,
    ) -> impl Future<Output = Result<Vec<Self::NodeRecord>, NetworkError>> + Send;

    /// Bootstrap the DHT by connecting to known boot nodes.
    fn bootstrap(
        &self,
        boot_nodes: &[Self::NodeRecord],
    ) -> impl Future<Output = Result<(), NetworkError>> + Send;

    /// Snapshot of the routing table health.
    fn routing_health(&self) -> RoutingHealth;
}
