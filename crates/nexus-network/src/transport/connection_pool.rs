//! Connection pool — tracks peer connection states and handles lifecycle.
//!
//! Uses [`DashMap`] for concurrent access from multiple tasks.

use std::time::Instant;

use dashmap::DashMap;
use libp2p::Multiaddr;
use tracing::debug;

use crate::types::{ConnectionState, PeerId};

// ── Peer State ───────────────────────────────────────────────────────────────

/// Internal tracked state for a connected or recently-seen peer.
#[derive(Debug, Clone)]
pub(crate) struct PeerState {
    /// Current connection state.
    pub state: ConnectionState,
    /// Remote address (if known).
    #[allow(dead_code)] // used by NetworkService diagnostics in T-6009
    pub addr: Option<Multiaddr>,
    /// When this peer was first seen.
    #[allow(dead_code)] // used by peer age tracking in T-6008
    pub first_seen: Instant,
    /// When the last message was received from this peer.
    pub last_active: Instant,
}

// ── ConnectionPool ───────────────────────────────────────────────────────────

/// Thread-safe connection pool tracking all known peers.
pub struct ConnectionPool {
    peers: DashMap<PeerId, PeerState>,
    max_peers: usize,
    idle_timeout_ms: u64,
}

impl ConnectionPool {
    /// Create a new pool with the given capacity and idle timeout.
    pub fn new(max_peers: usize, idle_timeout_ms: u64) -> Self {
        Self {
            peers: DashMap::with_capacity(max_peers),
            max_peers,
            idle_timeout_ms,
        }
    }

    /// Record a new connection.
    pub fn on_connected(&self, peer: PeerId, addr: Multiaddr) {
        let now = Instant::now();
        let state = PeerState {
            state: ConnectionState::Connected { latency_ms: 0 },
            addr: Some(addr),
            first_seen: now,
            last_active: now,
        };
        self.peers.insert(peer, state);
        debug!(?peer, total = self.peers.len(), "peer connected");
    }

    /// Record a disconnection.
    pub fn on_disconnected(&self, peer: &PeerId) {
        if let Some(mut entry) = self.peers.get_mut(peer) {
            entry.state = ConnectionState::Disconnected;
        }
        debug!(?peer, "peer disconnected");
    }

    /// Check if a peer is currently connected.
    pub fn is_connected(&self, peer: &PeerId) -> bool {
        self.peers
            .get(peer)
            .map(|e| e.state.is_connected())
            .unwrap_or(false)
    }

    /// Get the connection state of a peer.
    pub fn connection_state(&self, peer: &PeerId) -> ConnectionState {
        self.peers
            .get(peer)
            .map(|e| e.state.clone())
            .unwrap_or(ConnectionState::Disconnected)
    }

    /// List all peers that are currently connected.
    pub fn known_peers(&self) -> Vec<PeerId> {
        self.peers
            .iter()
            .filter(|e| e.state.is_connected())
            .map(|e| *e.key())
            .collect()
    }

    /// Number of currently connected peers.
    pub fn connected_count(&self) -> usize {
        self.peers.iter().filter(|e| e.state.is_connected()).count()
    }

    /// Total tracked peers (including disconnected).
    pub fn total_count(&self) -> usize {
        self.peers.len()
    }

    /// Whether we can accept a new connection.
    pub fn has_capacity(&self) -> bool {
        self.connected_count() < self.max_peers
    }

    /// Update the last-active timestamp for a peer.
    pub fn touch(&self, peer: &PeerId) {
        if let Some(mut entry) = self.peers.get_mut(peer) {
            entry.last_active = Instant::now();
        }
    }

    /// Update latency measurement for a connected peer.
    pub fn update_latency(&self, peer: &PeerId, latency_ms: u32) {
        if let Some(mut entry) = self.peers.get_mut(peer) {
            entry.state = ConnectionState::Connected { latency_ms };
        }
    }

    /// Ban a peer for the given duration.
    pub fn ban(&self, peer: &PeerId, duration: std::time::Duration) {
        let until = Instant::now() + duration;
        if let Some(mut entry) = self.peers.get_mut(peer) {
            entry.state = ConnectionState::Banned { until };
        } else {
            self.peers.insert(
                *peer,
                PeerState {
                    state: ConnectionState::Banned { until },
                    addr: None,
                    first_seen: Instant::now(),
                    last_active: Instant::now(),
                },
            );
        }
    }

