// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Network-layer observability counters and gauges.
//!
//! All metrics use the [`metrics`] facade crate. The actual exporter
//! (Prometheus, OpenTelemetry, etc.) is plugged in at the application level.
//!
//! # Naming Convention
//! `nexus_network_<subsystem>_<metric>` — e.g. `nexus_network_connections_active`.

use metrics::{counter, gauge};

use crate::types::Topic;

// ── Connection Metrics ───────────────────────────────────────────────────────

/// Increment when a new peer connection is established.
pub fn connection_established() {
    counter!("nexus_network_connections_established_total").increment(1);
    gauge!("nexus_network_connections_active").increment(1.0);
}

/// Decrement when a peer connection closes.
pub fn connection_closed() {
    counter!("nexus_network_connections_closed_total").increment(1);
    gauge!("nexus_network_connections_active").decrement(1.0);
}

/// Record a peer ban event.
pub fn peer_banned() {
    counter!("nexus_network_peers_banned_total").increment(1);
}

// ── GossipSub Metrics ────────────────────────────────────────────────────────

/// A message was published to a topic.
pub fn gossip_message_published(topic: &Topic) {
    counter!("nexus_network_gossip_published_total", "topic" => topic.topic_string()).increment(1);
}

/// A message was received from a topic.
pub fn gossip_message_received(topic: &Topic) {
    counter!("nexus_network_gossip_received_total", "topic" => topic.topic_string()).increment(1);
}

/// A duplicate message was suppressed by the dedup layer.
pub fn gossip_message_deduplicated() {
    counter!("nexus_network_gossip_deduplicated_total").increment(1);
}

// ── Discovery / DHT Metrics ─────────────────────────────────────────────────

/// A Kademlia bootstrap round completed.
pub fn dht_bootstrap_completed() {
    counter!("nexus_network_dht_bootstrap_completed_total").increment(1);
}

/// Set the current number of known routing-table peers.
pub fn dht_routing_table_size(size: usize) {
    gauge!("nexus_network_dht_routing_table_size").set(size as f64);
}

/// A disjoint lookup completed successfully.
pub fn dht_disjoint_lookup_completed() {
    counter!("nexus_network_dht_disjoint_lookups_total").increment(1);
}

// ── Rate Limiting Metrics ────────────────────────────────────────────────────

/// A message was dropped due to rate limiting.
pub fn rate_limit_exceeded() {
    counter!("nexus_network_rate_limit_exceeded_total").increment(1);
}

// ── Bandwidth Metrics ────────────────────────────────────────────────────────

/// Record bytes sent.
pub fn bytes_sent(n: u64) {
    counter!("nexus_network_bytes_sent_total").increment(n);
}

/// Record bytes received.
pub fn bytes_received(n: u64) {
    counter!("nexus_network_bytes_received_total").increment(n);
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // metrics facade functions are no-ops without a recorder installed,
    // so we can call them safely in tests to verify they don't panic.

    #[test]
    fn connection_metrics_do_not_panic() {
        connection_established();
        connection_closed();
        peer_banned();
    }

    #[test]
    fn gossip_metrics_do_not_panic() {
        gossip_message_published(&Topic::Consensus);
        gossip_message_published(&Topic::Transaction);
        gossip_message_received(&Topic::Intent);
        gossip_message_received(&Topic::StateSync);
        gossip_message_deduplicated();
    }

    #[test]
    fn dht_metrics_do_not_panic() {
        dht_bootstrap_completed();
        dht_routing_table_size(42);
        dht_disjoint_lookup_completed();
    }

    #[test]
    fn rate_limit_metric_does_not_panic() {
        rate_limit_exceeded();
    }

    #[test]
    fn bandwidth_metrics_do_not_panic() {
        bytes_sent(1024);
        bytes_received(2048);
    }
}
