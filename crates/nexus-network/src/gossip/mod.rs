// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! GossipSub 1.1 message broadcasting — topic-based publish/subscribe.
//!
//! This module implements the [`GossipNetwork`] trait using libp2p GossipSub.
//! Upper layers (consensus, intent) subscribe to predefined topics and
//! receive messages through broadcast channels.

pub mod dedup;
pub mod scoring;

use std::collections::HashMap;
use std::sync::Arc;

use dashmap::DashMap;
use libp2p::gossipsub::{self, IdentTopic, Message as GossipMessage, TopicHash};
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use crate::config::NetworkConfig;
use crate::error::{NetworkError, NetworkResult};
use crate::types::Topic;

use self::dedup::MessageDedup;

// ── Topic Registry ───────────────────────────────────────────────────────────

/// Maps Nexus [`Topic`] to libp2p [`IdentTopic`] and tracks subscriptions.
pub struct TopicRegistry {
    topics: HashMap<Topic, IdentTopic>,
    subscribed: DashMap<Topic, bool>,
}

impl TopicRegistry {
    /// Create a new registry with all predefined global Nexus topics.
    pub fn new() -> Self {
        let mut topics = HashMap::new();
        for topic in Topic::global_topics() {
            topics.insert(*topic, IdentTopic::new(topic.topic_string()));
        }
        Self {
            topics,
            subscribed: DashMap::new(),
        }
    }

    /// Create a registry that includes per-shard topics for `num_shards` shards.
    pub fn with_shards(num_shards: u16) -> Self {
        let mut topics = HashMap::new();
        for topic in Topic::global_topics() {
            topics.insert(*topic, IdentTopic::new(topic.topic_string()));
        }
        for shard_topic in Topic::shard_topics(num_shards) {
            topics.insert(shard_topic, IdentTopic::new(shard_topic.topic_string()));
        }
        Self {
            topics,
            subscribed: DashMap::new(),
        }
    }

    /// Dynamically register a topic that was not part of the initial set.
    ///
    /// Returns `true` if the topic was newly inserted, `false` if it
    /// already existed.
    pub fn register(&mut self, topic: Topic) -> bool {
        if self.topics.contains_key(&topic) {
            return false;
        }
        self.topics
            .insert(topic, IdentTopic::new(topic.topic_string()));
        true
    }

    /// Get the libp2p topic for a Nexus topic.
    pub fn get_ident_topic(&self, topic: &Topic) -> Option<&IdentTopic> {
        self.topics.get(topic)
    }

    /// Get the libp2p TopicHash for a Nexus topic.
    pub fn get_topic_hash(&self, topic: &Topic) -> Option<TopicHash> {
        self.topics.get(topic).map(|t| t.hash())
    }

    /// Mark a topic as subscribed.
    pub fn mark_subscribed(&self, topic: Topic) {
        self.subscribed.insert(topic, true);
    }

    /// Mark a topic as unsubscribed.
    pub fn mark_unsubscribed(&self, topic: Topic) {
        self.subscribed.remove(&topic);
    }

    /// Check if we're subscribed to a topic.
    pub fn is_subscribed(&self, topic: &Topic) -> bool {
        self.subscribed.get(topic).map(|v| *v).unwrap_or(false)
    }

    /// Resolve a libp2p TopicHash back to a Nexus Topic.
    pub fn resolve_hash(&self, hash: &TopicHash) -> Option<Topic> {
        self.topics
            .iter()
            .find(|(_, ident)| ident.hash() == *hash)
            .map(|(topic, _)| *topic)
    }

    /// Return all currently registered topics.
    pub fn registered_topics(&self) -> Vec<Topic> {
        self.topics.keys().copied().collect()
    }
}

impl Default for TopicRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ── GossipHandle (clone-able sender) ─────────────────────────────────────────

/// Command sent from [`GossipHandle`] to the GossipService event loop.
#[derive(Debug)]
pub(crate) enum GossipCommand {
    /// Subscribe to a topic.
    Subscribe {
        topic: Topic,
        reply: tokio::sync::oneshot::Sender<Result<(), NetworkError>>,
    },
    /// Unsubscribe from a topic.
    Unsubscribe {
        topic: Topic,
        reply: tokio::sync::oneshot::Sender<Result<(), NetworkError>>,
    },
    /// Publish data to a topic.
    Publish {
        topic: Topic,
        data: Vec<u8>,
        reply: tokio::sync::oneshot::Sender<Result<(), NetworkError>>,
    },
}

/// Handle for interacting with the GossipService from upper layers.
#[derive(Clone)]
pub struct GossipHandle {
    cmd_tx: tokio::sync::mpsc::Sender<GossipCommand>,
    /// Broadcast receivers for each topic — upper layers subscribe here.
    receivers: Arc<DashMap<Topic, broadcast::Sender<Vec<u8>>>>,
}

