// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Transport layer — QUIC-based peer-to-peer message delivery.
//!
//! Provides [`TransportManager`] which drives the libp2p `Swarm` event loop
//! and exposes point-to-point and broadcast messaging through an internal
//! command channel.

pub mod connection_pool;
pub mod quic;

use std::collections::HashMap;

use futures::StreamExt;
use libp2p::gossipsub;
use libp2p::swarm::SwarmEvent;
use libp2p::{Multiaddr, Swarm};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::config::NetworkConfig;
use crate::error::{NetworkError, NetworkResult};
use crate::gossip::GossipService;
use crate::metrics;
use crate::types::{ConnectionState, PeerId};

use self::connection_pool::ConnectionPool;
use self::quic::NexusBehaviour;

/// Events forwarded from Kademlia to the discovery layer.
#[derive(Debug)]
pub enum KadEvent {
    /// A closest-peers query completed with successful results.
    ClosestPeers {
        /// Peer IDs returned by the query.
        peers: Vec<libp2p::PeerId>,
    },
    /// A bootstrap round finished successfully.
    BootstrapOk,
    /// Routing table was updated: a peer was added or evicted.
    RoutingUpdated {
        /// The peer whose routing entry changed.
        peer: libp2p::PeerId,
        /// Whether the peer is now in the routing table (true) or was evicted (false).
        is_new_peer: bool,
    },
}

// ── Command channel messages ─────────────────────────────────────────────────

/// Commands sent from [`TransportHandle`] to the [`TransportManager`] event loop.
#[derive(Debug)]
pub(crate) enum TransportCommand {
    /// Send a raw message to a specific peer.
    SendTo {
        peer: PeerId,
        data: Vec<u8>,
        reply: oneshot::Sender<Result<(), NetworkError>>,
    },
    /// Broadcast a raw message to all connected peers.
    Broadcast {
        data: Vec<u8>,
        reply: oneshot::Sender<Result<(), NetworkError>>,
    },
    /// Query known peers.
    KnownPeers { reply: oneshot::Sender<Vec<PeerId>> },
    /// Query connection state.
    ConnectionState {
        peer: PeerId,
        reply: oneshot::Sender<ConnectionState>,
    },
    /// Dial a peer at a given multiaddr.
    Dial {
        addr: Multiaddr,
        reply: oneshot::Sender<Result<(), NetworkError>>,
    },
    /// Initiate a Kademlia bootstrap.
    KadBootstrap {
        reply: oneshot::Sender<Result<(), NetworkError>>,
    },
    /// Add a known address for a peer to the Kademlia routing table.
    KadAddAddress {
        peer: libp2p::PeerId,
        addr: Multiaddr,
    },
    /// Start a Kademlia `get_closest_peers` query.
    KadFindClosest {
        target: Vec<u8>,
        reply: oneshot::Sender<Result<(), NetworkError>>,
    },
    /// Shutdown the transport.
    Shutdown,
}

// ── TransportHandle (Clone-able sender) ──────────────────────────────────────

/// Cheaply cloneable handle for sending commands to the [`TransportManager`].
///
/// This is what upper layers hold to interact with the transport.
#[derive(Clone)]
pub struct TransportHandle {
    cmd_tx: mpsc::Sender<TransportCommand>,
}

