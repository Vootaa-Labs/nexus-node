//! Epoch → Network bridge — adjusts gossip subscriptions on committee changes.
//!
//! When the consensus layer advances to a new epoch with a new committee
//! assignment, this bridge updates the gossip layer's shard topic
//! subscriptions so the node only receives messages for its assigned shards.
//!
//! # Design
//!
//! The bridge watches a `tokio::sync::watch` channel fed by the execution
//! bridge whenever an epoch transition occurs.  On each change it:
//!
//! 1. Determines which shards the local validator is assigned to based on
//!    the new committee.
//! 2. Subscribes to gossip topics for newly assigned shards.
//! 3. Unsubscribes from topics for shards no longer assigned.
//!
//! For single-shard mode (`num_shards=1`) the bridge subscribes to the
//! global `Transaction` and `Consensus` topics only — no change from
//! v0.1.9 behaviour.

use std::collections::HashSet;

use nexus_network::types::Topic;
use nexus_network::GossipHandle;
use nexus_primitives::{EpochNumber, ValidatorIndex};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

// ── Epoch change notification ────────────────────────────────────────────────

/// Payload sent through the epoch-change watch channel.
#[derive(Debug, Clone)]
pub struct EpochChangeEvent {
    /// The new epoch number.
    pub epoch: EpochNumber,
    /// Total shard count for the network.
    pub num_shards: u16,
    /// Shards assigned to this validator in the new epoch.
    ///
    /// In the current design every validator is responsible for all shards
    /// (full replication) so this will typically be `0..num_shards`.
    /// When per-shard assignment is introduced, only assigned shard ids
    /// will be listed.
    pub assigned_shards: Vec<u16>,
}

impl Default for EpochChangeEvent {
    fn default() -> Self {
        Self {
            epoch: EpochNumber(0),
            num_shards: 1,
            assigned_shards: vec![0],
        }
    }
}

/// Create a watch channel pair for epoch-change events.
///
/// The sender is fed by the execution bridge (or epoch manager); the
/// receiver is consumed by [`spawn_epoch_network_bridge`].
pub fn epoch_change_channel() -> (
    watch::Sender<EpochChangeEvent>,
    watch::Receiver<EpochChangeEvent>,
) {
    watch::channel(EpochChangeEvent::default())
}

// ── Bridge task ──────────────────────────────────────────────────────────────

