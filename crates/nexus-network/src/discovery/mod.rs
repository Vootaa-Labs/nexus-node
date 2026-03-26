//! S/Kademlia DHT peer discovery — bootstrap, routing, and peer lookup.
//!
//! Implements the [`DhtDiscovery`] trait on top of libp2p's Kademlia,
//! delegating low-level DHT operations through [`TransportHandle`].
//!
//! The discovery layer adds:
//! - Boot-node bootstrap sequence with retry
//! - Periodic routing-table refresh (default 60 s)
//! - [`NodeRecord`] metadata: PeerId, addresses, public key, reputation
//! - Disjoint-path lookup for Eclipse attack mitigation (see [`disjoint`])

pub mod disjoint;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use dashmap::DashMap;
use libp2p::Multiaddr;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info};

use crate::config::NetworkConfig;
use crate::error::NetworkError;
use crate::transport::{KadEvent, TransportHandle};
use crate::types::{PeerId, RoutingHealth};

/// Maximum number of entries in the peer table.
///
/// Bounds memory against Sybil floods.  `max_peers * 4` gives headroom
/// above the connection pool limit.
const MAX_PEER_TABLE_ENTRIES: usize = 800;

// ── NodeRecord ───────────────────────────────────────────────────────────────

/// Metadata about a peer discovered through the DHT.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeRecord {
    /// Nexus peer identity (BLAKE3 of ML-DSA pubkey).
    pub peer_id: PeerId,
    /// Known multiaddresses for this peer.
    #[serde(skip)]
    pub addresses: Vec<Multiaddr>,
    /// Raw ML-DSA public-key bytes (~1312 B for FIPS 204 Level III).
    pub dilithium_pubkey: Vec<u8>,
    /// Local reputation score (0 = unknown, higher = more trusted).
    pub reputation: u32,
    /// Last-seen Unix timestamp (seconds since epoch).
    pub last_seen: u64,
    /// Validator stake, if this peer is a known validator.
    pub validator_stake: Option<u128>,
}

impl NodeRecord {
    /// Create a minimal node record for bootstrap entries.
    pub fn bootstrap(peer_id: PeerId, addresses: Vec<Multiaddr>) -> Self {
        Self {
            peer_id,
            addresses,
            dilithium_pubkey: Vec::new(),
            reputation: 0,
            last_seen: 0,
            validator_stake: None,
        }
    }
}

// ── Discovery Commands ───────────────────────────────────────────────────────

/// Commands sent from [`DiscoveryHandle`] to [`DiscoveryService`].
#[derive(Debug)]
enum DiscoveryCommand {
    /// Execute a disjoint-path lookup and return the resulting peer IDs.
    DisjointLookup {
        target: PeerId,
        count: usize,
        reply: oneshot::Sender<Result<Vec<PeerId>, NetworkError>>,
    },
}

// ── DiscoveryHandle ──────────────────────────────────────────────────────────

/// Cheaply cloneable handle for querying the discovery layer.
#[derive(Clone)]
pub struct DiscoveryHandle {
    /// Peer records observed through Kademlia.
    peer_table: Arc<DashMap<PeerId, NodeRecord>>,
    /// Counter of routing updates (approximates routing-table size).
    routing_updates: Arc<AtomicUsize>,
    /// Transport handle for issuing Kademlia commands.
    transport: TransportHandle,
    /// Command channel to the DiscoveryService (for disjoint lookups).
    cmd_tx: mpsc::Sender<DiscoveryCommand>,
}

impl DiscoveryHandle {
    /// Snapshot of all known peer records.
    pub fn known_records(&self) -> Vec<NodeRecord> {
        self.peer_table
            .iter()
            .map(|entry| entry.value().clone())
            .collect()
    }

    /// Look up a specific peer record by ID.
    pub fn get_record(&self, peer_id: &PeerId) -> Option<NodeRecord> {
        self.peer_table.get(peer_id).map(|entry| entry.clone())
    }