impl TransportHandle {
    /// Send raw bytes to a specific peer.
    pub async fn send_to(&self, peer: &PeerId, data: Vec<u8>) -> NetworkResult<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(TransportCommand::SendTo {
                peer: *peer,
                data,
                reply: reply_tx,
            })
            .await
            .map_err(|_| NetworkError::ShuttingDown)?;
        reply_rx.await.map_err(|_| NetworkError::ShuttingDown)?
    }

    /// Broadcast raw bytes to all connected peers.
    pub async fn broadcast(&self, data: Vec<u8>) -> NetworkResult<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(TransportCommand::Broadcast {
                data,
                reply: reply_tx,
            })
            .await
            .map_err(|_| NetworkError::ShuttingDown)?;
        reply_rx.await.map_err(|_| NetworkError::ShuttingDown)?
    }

    /// List all known peer IDs.
    pub async fn known_peers(&self) -> NetworkResult<Vec<PeerId>> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(TransportCommand::KnownPeers { reply: reply_tx })
            .await
            .map_err(|_| NetworkError::ShuttingDown)?;
        reply_rx.await.map_err(|_| NetworkError::ShuttingDown)
    }

    /// Query connection state for a peer.
    pub async fn connection_state(&self, peer: &PeerId) -> NetworkResult<ConnectionState> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(TransportCommand::ConnectionState {
                peer: *peer,
                reply: reply_tx,
            })
            .await
            .map_err(|_| NetworkError::ShuttingDown)?;
        reply_rx.await.map_err(|_| NetworkError::ShuttingDown)
    }

    /// Dial a remote peer.
    pub async fn dial(&self, addr: Multiaddr) -> NetworkResult<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(TransportCommand::Dial {
                addr,
                reply: reply_tx,
            })
            .await
            .map_err(|_| NetworkError::ShuttingDown)?;
        reply_rx.await.map_err(|_| NetworkError::ShuttingDown)?
    }

    /// Send shutdown signal to the transport.
    pub async fn shutdown(&self) -> NetworkResult<()> {
        self.cmd_tx
            .send(TransportCommand::Shutdown)
            .await
            .map_err(|_| NetworkError::ShuttingDown)
    }

    /// Trigger a Kademlia bootstrap round.
    pub async fn kad_bootstrap(&self) -> NetworkResult<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(TransportCommand::KadBootstrap { reply: reply_tx })
            .await
            .map_err(|_| NetworkError::ShuttingDown)?;
        reply_rx.await.map_err(|_| NetworkError::ShuttingDown)?
    }

    /// Add a peer's address to the Kademlia routing table.
    pub async fn kad_add_address(
        &self,
        peer: libp2p::PeerId,
        addr: Multiaddr,
    ) -> NetworkResult<()> {
        self.cmd_tx
            .send(TransportCommand::KadAddAddress { peer, addr })
            .await
            .map_err(|_| NetworkError::ShuttingDown)
    }

    /// Start a Kademlia closest-peers query for the given key bytes.
    pub async fn kad_find_closest(&self, target: Vec<u8>) -> NetworkResult<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(TransportCommand::KadFindClosest {
                target,
                reply: reply_tx,
            })
            .await
            .map_err(|_| NetworkError::ShuttingDown)?;
        reply_rx.await.map_err(|_| NetworkError::ShuttingDown)?
    }
}

// ── TransportManager (event loop) ────────────────────────────────────────────

/// The transport manager owns the libp2p `Swarm` and drives its event loop.
///
/// Spawned as a Tokio task. Upper layers interact through [`TransportHandle`].
pub struct TransportManager {
    cmd_rx: mpsc::Receiver<TransportCommand>,
    swarm: Swarm<NexusBehaviour>,
    pool: ConnectionPool,
    /// Channel for delivering incoming messages to upper layers.
    incoming_tx: mpsc::Sender<(PeerId, Vec<u8>)>,
    /// Optional channel for Kademlia events → DiscoveryService.
    kad_event_tx: Option<mpsc::Sender<KadEvent>>,
    /// GossipService — processes commands and messages inline in the event loop.
    gossip_service: Option<GossipService>,
    /// Gossip command receiver — stored separately to enable select! without borrow conflicts.
    gossip_cmd_rx: Option<mpsc::Receiver<crate::gossip::GossipCommand>>,
    /// Reverse mapping: Nexus PeerId → libp2p PeerId (for request-response sends).
    peer_map: HashMap<PeerId, libp2p::PeerId>,
}