impl GossipHandle {
    /// Subscribe to a topic.
    pub async fn subscribe(&self, topic: Topic) -> NetworkResult<()> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(GossipCommand::Subscribe {
                topic,
                reply: reply_tx,
            })
            .await
            .map_err(|_| NetworkError::ShuttingDown)?;
        reply_rx.await.map_err(|_| NetworkError::ShuttingDown)?
    }

    /// Unsubscribe from a topic.
    pub async fn unsubscribe(&self, topic: Topic) -> NetworkResult<()> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(GossipCommand::Unsubscribe {
                topic,
                reply: reply_tx,
            })
            .await
            .map_err(|_| NetworkError::ShuttingDown)?;
        reply_rx.await.map_err(|_| NetworkError::ShuttingDown)?
    }

    /// Publish data to a topic.
    pub async fn publish(&self, topic: Topic, data: Vec<u8>) -> NetworkResult<()> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(GossipCommand::Publish {
                topic,
                data,
                reply: reply_tx,
            })
            .await
            .map_err(|_| NetworkError::ShuttingDown)?;
        reply_rx.await.map_err(|_| NetworkError::ShuttingDown)?
    }

    /// Get a broadcast receiver for a specific topic.
    ///
    /// Messages published by peers arrive on this receiver. Callers should
    /// subscribe to the topic first via [`subscribe`](Self::subscribe).
    pub fn topic_receiver(&self, topic: Topic) -> broadcast::Receiver<Vec<u8>> {
        self.receivers
            .entry(topic)
            .or_insert_with(|| broadcast::channel(1024).0)
            .subscribe()
    }

    /// Inject a message directly into the local broadcast channel.
    ///
    /// This bypasses GossipSub entirely — the message is never sent to
    /// the network. Use for testing or for locally-generated events that
    /// should reach the same pipeline as incoming gossip messages.
    pub fn inject_local(&self, topic: Topic, data: Vec<u8>) {
        if let Some(tx) = self.receivers.get(&topic) {
            let _ = tx.send(data);
        }
    }
}

// ── GossipService (command processor) ────────────────────────────────────────

/// Processes gossip commands from handles. Integrated into the NetworkService
/// event loop — doesn't run its own task.
pub struct GossipService {
    registry: TopicRegistry,
    dedup: MessageDedup,
    cmd_rx: Option<tokio::sync::mpsc::Receiver<GossipCommand>>,
    broadcast_txs: Arc<DashMap<Topic, broadcast::Sender<Vec<u8>>>>,
}

impl GossipService {
    /// Create a new GossipService and its handle (global topics only).
    pub fn new(config: &NetworkConfig) -> (GossipHandle, Self) {
        Self::build(config, TopicRegistry::new(), Topic::global_topics())
    }

    /// Create a GossipService pre-configured with per-shard topics.
    pub fn new_with_shards(config: &NetworkConfig, num_shards: u16) -> (GossipHandle, Self) {
        let registry = TopicRegistry::with_shards(num_shards);
        let all_topics: Vec<Topic> = registry.registered_topics();
        Self::build(config, registry, &all_topics)
    }

    fn build(
        config: &NetworkConfig,
        registry: TopicRegistry,
        topics: &[Topic],
    ) -> (GossipHandle, Self) {
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel(256);
        let broadcast_txs = Arc::new(DashMap::new());

        // Pre-create broadcast channels for all provided topics
        for topic in topics {
            broadcast_txs.insert(*topic, broadcast::channel(1024).0);
        }

        let handle = GossipHandle {
            cmd_tx,
            receivers: Arc::clone(&broadcast_txs),
        };

        let service = Self {
            registry,
            dedup: MessageDedup::new(config),
            cmd_rx: Some(cmd_rx),
            broadcast_txs,
        };

        (handle, service)
    }

    /// Drain pending commands, applying them to the gossipsub behaviour.
    ///
    /// Called by the NetworkService event loop on each iteration.
    pub fn process_commands(&mut self, gossipsub: &mut gossipsub::Behaviour) {
        let mut cmds = Vec::new();
        if let Some(rx) = &mut self.cmd_rx {
            while let Ok(cmd) = rx.try_recv() {
                cmds.push(cmd);
            }
        }
        for cmd in cmds {
            self.handle_gossip_command(gossipsub, cmd);
        }
    }

    /// Take the command receiver out of this service.
    ///
    /// After calling this, `process_commands()` and `recv_command()` will no
    /// longer receive commands — the caller is responsible for polling the
    /// returned receiver and feeding commands back via `handle_gossip_command`.
    pub(crate) fn take_cmd_rx(&mut self) -> Option<tokio::sync::mpsc::Receiver<GossipCommand>> {
        self.cmd_rx.take()
    }