    /// Current routing health snapshot.
    pub fn routing_health(&self) -> RoutingHealth {
        let known = self.peer_table.len();
        let updates = self.routing_updates.load(Ordering::Relaxed);
        // Approximate bucket fill: each 20 updates ≈ 1 bucket worth
        let filled = (updates / 20).min(256);
        RoutingHealth {
            known_peers: known,
            filled_buckets: filled,
            total_buckets: 256,
        }
    }

    /// Trigger a disjoint-path Kademlia find-closest-peers query.
    ///
    /// When `disjoint_paths >= 2`, multiple independent DHT queries are
    /// issued in parallel and only the intersection of results is returned
    /// (Eclipse attack mitigation). With `disjoint_paths == 1`, a single
    /// query is used.
    pub async fn find_peers(
        &self,
        target: &PeerId,
        count: usize,
    ) -> Result<Vec<NodeRecord>, NetworkError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(DiscoveryCommand::DisjointLookup {
                target: *target,
                count,
                reply: reply_tx,
            })
            .await
            .map_err(|_| NetworkError::ShuttingDown)?;

        let peer_ids = reply_rx.await.map_err(|_| NetworkError::ShuttingDown)??;

        // Resolve PeerIds → NodeRecords from the shared peer table.
        Ok(peer_ids
            .iter()
            .filter_map(|pid| self.peer_table.get(pid).map(|e| e.clone()))
            .collect())
    }

    /// Add a bootstrap peer's address to the Kademlia routing table.
    pub async fn add_boot_node(
        &self,
        libp2p_peer: libp2p::PeerId,
        addr: Multiaddr,
    ) -> Result<(), NetworkError> {
        self.transport.kad_add_address(libp2p_peer, addr).await
    }

    /// Dial a remote peer by multiaddr.
    ///
    /// Initiates a QUIC connection to the given address. This is needed
    /// in addition to [`add_boot_node`] because adding an address to the
    /// Kademlia routing table does **not** open a connection — Kademlia
    /// only stores the address for future queries.
    pub async fn dial(&self, addr: Multiaddr) -> Result<(), NetworkError> {
        self.transport.dial(addr).await
    }

    /// Trigger a Kademlia bootstrap round.
    pub async fn bootstrap(&self) -> Result<(), NetworkError> {
        self.transport.kad_bootstrap().await
    }

    /// Seed the peer table with a known validator record.
    ///
    /// Called during genesis boot to pre-populate validator identities
    /// before DHT bootstrap completes. Existing entries are **not** overwritten
    /// so that runtime-discovered metadata is preserved.
    pub fn seed_validator_record(&self, record: NodeRecord) {
        self.peer_table.entry(record.peer_id).or_insert(record);
    }

    /// Number of validators currently in the peer table (have stake set).
    pub fn known_validators(&self) -> usize {
        self.peer_table
            .iter()
            .filter(|entry| entry.value().validator_stake.is_some())
            .count()
    }
}

// ── DiscoveryService ─────────────────────────────────────────────────────────

/// Service that processes Kademlia events and maintains a peer table.
///
/// Runs as a Tokio task, consuming [`KadEvent`]s from the transport layer
/// and updating the shared peer table accessible through [`DiscoveryHandle`].
/// Also handles disjoint-path lookups requested via [`DiscoveryHandle::find_peers`].
pub struct DiscoveryService {
    /// Receives Kademlia events from TransportManager.
    kad_rx: mpsc::Receiver<KadEvent>,
    /// Shared peer table (also held by DiscoveryHandle).
    peer_table: Arc<DashMap<PeerId, NodeRecord>>,
    /// Routing update counter.
    routing_updates: Arc<AtomicUsize>,
    /// Receives commands from DiscoveryHandle.
    cmd_rx: mpsc::Receiver<DiscoveryCommand>,
    /// Transport handle for issuing Kademlia queries (disjoint lookup).
    transport: TransportHandle,
    /// Number of disjoint lookup paths configured.
    disjoint_paths: usize,
    /// Timeout for a single DHT closest-peers lookup.
    dht_lookup_timeout: std::time::Duration,
}