impl TransportManager {
    /// Create a new transport manager and its handle.
    ///
    /// Returns `(handle, manager, incoming_rx)`. The caller must spawn
    /// `manager.run()` as a Tokio task and consume `incoming_rx` for inbound messages.
    #[allow(clippy::type_complexity)]
    pub fn new(
        config: &NetworkConfig,
    ) -> NetworkResult<(TransportHandle, Self, mpsc::Receiver<(PeerId, Vec<u8>)>)> {
        let (cmd_tx, cmd_rx) = mpsc::channel(256);
        let (incoming_tx, incoming_rx) = mpsc::channel(1024);

        let mut swarm = quic::build_swarm(config)?;
        let pool = ConnectionPool::new(config.max_peers, config.connection_idle_timeout_ms);

        // Start listening on the configured address so the node can accept
        // inbound connections and Kademlia can advertise a reachable address.
        let listen_multiaddr: Multiaddr = format!(
            "/ip4/{}/udp/{}/quic-v1",
            config.listen_addr.ip(),
            config.listen_addr.port()
        )
        .parse()
        .map_err(|e| {
            NetworkError::Io(std::io::Error::other(format!(
                "invalid listen address: {e}"
            )))
        })?;
        swarm.listen_on(listen_multiaddr).map_err(|e| {
            NetworkError::Io(std::io::Error::other(format!("failed to listen: {e}")))
        })?;

        let handle = TransportHandle { cmd_tx };
        let manager = Self {
            cmd_rx,
            swarm,
            pool,
            incoming_tx,
            kad_event_tx: None,
            gossip_service: None,
            gossip_cmd_rx: None,
            peer_map: HashMap::new(),
        };

        Ok((handle, manager, incoming_rx))
    }

    /// Attach a Kademlia event sender for the discovery layer.
    pub fn set_kad_event_tx(&mut self, tx: mpsc::Sender<KadEvent>) {
        self.kad_event_tx = Some(tx);
    }

    /// Attach the GossipService for inline command and event processing.
    pub fn set_gossip_service(&mut self, mut gs: GossipService) {
        self.gossip_cmd_rx = gs.take_cmd_rx();
        self.gossip_service = Some(gs);
    }

    /// Run the event loop. This future never returns unless `Shutdown` is received.
    pub async fn run(mut self) {
        info!("TransportManager starting event loop");

        // Periodic idle-peer eviction (every 60 s).
        let mut evict_interval = tokio::time::interval(std::time::Duration::from_secs(60));
        evict_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            // Use a helper to optionally recv from gossip_cmd_rx without borrow issues
            let gossip_cmd = async {
                match &mut self.gossip_cmd_rx {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            };

            tokio::select! {
                // Transport commands
                cmd = self.cmd_rx.recv() => {
                    match cmd {
                        Some(TransportCommand::Shutdown) | None => {
                            info!("TransportManager shutting down");
                            return;
                        }
                        Some(cmd) => self.handle_command(cmd),
                    }
                }
                // Swarm events
                event = self.swarm.select_next_some() => {
                    self.handle_swarm_event(event).await;
                }
                // Gossip commands (subscribe, publish, unsubscribe)
                Some(cmd) = gossip_cmd => {
                    if let Some(gs) = &mut self.gossip_service {
                        gs.handle_gossip_command(
                            &mut self.swarm.behaviour_mut().gossipsub,
                            cmd,
                        );
                    }
                }
                // Periodic cleanup: evict idle / disconnected peers from pool
                _ = evict_interval.tick() => {
                    self.pool.evict_idle();
                }
            }
        }
    }

    fn handle_command(&mut self, cmd: TransportCommand) {
        match cmd {
            TransportCommand::SendTo { peer, data, reply } => {
                let result = self.do_send_to(&peer, data);
                let _ = reply.send(result);
            }
            TransportCommand::Broadcast { data, reply } => {
                let result = self.do_broadcast(data);
                let _ = reply.send(result);
            }
            TransportCommand::KnownPeers { reply } => {
                let peers = self.pool.known_peers();
                let _ = reply.send(peers);
            }
            TransportCommand::ConnectionState { peer, reply } => {
                let state = self.pool.connection_state(&peer);
                let _ = reply.send(state);
            }
            TransportCommand::Dial { addr, reply } => {
                let result = self.do_dial(addr);
                let _ = reply.send(result);
            }
            TransportCommand::KadBootstrap { reply } => {
                let result = self.do_kad_bootstrap();
                let _ = reply.send(result);
            }
            TransportCommand::KadAddAddress { peer, addr } => {
                self.swarm.behaviour_mut().kademlia.add_address(&peer, addr);
            }
            TransportCommand::KadFindClosest { target, reply } => {
                self.swarm
                    .behaviour_mut()
                    .kademlia
                    .get_closest_peers(target);
                let _ = reply.send(Ok(()));
            }
            TransportCommand::Shutdown => {
                // Handled in the select loop
            }
        }
    }