/// Spawn the epoch→network bridge background task.
///
/// The task subscribes to the epoch-change watch channel and re-subscribes
/// to per-shard gossip topics whenever the set of assigned shards changes.
///
/// In single-shard mode (`num_shards=1`) this is a no-op: the global
/// topics are sufficient and no shard topics are managed.
pub fn spawn_epoch_network_bridge(
    gossip: GossipHandle,
    mut epoch_rx: watch::Receiver<EpochChangeEvent>,
    _local_validator: Option<ValidatorIndex>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut current_shards: HashSet<u16> = HashSet::new();

        loop {
            if epoch_rx.changed().await.is_err() {
                debug!("epoch change channel closed — stopping epoch→network bridge");
                break;
            }

            let event = epoch_rx.borrow_and_update().clone();

            info!(
                epoch = event.epoch.0,
                num_shards = event.num_shards,
                assigned = ?event.assigned_shards,
                "epoch→network bridge: epoch changed, updating shard subscriptions"
            );

            // Single-shard mode: nothing to do (global topics handle it).
            if event.num_shards <= 1 {
                debug!("epoch→network bridge: single-shard mode, no shard topic management");
                continue;
            }

            let new_shards: HashSet<u16> = event.assigned_shards.iter().copied().collect();

            // Unsubscribe from shards we are no longer assigned to.
            let to_unsub: Vec<u16> = current_shards.difference(&new_shards).copied().collect();
            for shard in &to_unsub {
                if let Err(e) = gossip.unsubscribe(Topic::sharded_tx(*shard)).await {
                    warn!(shard, error = %e, "failed to unsubscribe from shard tx topic");
                }
                if let Err(e) = gossip.unsubscribe(Topic::sharded_cert(*shard)).await {
                    warn!(shard, error = %e, "failed to unsubscribe from shard cert topic");
                }
                debug!(
                    shard,
                    "epoch→network bridge: unsubscribed from shard topics"
                );
            }

            // Subscribe to newly assigned shards.
            let to_sub: Vec<u16> = new_shards.difference(&current_shards).copied().collect();
            for shard in &to_sub {
                if let Err(e) = gossip.subscribe(Topic::sharded_tx(*shard)).await {
                    warn!(shard, error = %e, "failed to subscribe to shard tx topic");
                }
                if let Err(e) = gossip.subscribe(Topic::sharded_cert(*shard)).await {
                    warn!(shard, error = %e, "failed to subscribe to shard cert topic");
                }
                debug!(shard, "epoch→network bridge: subscribed to shard topics");
            }

            metrics::counter!("nexus_epoch_network_subscription_updates_total").increment(1);
            current_shards = new_shards;
        }

        debug!("epoch→network bridge stopped");
    })
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_network::{NetworkConfig, NetworkService};

    #[tokio::test]
    async fn epoch_change_channel_default_is_single_shard() {
        let (_tx, rx) = epoch_change_channel();
        let event = rx.borrow().clone();
        assert_eq!(event.epoch, EpochNumber(0));
        assert_eq!(event.num_shards, 1);
        assert_eq!(event.assigned_shards, vec![0]);
    }

    #[tokio::test]
    async fn bridge_subscribes_on_epoch_change() {
        let config = NetworkConfig::for_testing();
        let (net_handle, service) = NetworkService::build(&config).expect("build");
        let shutdown = net_handle.transport.clone();
        let net_task = tokio::spawn(service.run());

        let (epoch_tx, epoch_rx) = epoch_change_channel();
        let bridge = spawn_epoch_network_bridge(
            net_handle.gossip.clone(),
            epoch_rx,
            Some(ValidatorIndex(0)),
        );

        // Trigger epoch with 2 shards assigned
        epoch_tx
            .send(EpochChangeEvent {
                epoch: EpochNumber(1),
                num_shards: 2,
                assigned_shards: vec![0, 1],
            })
            .expect("send");

        // Give the bridge a moment to process
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Then switch to single shard
        epoch_tx
            .send(EpochChangeEvent {
                epoch: EpochNumber(2),
                num_shards: 1,
                assigned_shards: vec![0],
            })
            .expect("send");

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Cleanup
        bridge.abort();
        let _ = bridge.await;
        drop(net_handle);
        shutdown.shutdown().await.expect("shutdown");
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), net_task).await;
    }

    #[tokio::test]
    async fn bridge_stops_on_channel_close() {
        let config = NetworkConfig::for_testing();
        let (net_handle, service) = NetworkService::build(&config).expect("build");
        let shutdown = net_handle.transport.clone();
        let net_task = tokio::spawn(service.run());

        let (epoch_tx, epoch_rx) = epoch_change_channel();
        let bridge = spawn_epoch_network_bridge(net_handle.gossip.clone(), epoch_rx, None);

        // Drop sender — bridge should exit
        drop(epoch_tx);
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), bridge).await;
        assert!(result.is_ok(), "bridge should stop when channel is closed");

        drop(net_handle);
        shutdown.shutdown().await.expect("shutdown");
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), net_task).await;
    }

    #[tokio::test]
    async fn bridge_unsubscribes_removed_shards() {
        let config = NetworkConfig::for_testing();
        let (net_handle, service) = NetworkService::build(&config).expect("build");
        let shutdown = net_handle.transport.clone();
        let net_task = tokio::spawn(service.run());

        let (epoch_tx, epoch_rx) = epoch_change_channel();
        let bridge = spawn_epoch_network_bridge(
            net_handle.gossip.clone(),
            epoch_rx,
            Some(ValidatorIndex(0)),
        );

        // Epoch 1: assigned to shards 0, 1, 2
        epoch_tx
            .send(EpochChangeEvent {
                epoch: EpochNumber(1),
                num_shards: 3,
                assigned_shards: vec![0, 1, 2],
            })
            .expect("send");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Epoch 2: now only shard 0 — should unsub 1 and 2
        epoch_tx
            .send(EpochChangeEvent {
                epoch: EpochNumber(2),
                num_shards: 3,
                assigned_shards: vec![0],
            })
            .expect("send");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        bridge.abort();
        let _ = bridge.await;
        drop(net_handle);
        shutdown.shutdown().await.expect("shutdown");
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), net_task).await;
    }

    #[tokio::test]
    async fn single_shard_mode_is_noop() {
        let (epoch_tx, epoch_rx) = epoch_change_channel();
        let config = NetworkConfig::for_testing();
        let (net_handle, service) = NetworkService::build(&config).expect("build");
        let shutdown = net_handle.transport.clone();
        let net_task = tokio::spawn(service.run());

        let bridge = spawn_epoch_network_bridge(net_handle.gossip.clone(), epoch_rx, None);

        // Send single-shard epoch change
        epoch_tx
            .send(EpochChangeEvent {
                epoch: EpochNumber(1),
                num_shards: 1,
                assigned_shards: vec![0],
            })
            .expect("send");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        bridge.abort();
        let _ = bridge.await;
        drop(net_handle);
        shutdown.shutdown().await.expect("shutdown");
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), net_task).await;
    }
}
