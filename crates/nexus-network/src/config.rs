//! Network service configuration.
//!
//! [`NetworkConfig`] captures all tuneable parameters for the P2P layer.
//! Defaults are production-ready; override in test environments.

use serde::{Deserialize, Serialize};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::PathBuf;

/// Configuration for the Nexus P2P network service.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NetworkConfig {
    /// Local socket address to bind. Default: `0.0.0.0:9100`.
    pub listen_addr: SocketAddr,

    /// Bootstrap peer addresses for initial DHT population.
    pub boot_nodes: Vec<String>,

    /// Maximum number of simultaneous peer connections.
    pub max_peers: usize,

    /// Path to the persistent Ed25519 identity key file.
    ///
    /// If set, the keypair is loaded from this file on startup (or generated
    /// and saved on first run). If `None`, an ephemeral keypair is generated
    /// every time — suitable for tests but not production.
    pub identity_key_path: Option<PathBuf>,

    // ── GossipSub Parameters ──────────────────────────────────────────────
    /// GossipSub mesh target size (D parameter).
    pub gossip_mesh_size: usize,

    /// GossipSub mesh lower bound (D_lo). Below this, the node seeks peers.
    pub gossip_mesh_lo: usize,

    /// GossipSub mesh upper bound (D_hi). Above this, the node prunes peers.
    pub gossip_mesh_hi: usize,

    // ── Kademlia / DHT ────────────────────────────────────────────────────
    /// Kademlia replication factor (K value).
    pub kademlia_replication: usize,

    /// Number of disjoint lookup paths for S/Kademlia Eclipse mitigation.
    pub disjoint_lookup_paths: usize,

    // ── Rate Limiting ─────────────────────────────────────────────────────
    /// Maximum messages per second from a single peer.
    pub rate_limit_per_peer_rps: u32,

    // ── Timeouts ──────────────────────────────────────────────────────────
    /// Idle connection timeout in milliseconds. Connections with no activity
    /// beyond this duration are closed.
    pub connection_idle_timeout_ms: u64,

    /// Connection attempt timeout in milliseconds.
    pub connection_timeout_ms: u64,

    /// DHT closest-peers lookup timeout in milliseconds.
    /// Queries that exceed this duration are cancelled.  Default: 30 s.
    pub dht_lookup_timeout_ms: u64,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            listen_addr: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 9100)),
            boot_nodes: Vec::new(),
            max_peers: 200,
            identity_key_path: None,
            gossip_mesh_size: 8,
            gossip_mesh_lo: 6,
            gossip_mesh_hi: 12,
            kademlia_replication: 20,
            disjoint_lookup_paths: 2,
            rate_limit_per_peer_rps: 100,
            connection_idle_timeout_ms: 5 * 60 * 1000, // 5 minutes
            connection_timeout_ms: 10_000,             // 10 seconds
            dht_lookup_timeout_ms: 30_000,             // 30 seconds
        }
    }
}