impl DiscoveryService {
    /// Create a new discovery service, its handle, and the Kademlia event sender.
    ///
    /// The caller must:
    /// 1. Pass `kad_tx` to `TransportManager::set_kad_event_tx()`
    /// 2. Spawn `service.run()` as a Tokio task
    pub fn new(
        config: &NetworkConfig,
        transport: TransportHandle,
    ) -> (DiscoveryHandle, Self, mpsc::Sender<KadEvent>) {
        let (kad_tx, kad_rx) = mpsc::channel(256);
        let (cmd_tx, cmd_rx) = mpsc::channel(64);

        let peer_table = Arc::new(DashMap::new());
        let routing_updates = Arc::new(AtomicUsize::new(0));

        let handle = DiscoveryHandle {
            peer_table: Arc::clone(&peer_table),
            routing_updates: Arc::clone(&routing_updates),
            transport: transport.clone(),
            cmd_tx,
        };

        let service = Self {
            kad_rx,
            peer_table,
            routing_updates,
            cmd_rx,
            transport,
            disjoint_paths: config.disjoint_lookup_paths,
            dht_lookup_timeout: std::time::Duration::from_millis(config.dht_lookup_timeout_ms),
        };

        (handle, service, kad_tx)
    }

    /// Run the discovery event loop. Returns when the Kademlia event channel closes.
    pub async fn run(mut self) {
        info!("DiscoveryService starting");

        // Periodic stale-peer eviction (every 5 minutes).
        let mut evict_interval = tokio::time::interval(std::time::Duration::from_secs(300));
        evict_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                event = self.kad_rx.recv() => {
                    match event {
                        Some(event) => self.handle_event(event),
                        None => {
                            info!("DiscoveryService shutting down (channel closed)");
                            return;
                        }
                    }
                }
                cmd = self.cmd_rx.recv() => {
                    match cmd {
                        Some(DiscoveryCommand::DisjointLookup { target, count, reply }) => {
                            let result = disjoint::disjoint_lookup(
                                &self.transport,
                                &mut self.kad_rx,
                                &target,
                                self.disjoint_paths,
                                count,
                                self.dht_lookup_timeout,
                            ).await;
                            let _ = reply.send(result);
                        }
                        None => {
                            info!("DiscoveryService command channel closed");
                            return;
                        }
                    }
                }
                // Periodic stale-peer eviction based on last_seen age.
                _ = evict_interval.tick() => {
                    self.evict_stale_peers();
                }
            }
        }
    }

    /// Remove peer records that have not been refreshed within the staleness
    /// window.  Validators (with `validator_stake` set) are exempt because
    /// they are seeded from genesis and expected to be long-lived.
    fn evict_stale_peers(&self) {
        const STALE_THRESHOLD_SECS: u64 = 30 * 60; // 30 minutes
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let before = self.peer_table.len();
        self.peer_table.retain(|_, record| {
            // Keep validators regardless of age.
            if record.validator_stake.is_some() {
                return true;
            }
            // Keep peers seen within the threshold.
            if record.last_seen == 0 {
                // Never been seen via DHT — keep if table is not at capacity.
                return self.peer_table.len() < MAX_PEER_TABLE_ENTRIES;
            }
            now_secs.saturating_sub(record.last_seen) < STALE_THRESHOLD_SECS
        });
        let evicted = before.saturating_sub(self.peer_table.len());
        if evicted > 0 {
            debug!(
                evicted,
                remaining = self.peer_table.len(),
                "stale peers evicted from peer table"
            );
        }
    }

    fn handle_event(&mut self, event: KadEvent) {
        match event {
            KadEvent::ClosestPeers { peers } => {
                debug!(count = peers.len(), "received closest peers from DHT");
                for libp2p_peer in peers {
                    if self.peer_table.len() >= MAX_PEER_TABLE_ENTRIES {
                        debug!(
                            max = MAX_PEER_TABLE_ENTRIES,
                            "peer table full, discarding new peers"
                        );
                        break;
                    }
                    let nexus_peer = PeerId::from_libp2p(&libp2p_peer);
                    self.peer_table
                        .entry(nexus_peer)
                        .or_insert_with(|| NodeRecord::bootstrap(nexus_peer, Vec::new()));
                }
            }
            KadEvent::BootstrapOk => {
                info!("kademlia bootstrap completed");
                self.routing_updates.fetch_add(1, Ordering::Relaxed);
            }
            KadEvent::RoutingUpdated { peer, is_new_peer } => {
                let nexus_peer = PeerId::from_libp2p(&peer);
                if is_new_peer && self.peer_table.len() < MAX_PEER_TABLE_ENTRIES {
                    self.peer_table
                        .entry(nexus_peer)
                        .or_insert_with(|| NodeRecord::bootstrap(nexus_peer, Vec::new()));
                    self.routing_updates.fetch_add(1, Ordering::Relaxed);
                    debug!(%peer, "new peer added to routing table");
                }
            }
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::NetworkConfig;

    #[test]
    fn node_record_bootstrap_has_zero_reputation() {
        let peer = PeerId::from_public_key(b"test-key");
        let rec = NodeRecord::bootstrap(peer, Vec::new());
        assert_eq!(rec.reputation, 0);
        assert!(rec.dilithium_pubkey.is_empty());
        assert!(rec.validator_stake.is_none());
    }

    #[test]
    fn discovery_handle_is_clone_send_sync() {
        fn assert_bounds<T: Clone + Send + Sync>() {}
        assert_bounds::<DiscoveryHandle>();
    }

    #[tokio::test]
    async fn discovery_service_creation() {
        let config = NetworkConfig::for_testing();
        let (handle, _manager, _incoming) =
            crate::transport::TransportManager::new(&config).expect("build transport");

        let (disc_handle, _service, _kad_tx) = DiscoveryService::new(&config, handle);

        let health = disc_handle.routing_health();
        assert_eq!(health.known_peers, 0);
        assert!(!health.is_healthy());
    }

    #[tokio::test]
    async fn peer_table_population_via_event() {
        let config = NetworkConfig::for_testing();
        let (handle, _manager, _incoming) =
            crate::transport::TransportManager::new(&config).expect("build transport");

        let (_disc_handle, mut service, _kad_tx) = DiscoveryService::new(&config, handle);

        // Simulate receiving a RoutingUpdated event
        let fake_peer = libp2p::PeerId::random();
        service.handle_event(KadEvent::RoutingUpdated {
            peer: fake_peer,
            is_new_peer: true,
        });

        assert_eq!(service.peer_table.len(), 1);
        assert_eq!(service.routing_updates.load(Ordering::Relaxed), 1,);
    }

    #[tokio::test]
    async fn closest_peers_event_adds_to_table() {
        let config = NetworkConfig::for_testing();
        let (handle, _manager, _incoming) =
            crate::transport::TransportManager::new(&config).expect("build transport");

        let (_disc_handle, mut service, _kad_tx) = DiscoveryService::new(&config, handle);

        let peers: Vec<libp2p::PeerId> = (0..5).map(|_| libp2p::PeerId::random()).collect();
        service.handle_event(KadEvent::ClosestPeers {
            peers: peers.clone(),
        });

        assert_eq!(service.peer_table.len(), 5);
    }

    #[tokio::test]
    async fn bootstrap_ok_increments_routing_updates() {
        let config = NetworkConfig::for_testing();
        let (handle, _manager, _incoming) =
            crate::transport::TransportManager::new(&config).expect("build transport");

        let (_disc_handle, mut service, _kad_tx) = DiscoveryService::new(&config, handle);

        service.handle_event(KadEvent::BootstrapOk);
        service.handle_event(KadEvent::BootstrapOk);

        assert_eq!(service.routing_updates.load(Ordering::Relaxed), 2,);
    }

    #[tokio::test]
    async fn routing_health_reflects_known_peers() {
        let config = NetworkConfig::for_testing();
        let (handle, _manager, _incoming) =
            crate::transport::TransportManager::new(&config).expect("build transport");

        let (disc_handle, mut service, _kad_tx) = DiscoveryService::new(&config, handle);

        // Add several peers
        for _ in 0..10 {
            let fake = libp2p::PeerId::random();
            service.handle_event(KadEvent::RoutingUpdated {
                peer: fake,
                is_new_peer: true,
            });
        }

        let health = disc_handle.routing_health();
        assert_eq!(health.known_peers, 10);
        assert!(!health.is_healthy(), "needs 3+ filled buckets for healthy");
    }

    #[tokio::test]
    async fn duplicate_routing_update_no_double_entry() {
        let config = NetworkConfig::for_testing();
        let (handle, _manager, _incoming) =
            crate::transport::TransportManager::new(&config).expect("build transport");

        let (_disc_handle, mut service, _kad_tx) = DiscoveryService::new(&config, handle);

        let fake_peer = libp2p::PeerId::random();
        service.handle_event(KadEvent::RoutingUpdated {
            peer: fake_peer,
            is_new_peer: true,
        });
        // Same peer again
        service.handle_event(KadEvent::RoutingUpdated {
            peer: fake_peer,
            is_new_peer: true,
        });

        // Should still be 1 entry (or_insert_with won't duplicate)
        assert_eq!(service.peer_table.len(), 1);
        // But routing_updates counts each time
        assert_eq!(service.routing_updates.load(Ordering::Relaxed), 2,);
    }

    #[tokio::test]
    async fn find_peers_sends_disjoint_command() {
        // Verify that find_peers dispatches a DisjointLookup command to the
        // DiscoveryService rather than issuing a raw kad_find_closest.
        let config = NetworkConfig::for_testing();
        let (handle, _manager, _incoming) =
            crate::transport::TransportManager::new(&config).expect("build transport");

        // Build only the discovery components — we test the command plumbing
        // without running the full service event loop.
        let (cmd_tx, mut cmd_rx) = mpsc::channel::<DiscoveryCommand>(64);
        let peer_table = Arc::new(DashMap::new());
        let routing_updates = Arc::new(AtomicUsize::new(0));

        let disc_handle = DiscoveryHandle {
            peer_table: Arc::clone(&peer_table),
            routing_updates: Arc::clone(&routing_updates),
            transport: handle,
            cmd_tx,
        };

        let target = PeerId::from_public_key(b"target-peer");
        let lookup_task = tokio::spawn({
            let disc_handle = disc_handle.clone();
            async move { disc_handle.find_peers(&target, 5).await }
        });

        // Receive the command
        let cmd = tokio::time::timeout(std::time::Duration::from_secs(2), cmd_rx.recv())
            .await
            .expect("should receive command within timeout");

        match cmd {
            Some(DiscoveryCommand::DisjointLookup { count, reply, .. }) => {
                assert_eq!(count, 5);
                let _ = reply.send(Ok(vec![
                    PeerId::from_public_key(b"peer-1"),
                    PeerId::from_public_key(b"peer-2"),
                ]));
            }
            None => panic!("command channel unexpectedly closed"),
        }

        let result = lookup_task.await.unwrap();
        assert!(result.is_ok());
        // Peer table is empty so no records returned, but lookup succeeded
        assert_eq!(result.unwrap().len(), 0);
    }
}