    /// Process a single gossip command.
    pub(crate) fn handle_gossip_command(
        &mut self,
        gossipsub: &mut gossipsub::Behaviour,
        cmd: GossipCommand,
    ) {
        match cmd {
            GossipCommand::Subscribe { topic, reply } => {
                let result = self.do_subscribe(gossipsub, topic);
                let _ = reply.send(result);
            }
            GossipCommand::Unsubscribe { topic, reply } => {
                let result = self.do_unsubscribe(gossipsub, topic);
                let _ = reply.send(result);
            }
            GossipCommand::Publish { topic, data, reply } => {
                let result = self.do_publish(gossipsub, topic, data);
                let _ = reply.send(result);
            }
        }
    }

    /// Handle an incoming gossipsub message from the swarm.
    pub fn on_message(&mut self, msg: &GossipMessage) {
        // Deduplicate
        if !self.dedup.is_new(&msg.data) {
            debug!("duplicate gossip message dropped");
            crate::metrics::gossip_message_deduplicated();
            return;
        }

        // Resolve topic
        if let Some(topic) = self.registry.resolve_hash(&msg.topic) {
            crate::metrics::gossip_message_received(&topic);
            if let Some(tx) = self.broadcast_txs.get(&topic) {
                let _ = tx.send(msg.data.clone());
                debug!(?topic, bytes = msg.data.len(), "gossip message dispatched");
            }
        } else {
            warn!(topic_hash = %msg.topic, "message on unknown topic");
        }
    }

    fn do_subscribe(
        &mut self,
        gossipsub: &mut gossipsub::Behaviour,
        topic: Topic,
    ) -> Result<(), NetworkError> {
        // Dynamically register the topic if it is sharded and not yet known.
        if self.registry.get_ident_topic(&topic).is_none() && topic.is_sharded() {
            self.registry.register(topic);
            // Also ensure a broadcast channel exists
            self.broadcast_txs
                .entry(topic)
                .or_insert_with(|| broadcast::channel(1024).0);
        }
        let ident =
            self.registry
                .get_ident_topic(&topic)
                .ok_or_else(|| NetworkError::UnknownTopic {
                    topic: topic.to_string(),
                })?;
        gossipsub
            .subscribe(ident)
            .map_err(|e| NetworkError::InvalidMessage {
                reason: format!("subscribe failed: {}", e),
            })?;
        self.registry.mark_subscribed(topic);
        info!(?topic, "subscribed");
        Ok(())
    }

    fn do_unsubscribe(
        &mut self,
        gossipsub: &mut gossipsub::Behaviour,
        topic: Topic,
    ) -> Result<(), NetworkError> {
        let ident =
            self.registry
                .get_ident_topic(&topic)
                .ok_or_else(|| NetworkError::UnknownTopic {
                    topic: topic.to_string(),
                })?;
        gossipsub
            .unsubscribe(ident)
            .map_err(|e| NetworkError::InvalidMessage {
                reason: format!("unsubscribe failed: {}", e),
            })?;
        self.registry.mark_unsubscribed(topic);
        info!(?topic, "unsubscribed");
        Ok(())
    }