impl NetworkConfig {
    /// Create a minimal configuration suitable for tests.
    ///
    /// Uses `127.0.0.1:0` (OS-assigned port), no boot nodes, small peer limits.
    pub fn for_testing() -> Self {
        Self {
            listen_addr: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)),
            boot_nodes: Vec::new(),
            max_peers: 8,
            identity_key_path: None,
            gossip_mesh_size: 3,
            gossip_mesh_lo: 2,
            gossip_mesh_hi: 4,
            kademlia_replication: 3,
            disjoint_lookup_paths: 1,
            rate_limit_per_peer_rps: 1000,
            connection_idle_timeout_ms: 30_000,
            connection_timeout_ms: 5_000,
            dht_lookup_timeout_ms: 5_000,
        }
    }

    /// Validate the configuration at runtime.
    ///
    /// Checks structural invariants that GossipSub and the transport layer
    /// depend on. Returns descriptive errors for every violated constraint.
    pub fn validate(&self) -> Result<(), crate::error::NetworkError> {
        // GossipSub mesh invariant: D_lo < D < D_hi
        if self.gossip_mesh_lo >= self.gossip_mesh_size {
            return Err(crate::error::NetworkError::InvalidMessage {
                reason: format!(
                    "gossip_mesh_lo ({}) must be < gossip_mesh_size ({})",
                    self.gossip_mesh_lo, self.gossip_mesh_size
                ),
            });
        }
        if self.gossip_mesh_size >= self.gossip_mesh_hi {
            return Err(crate::error::NetworkError::InvalidMessage {
                reason: format!(
                    "gossip_mesh_size ({}) must be < gossip_mesh_hi ({})",
                    self.gossip_mesh_size, self.gossip_mesh_hi
                ),
            });
        }
        // mesh_outbound_min = D_lo / 2 must be > 0
        if self.gossip_mesh_lo < 2 {
            return Err(crate::error::NetworkError::InvalidMessage {
                reason: format!(
                    "gossip_mesh_lo ({}) must be >= 2 for valid mesh_outbound_min",
                    self.gossip_mesh_lo
                ),
            });
        }
        if self.max_peers == 0 {
            return Err(crate::error::NetworkError::InvalidMessage {
                reason: "max_peers must be > 0".into(),
            });
        }
        if self.rate_limit_per_peer_rps == 0 {
            return Err(crate::error::NetworkError::InvalidMessage {
                reason: "rate_limit_per_peer_rps must be > 0".into(),
            });
        }
        if self.connection_timeout_ms == 0 {
            return Err(crate::error::NetworkError::InvalidMessage {
                reason: "connection_timeout_ms must be > 0".into(),
            });
        }
        if self.disjoint_lookup_paths == 0 {
            return Err(crate::error::NetworkError::InvalidMessage {
                reason: "disjoint_lookup_paths must be >= 1".into(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_values() {
        let cfg = NetworkConfig::default();
        assert_eq!(cfg.max_peers, 200);
        assert_eq!(cfg.gossip_mesh_size, 8);
        assert_eq!(cfg.gossip_mesh_lo, 6);
        assert_eq!(cfg.gossip_mesh_hi, 12);
        assert_eq!(cfg.kademlia_replication, 20);
        assert_eq!(cfg.disjoint_lookup_paths, 2);
        assert_eq!(cfg.rate_limit_per_peer_rps, 100);
    }

    #[test]
    fn test_config_is_minimal() {
        let cfg = NetworkConfig::for_testing();
        assert!(cfg.max_peers < 20);
        assert!(cfg.boot_nodes.is_empty());
        assert_eq!(
            cfg.listen_addr.port(),
            0,
            "test config should use OS-assigned port"
        );
    }

    #[test]
    fn gossip_mesh_invariant() {
        let cfg = NetworkConfig::default();
        assert!(cfg.gossip_mesh_lo < cfg.gossip_mesh_size);
        assert!(cfg.gossip_mesh_size < cfg.gossip_mesh_hi);
    }

    #[test]
    fn config_serialization_roundtrip() {
        let cfg = NetworkConfig::default();
        let json = serde_json::to_string(&cfg).unwrap();
        let restored: NetworkConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.max_peers, cfg.max_peers);
        assert_eq!(restored.gossip_mesh_size, cfg.gossip_mesh_size);
    }

    #[test]
    fn default_config_validates() {
        let cfg = NetworkConfig::default();
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn test_config_validates() {
        let cfg = NetworkConfig::for_testing();
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn invalid_mesh_lo_ge_size() {
        let mut cfg = NetworkConfig::for_testing();
        cfg.gossip_mesh_lo = cfg.gossip_mesh_size; // violates lo < D
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn invalid_mesh_size_ge_hi() {
        let mut cfg = NetworkConfig::for_testing();
        cfg.gossip_mesh_hi = cfg.gossip_mesh_size; // violates D < hi
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn invalid_zero_max_peers() {
        let mut cfg = NetworkConfig::for_testing();
        cfg.max_peers = 0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn invalid_zero_rate_limit() {
        let mut cfg = NetworkConfig::for_testing();
        cfg.rate_limit_per_peer_rps = 0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn invalid_zero_timeout() {
        let mut cfg = NetworkConfig::for_testing();
        cfg.connection_timeout_ms = 0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn invalid_zero_disjoint_paths() {
        let mut cfg = NetworkConfig::for_testing();
        cfg.disjoint_lookup_paths = 0;
        assert!(cfg.validate().is_err());
    }
}