    fn do_send_to(&mut self, peer: &PeerId, data: Vec<u8>) -> Result<(), NetworkError> {
        if !self.pool.is_connected(peer) {
            return Err(NetworkError::PeerUnreachable {
                peer_id: format!("{}", peer),
            });
        }
        let libp2p_peer = self
            .peer_map
            .get(peer)
            .ok_or_else(|| NetworkError::PeerUnreachable {
                peer_id: format!("{}", peer),
            })?;
        self.swarm
            .behaviour_mut()
            .reqres
            .send_request(libp2p_peer, data);
        debug!(?peer, "queued send_to via request-response");
        Ok(())
    }

    fn do_broadcast(&mut self, data: Vec<u8>) -> Result<(), NetworkError> {
        let peers = self.pool.known_peers();
        if peers.is_empty() {
            debug!("broadcast: no connected peers");
        }
        for peer in &peers {
            if let Some(libp2p_peer) = self.peer_map.get(peer) {
                self.swarm
                    .behaviour_mut()
                    .reqres
                    .send_request(libp2p_peer, data.clone());
            }
        }
        debug!(
            peer_count = peers.len(),
            "broadcast queued via request-response"
        );
        Ok(())
    }

    fn do_dial(&mut self, addr: Multiaddr) -> Result<(), NetworkError> {
        self.swarm.dial(addr).map_err(|e| {
            NetworkError::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                e.to_string(),
            ))
        })?;
        Ok(())
    }

    fn do_kad_bootstrap(&mut self) -> Result<(), NetworkError> {
        self.swarm
            .behaviour_mut()
            .kademlia
            .bootstrap()
            .map(|_| ())
            .map_err(|e| NetworkError::DiscoveryError {
                reason: format!("kademlia bootstrap failed: {e}"),
            })
    }

    async fn handle_swarm_event(&mut self, event: SwarmEvent<quic::BehaviourEvent>) {
        match event {
            SwarmEvent::ConnectionEstablished {
                peer_id, endpoint, ..
            } => {
                let nexus_peer = PeerId::from_libp2p(&peer_id);

                // Reject inbound connections that exceed max_peers.
                if !self.pool.has_capacity() {
                    warn!(
                        %peer_id,
                        connected = self.pool.connected_count(),
                        "connection pool full — closing excess connection"
                    );
                    // Disconnect the peer; the ConnectionClosed event will
                    // clean up the pool entry if one was briefly inserted.
                    let _ = self.swarm.disconnect_peer_id(peer_id);
                    return;
                }

                self.pool
                    .on_connected(nexus_peer, endpoint.get_remote_address().clone());
                self.peer_map.insert(nexus_peer, peer_id);
                metrics::connection_established();
                info!(%peer_id, "connection established");
            }
            SwarmEvent::ConnectionClosed { peer_id, cause, .. } => {
                let nexus_peer = PeerId::from_libp2p(&peer_id);
                self.pool.on_disconnected(&nexus_peer);
                self.peer_map.remove(&nexus_peer);
                metrics::connection_closed();
                debug!(%peer_id, ?cause, "connection closed");
            }
            SwarmEvent::IncomingConnection { local_addr, .. } => {
                debug!(%local_addr, "incoming connection");
            }
            SwarmEvent::NewListenAddr { address, .. } => {
                info!(%address, "listening on");
            }
            SwarmEvent::Behaviour(bev) => {
                self.handle_behaviour_event(bev).await;
            }
            SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
                warn!(?peer_id, %error, "outgoing connection failed");
            }
            SwarmEvent::IncomingConnectionError {
                local_addr, error, ..
            } => {
                warn!(%local_addr, %error, "incoming connection failed");
            }
            SwarmEvent::ListenerError { error, .. } => {
                warn!(%error, "listener error");
            }
            _ => {}
        }
    }

    async fn handle_behaviour_event(&mut self, event: quic::BehaviourEvent) {
        match event {
            quic::NexusBehaviourEvent::Kademlia(kad_event) => {
                self.handle_kad_event(kad_event).await;
            }
            quic::NexusBehaviourEvent::Gossipsub(gossipsub::Event::Message {
                propagation_source,
                message,
                ..
            }) => {
                // Update last-active timestamp for the message sender
                let source_peer = PeerId::from_libp2p(&propagation_source);
                self.pool.touch(&source_peer);

                // Record received bytes for bandwidth metrics
                metrics::bytes_received(message.data.len() as u64);

                if let Some(gs) = &mut self.gossip_service {
                    gs.on_message(&message);
                }
            }
            quic::NexusBehaviourEvent::Reqres(libp2p::request_response::Event::Message {
                peer,
                message,
            }) => {
                self.handle_reqres_message(peer, message).await;
            }
            quic::NexusBehaviourEvent::Reqres(
                libp2p::request_response::Event::OutboundFailure { peer, error, .. },
            ) => {
                warn!(%peer, %error, "request-response outbound failure");
            }
            quic::NexusBehaviourEvent::Reqres(
                libp2p::request_response::Event::InboundFailure { peer, error, .. },
            ) => {
                debug!(%peer, %error, "request-response inbound failure");
            }
            quic::NexusBehaviourEvent::Reqres(libp2p::request_response::Event::ResponseSent {
                peer,
                ..
            }) => {
                debug!(%peer, "request-response response sent");
            }
            _ => {}
        }
    }

    async fn handle_reqres_message(
        &mut self,
        peer: libp2p::PeerId,
        message: libp2p::request_response::Message<Vec<u8>, Vec<u8>>,
    ) {
        use libp2p::request_response::Message;
        match message {
            Message::Request {
                request, channel, ..
            } => {
                let nexus_peer = PeerId::from_libp2p(&peer);
                self.pool.touch(&nexus_peer);
                // Forward to upper layers
                if self.incoming_tx.send((nexus_peer, request)).await.is_err() {
                    debug!("incoming_tx dropped; cannot deliver request");
                }
                // Send an empty ACK response
                let _ = self
                    .swarm
                    .behaviour_mut()
                    .reqres
                    .send_response(channel, Vec::new());
            }
            Message::Response { response, .. } => {
                if !response.is_empty() {
                    let nexus_peer = PeerId::from_libp2p(&peer);
                    self.pool.touch(&nexus_peer);
                    if self.incoming_tx.send((nexus_peer, response)).await.is_err() {
                        debug!("incoming_tx dropped; cannot deliver response");
                    }
                }
            }
        }
    }

    async fn handle_kad_event(&mut self, event: libp2p::kad::Event) {
        use libp2p::kad;

        match event {
            kad::Event::OutboundQueryProgressed {
                result: kad::QueryResult::GetClosestPeers(Ok(ok)),
                ..
            } => {
                debug!(count = ok.peers.len(), "kademlia closest peers found");
                if let Some(tx) = &self.kad_event_tx {
                    let _ = tx
                        .send(KadEvent::ClosestPeers {
                            peers: ok.peers.into_iter().map(|p| p.peer_id).collect(),
                        })
                        .await;
                }
            }
            kad::Event::OutboundQueryProgressed {
                result: kad::QueryResult::Bootstrap(Ok(_)),
                ..
            } => {
                debug!("kademlia bootstrap round ok");
                metrics::dht_bootstrap_completed();
                if let Some(tx) = &self.kad_event_tx {
                    let _ = tx.send(KadEvent::BootstrapOk).await;
                }
            }
            kad::Event::RoutingUpdated {
                peer, is_new_peer, ..
            } => {
                // Touch activity for peers that update our routing table
                let nexus_peer = PeerId::from_libp2p(&peer);
                self.pool.touch(&nexus_peer);

                debug!(%peer, is_new_peer, "kademlia routing updated");
                if let Some(tx) = &self.kad_event_tx {
                    let _ = tx
                        .send(KadEvent::RoutingUpdated { peer, is_new_peer })
                        .await;
                }
            }
            _ => {
                debug!(?event, "unhandled kademlia event");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_handle_is_clone() {
        fn assert_clone<T: Clone>() {}
        assert_clone::<TransportHandle>();
    }

    #[test]
    fn transport_handle_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<TransportHandle>();
    }

    #[tokio::test]
    async fn handle_shutdown_without_panic() {
        let config = NetworkConfig::for_testing();
        let (handle, manager, _incoming) =
            TransportManager::new(&config).expect("should build manager");

        let mgr_task = tokio::spawn(manager.run());
        handle.shutdown().await.expect("shutdown should succeed");
        mgr_task.await.expect("manager task should complete");
    }
}