    /// Remove peers that have been idle beyond the timeout threshold.
    pub fn evict_idle(&self) -> usize {
        let now = Instant::now();
        let timeout = std::time::Duration::from_millis(self.idle_timeout_ms);
        let mut evicted = 0;

        self.peers.retain(|_, state| {
            let keep = match &state.state {
                ConnectionState::Connected { .. } => {
                    now.duration_since(state.last_active) < timeout
                }
                ConnectionState::Banned { until } => now < *until,
                ConnectionState::Disconnected | ConnectionState::Connecting => false,
            };
            if !keep {
                evicted += 1;
            }
            keep
        });

        if evicted > 0 {
            debug!(evicted, remaining = self.peers.len(), "idle peers evicted");
        }
        evicted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::PeerId;
    use nexus_crypto::Blake3Hasher;

    fn make_peer(seed: u8) -> PeerId {
        let digest = Blake3Hasher::digest(b"test", &[seed]);
        PeerId::from_digest(digest)
    }

    fn make_addr() -> Multiaddr {
        "/ip4/127.0.0.1/udp/9100/quic-v1".parse().unwrap()
    }

    #[test]
    fn new_pool_is_empty() {
        let pool = ConnectionPool::new(10, 30_000);
        assert_eq!(pool.total_count(), 0);
        assert_eq!(pool.connected_count(), 0);
        assert!(pool.has_capacity());
    }

    #[test]
    fn connect_and_disconnect() {
        let pool = ConnectionPool::new(10, 30_000);
        let peer = make_peer(1);

        pool.on_connected(peer, make_addr());
        assert!(pool.is_connected(&peer));
        assert_eq!(pool.connected_count(), 1);

        pool.on_disconnected(&peer);
        assert!(!pool.is_connected(&peer));
    }

    #[test]
    fn known_peers_only_returns_connected() {
        let pool = ConnectionPool::new(10, 30_000);
        let p1 = make_peer(1);
        let p2 = make_peer(2);

        pool.on_connected(p1, make_addr());
        pool.on_connected(p2, make_addr());
        pool.on_disconnected(&p1);

        let known = pool.known_peers();
        assert_eq!(known.len(), 1);
        assert_eq!(known[0], p2);
    }

    #[test]
    fn capacity_check() {
        let pool = ConnectionPool::new(2, 30_000);
        assert!(pool.has_capacity());

        pool.on_connected(make_peer(1), make_addr());
        assert!(pool.has_capacity());

        pool.on_connected(make_peer(2), make_addr());
        assert!(!pool.has_capacity());
    }

    #[test]
    fn update_latency() {
        let pool = ConnectionPool::new(10, 30_000);
        let peer = make_peer(1);
        pool.on_connected(peer, make_addr());

        pool.update_latency(&peer, 42);
        match pool.connection_state(&peer) {
            ConnectionState::Connected { latency_ms } => assert_eq!(latency_ms, 42),
            other => panic!("expected Connected, got {:?}", other),
        }
    }

    #[test]
    fn ban_peer() {
        let pool = ConnectionPool::new(10, 30_000);
        let peer = make_peer(1);
        pool.on_connected(peer, make_addr());

        pool.ban(&peer, std::time::Duration::from_secs(60));
        assert!(!pool.is_connected(&peer));
        match pool.connection_state(&peer) {
            ConnectionState::Banned { .. } => {}
            other => panic!("expected Banned, got {:?}", other),
        }
    }

    #[test]
    fn evict_idle_removes_disconnected() {
        let pool = ConnectionPool::new(10, 30_000);
        let peer = make_peer(1);
        pool.on_connected(peer, make_addr());
        pool.on_disconnected(&peer);

        let evicted = pool.evict_idle();
        assert_eq!(evicted, 1);
        assert_eq!(pool.total_count(), 0);
    }

    #[test]
    fn disconnected_peer_returns_disconnected_state() {
        let pool = ConnectionPool::new(10, 30_000);
        let peer = make_peer(99);
        assert!(matches!(
            pool.connection_state(&peer),
            ConnectionState::Disconnected
        ));
    }

    #[test]
    fn touch_updates_last_active() {
        let pool = ConnectionPool::new(10, 30_000);
        let peer = make_peer(1);
        pool.on_connected(peer, make_addr());

        // Capture initial last_active
        let initial = pool.peers.get(&peer).unwrap().last_active;

        // Small sleep to ensure "now" differs
        std::thread::sleep(std::time::Duration::from_millis(10));
        pool.touch(&peer);

        let updated = pool.peers.get(&peer).unwrap().last_active;
        assert!(
            updated > initial,
            "touch() should advance last_active timestamp"
        );
    }

    #[test]
    fn touch_on_unknown_peer_is_noop() {
        let pool = ConnectionPool::new(10, 30_000);
        let peer = make_peer(99);
        pool.touch(&peer); // Should not panic
        assert_eq!(pool.total_count(), 0);
    }
}
