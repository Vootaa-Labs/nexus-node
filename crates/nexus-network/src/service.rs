//! Unified network service — lifecycle manager for all P2P subsystems.
//!
//! [`NetworkService`] owns and wires together:
//! - [`TransportManager`] — QUIC/libp2p swarm event loop (includes gossip command processing)
//! - [`DiscoveryService`] — S/Kademlia DHT peer discovery
//! - [`PeerRateLimiter`] — per-peer, per-topic token bucket
//!
//! The [`GossipService`] is owned by `TransportManager` and processes commands
//! inline during the swarm event loop (it needs direct access to the gossipsub
//! behaviour). External code interacts solely through [`NetworkServiceHandle`].

use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::info;

use crate::config::NetworkConfig;
use crate::discovery::{DiscoveryHandle, DiscoveryService};
use crate::error::NetworkResult;
use crate::gossip::{GossipHandle, GossipService};
use crate::rate_limit::PeerRateLimiter;
use crate::transport::{TransportHandle, TransportManager};
use crate::types::PeerId;

// ── NetworkServiceHandle ─────────────────────────────────────────────────────

/// Cheaply cloneable handle providing access to all network subsystems.
///
/// Distributed to consensus, execution, and intent layers.
#[derive(Clone)]
pub struct NetworkServiceHandle {
    /// Transport (send/receive raw bytes, dial peers).
    pub transport: TransportHandle,
    /// GossipSub (subscribe/publish on topics).
    pub gossip: GossipHandle,
    /// Discovery (peer lookup, routing health).
    pub discovery: DiscoveryHandle,
    /// Rate limiter (check before forwarding messages).
    pub rate_limiter: Arc<PeerRateLimiter>,
}

impl NetworkServiceHandle {
    /// Convenience: check the rate limiter for a peer+topic.
    pub fn is_rate_allowed(&self, peer: &PeerId, topic: crate::types::Topic) -> bool {
        self.rate_limiter.check(peer, topic)
    }
}

// ── NetworkService ───────────────────────────────────────────────────────────

/// Owns all subsystem tasks and drives the network layer lifecycle.
///
/// # Usage
/// ```ignore
/// let (handle, service) = NetworkService::build(&config)?;
/// service.run().await;
/// ```
pub struct NetworkService {
    transport_manager: TransportManager,
    discovery_service: DiscoveryService,
    #[allow(dead_code)] // consumed by upper layers in future phase
    incoming_rx: mpsc::Receiver<(PeerId, Vec<u8>)>,
}

impl NetworkService {
    /// Build all subsystems and return the service + a cloneable handle.
    pub fn build(config: &NetworkConfig) -> NetworkResult<(NetworkServiceHandle, Self)> {
        // 0. Validate config before anything else
        config.validate()?;

        // 1. Transport layer
        let (transport_handle, mut transport_manager, incoming_rx) = TransportManager::new(config)?;

        // 2. Gossip layer (commands processed inline by TransportManager)
        let (gossip_handle, gossip_service) = GossipService::new(config);
        transport_manager.set_gossip_service(gossip_service);

        // 3. Discovery layer — wires Kademlia events from transport
        let (discovery_handle, discovery_service, kad_tx) =
            DiscoveryService::new(config, transport_handle.clone());
        transport_manager.set_kad_event_tx(kad_tx);

        // 4. Rate limiter
        let rate_limiter = Arc::new(PeerRateLimiter::new(config.rate_limit_per_peer_rps));

        let handle = NetworkServiceHandle {
            transport: transport_handle,
            gossip: gossip_handle,
            discovery: discovery_handle,
            rate_limiter,
        };

        let service = Self {
            transport_manager,
            discovery_service,
            incoming_rx,
        };

        Ok((handle, service))
    }

    /// Run all subsystem event loops as concurrent Tokio tasks.
    ///
    /// Consumes `self` and runs until shutdown signal via the transport handle.
    pub async fn run(self) {
        info!("NetworkService starting all subsystems");

        let Self {
            transport_manager,
            discovery_service,
            incoming_rx: _incoming_rx,
        } = self;

        // Transport + Discovery run as independent tasks
        let mut transport_task = tokio::spawn(transport_manager.run());
        let mut discovery_task = tokio::spawn(discovery_service.run());

        // Wait for any subsystem to finish (usually shutdown).
        // When one exits, abort the other so no orphaned tasks linger
        // (e.g. a pending QUIC dial that blocks runtime teardown).
        tokio::select! {
            _ = &mut transport_task => {
                info!("transport subsystem exited");
                discovery_task.abort();
            }
            _ = &mut discovery_task => {
                info!("discovery subsystem exited");
                transport_task.abort();
            }
        }

        info!("NetworkService stopped");
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::NetworkConfig;

    #[test]
    fn network_service_handle_is_clone_send_sync() {
        fn assert_bounds<T: Clone + Send + Sync>() {}
        assert_bounds::<NetworkServiceHandle>();
    }

    #[tokio::test]
    async fn build_produces_all_subsystems() {
        let config = NetworkConfig::for_testing();
        let (handle, _service) = NetworkService::build(&config).expect("should build");

        // Verify each sub-handle exists and works
        let health = handle.discovery.routing_health();
        assert_eq!(health.known_peers, 0);
        assert_eq!(handle.rate_limiter.active_buckets(), 0);
    }

    #[tokio::test]
    async fn rate_limiter_accessible_through_handle() {
        let config = NetworkConfig::for_testing();
        let (handle, _service) = NetworkService::build(&config).expect("should build");

        let peer = PeerId::from_public_key(b"test-peer");
        assert!(handle.is_rate_allowed(&peer, crate::types::Topic::Transaction));
    }

    #[tokio::test]
    async fn service_shuts_down_via_handle() {
        let config = NetworkConfig::for_testing();
        let (handle, service) = NetworkService::build(&config).expect("should build");

        let service_task = tokio::spawn(service.run());

        // Shutdown via transport handle
        handle.transport.shutdown().await.expect("shutdown ok");

        // Service should exit cleanly
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), service_task).await;
        assert!(result.is_ok(), "service should exit after shutdown");
    }
}