    fn do_publish(
        &self,
        gossipsub: &mut gossipsub::Behaviour,
        topic: Topic,
        data: Vec<u8>,
    ) -> Result<(), NetworkError> {
        if !self.registry.is_subscribed(&topic) {
            return Err(NetworkError::UnknownTopic {
                topic: topic.to_string(),
            });
        }
        let ident =
            self.registry
                .get_ident_topic(&topic)
                .ok_or_else(|| NetworkError::UnknownTopic {
                    topic: topic.to_string(),
                })?;
        gossipsub
            .publish(ident.clone(), data.clone())
            .map_err(|e| NetworkError::InvalidMessage {
                reason: format!("publish failed: {}", e),
            })?;
        crate::metrics::gossip_message_published(&topic);
        crate::metrics::bytes_sent(data.len() as u64);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_registry_has_all_topics() {
        let registry = TopicRegistry::new();
        assert!(registry.get_ident_topic(&Topic::Consensus).is_some());
        assert!(registry.get_ident_topic(&Topic::Transaction).is_some());
        assert!(registry.get_ident_topic(&Topic::Intent).is_some());
        assert!(registry.get_ident_topic(&Topic::StateSync).is_some());
    }

    #[test]
    fn topic_subscription_tracking() {
        let registry = TopicRegistry::new();
        assert!(!registry.is_subscribed(&Topic::Consensus));

        registry.mark_subscribed(Topic::Consensus);
        assert!(registry.is_subscribed(&Topic::Consensus));

        registry.mark_unsubscribed(Topic::Consensus);
        assert!(!registry.is_subscribed(&Topic::Consensus));
    }

    #[test]
    fn topic_hash_resolution() {
        let registry = TopicRegistry::new();
        let hash = registry.get_topic_hash(&Topic::Transaction).unwrap();
        let resolved = registry.resolve_hash(&hash);
        assert_eq!(resolved, Some(Topic::Transaction));
    }

    #[test]
    fn gossip_handle_is_clone_send_sync() {
        fn assert_bounds<T: Clone + Send + Sync>() {}
        assert_bounds::<GossipHandle>();
    }

    #[test]
    fn gossip_service_creation() {
        let config = NetworkConfig::for_testing();
        let (handle, _service) = GossipService::new(&config);
        // Should be able to get receivers for all topics
        let _rx = handle.topic_receiver(Topic::Consensus);
        let _rx = handle.topic_receiver(Topic::Transaction);
    }

    #[tokio::test]
    async fn broadcast_channel_works() {
        let config = NetworkConfig::for_testing();
        let (handle, _service) = GossipService::new(&config);

        let mut rx = handle.topic_receiver(Topic::Consensus);

        // Simulate a message arriving via the broadcast channel
        if let Some(tx) = handle.receivers.get(&Topic::Consensus) {
            let _ = tx.send(vec![1, 2, 3]);
        }

        let msg = rx.recv().await.expect("should receive message");
        assert_eq!(msg, vec![1, 2, 3]);
    }

    // ── W-1: Shard-aware topic registry tests ────────────────────────────

    #[test]
    fn shard_registry_includes_global_and_shard_topics() {
        let registry = TopicRegistry::with_shards(2);
        assert!(registry.get_ident_topic(&Topic::Consensus).is_some());
        assert!(registry.get_ident_topic(&Topic::Transaction).is_some());
        assert!(registry
            .get_ident_topic(&Topic::ShardedTransaction(0))
            .is_some());
        assert!(registry
            .get_ident_topic(&Topic::ShardedTransaction(1))
            .is_some());
        assert!(registry
            .get_ident_topic(&Topic::ShardedCertificate(0))
            .is_some());
        assert!(registry
            .get_ident_topic(&Topic::ShardedCertificate(1))
            .is_some());
        // Out-of-range shard should not be registered
        assert!(registry
            .get_ident_topic(&Topic::ShardedTransaction(2))
            .is_none());
    }

    #[test]
    fn shard_topic_hash_resolution() {
        let registry = TopicRegistry::with_shards(3);
        let hash = registry
            .get_topic_hash(&Topic::ShardedTransaction(1))
            .unwrap();
        let resolved = registry.resolve_hash(&hash);
        assert_eq!(resolved, Some(Topic::ShardedTransaction(1)));
    }

    #[test]
    fn dynamic_register_new_shard_topic() {
        let mut registry = TopicRegistry::new();
        assert!(registry
            .get_ident_topic(&Topic::ShardedTransaction(5))
            .is_none());
        assert!(registry.register(Topic::ShardedTransaction(5)));
        assert!(registry
            .get_ident_topic(&Topic::ShardedTransaction(5))
            .is_some());
        // Second register returns false (already present)
        assert!(!registry.register(Topic::ShardedTransaction(5)));
    }

    #[test]
    fn gossip_service_with_shards() {
        let config = NetworkConfig::for_testing();
        let (handle, _service) = GossipService::new_with_shards(&config, 2);
        // Should have broadcast channels for shard topics
        let _rx = handle.topic_receiver(Topic::ShardedTransaction(0));
        let _rx = handle.topic_receiver(Topic::ShardedTransaction(1));
        let _rx = handle.topic_receiver(Topic::ShardedCertificate(0));
    }

    #[tokio::test]
    async fn shard_topic_broadcast_channel_isolation() {
        let config = NetworkConfig::for_testing();
        let (handle, _service) = GossipService::new_with_shards(&config, 2);

        let mut rx0 = handle.topic_receiver(Topic::ShardedTransaction(0));
        let mut rx1 = handle.topic_receiver(Topic::ShardedTransaction(1));

        // Send message to shard 0 topic
        handle.inject_local(Topic::ShardedTransaction(0), vec![10, 20]);

        let msg0 = rx0.recv().await.expect("shard 0 should receive");
        assert_eq!(msg0, vec![10, 20]);

        // Shard 1 should not have received it (use try_recv)
        assert!(
            rx1.try_recv().is_err(),
            "shard 1 should not receive shard 0 message"
        );
    }

    #[test]
    fn registered_topics_includes_all() {
        let registry = TopicRegistry::with_shards(1);
        let topics = registry.registered_topics();
        // 4 global + 2 shard topics (tx + cert for shard 0)
        assert_eq!(topics.len(), 6);
    }
}
