//! GossipSub peer scoring parameters — reputation-based relay decisions.
//!
//! Defines scoring policies that reward honest message relay and penalize
//! misbehaviour (invalid messages, excessive flooding, mesh underperformance).

use std::time::Duration;

use libp2p::gossipsub::{PeerScoreParams, PeerScoreThresholds, TopicScoreParams};

use crate::types::Topic;

// ── Scoring Constants ────────────────────────────────────────────────────────

/// Default score decay interval.
const DECAY_INTERVAL: Duration = Duration::from_secs(1);

/// Time before a peer score slot is reclaimed.
const RETAIN_SCORE_SECS: u64 = 3600; // 1 hour

/// Penalty for invalid messages.
const INVALID_MESSAGE_WEIGHT: f64 = -10.0;

/// Penalty for IP colocation (potential Sybil).
const IP_COLOCATION_WEIGHT: f64 = -5.0;

/// Max peers on same IP before colocation penalty triggers.
const IP_COLOCATION_THRESHOLD: f64 = 3.0;

// ── Topic-level Score Params ─────────────────────────────────────────────────

/// Build topic-specific scoring parameters.
///
/// High-throughput topics (Consensus, Transaction) have tighter mesh delivery
/// requirements; lower-throughput topics (Intent, StateSync) are more relaxed.
pub fn topic_score_params(topic: &Topic) -> TopicScoreParams {
    match topic {
        Topic::Consensus => TopicScoreParams {
            topic_weight: 1.0,
            time_in_mesh_weight: 0.5,
            time_in_mesh_quantum: Duration::from_secs(1),
            time_in_mesh_cap: 60.0,
            first_message_deliveries_weight: 2.0,
            first_message_deliveries_decay: 0.97,
            first_message_deliveries_cap: 100.0,
            mesh_message_deliveries_weight: -1.0,
            mesh_message_deliveries_decay: 0.97,
            mesh_message_deliveries_cap: 100.0,
            mesh_message_deliveries_threshold: 5.0,
            mesh_message_deliveries_window: Duration::from_millis(500),
            mesh_message_deliveries_activation: Duration::from_secs(5),
            mesh_failure_penalty_weight: -2.0,
            mesh_failure_penalty_decay: 0.95,
            invalid_message_deliveries_weight: INVALID_MESSAGE_WEIGHT,
            invalid_message_deliveries_decay: 0.9,
        },
        Topic::Transaction => TopicScoreParams {
            topic_weight: 0.8,
            time_in_mesh_weight: 0.3,
            time_in_mesh_quantum: Duration::from_secs(1),
            time_in_mesh_cap: 60.0,
            first_message_deliveries_weight: 1.5,
            first_message_deliveries_decay: 0.97,
            first_message_deliveries_cap: 200.0,
            mesh_message_deliveries_weight: -0.5,
            mesh_message_deliveries_decay: 0.97,
            mesh_message_deliveries_cap: 200.0,
            mesh_message_deliveries_threshold: 3.0,
            mesh_message_deliveries_window: Duration::from_millis(500),
            mesh_message_deliveries_activation: Duration::from_secs(10),
            mesh_failure_penalty_weight: -1.0,
            mesh_failure_penalty_decay: 0.95,
            invalid_message_deliveries_weight: INVALID_MESSAGE_WEIGHT,
            invalid_message_deliveries_decay: 0.9,
        },
        Topic::Intent | Topic::StateSync => TopicScoreParams {
            topic_weight: 0.5,
            time_in_mesh_weight: 0.2,
            time_in_mesh_quantum: Duration::from_secs(1),
            time_in_mesh_cap: 30.0,
            first_message_deliveries_weight: 1.0,
            first_message_deliveries_decay: 0.98,
            first_message_deliveries_cap: 50.0,
            mesh_message_deliveries_weight: -0.3,
            mesh_message_deliveries_decay: 0.98,
            mesh_message_deliveries_cap: 50.0,
            mesh_message_deliveries_threshold: 1.0,
            mesh_message_deliveries_window: Duration::from_millis(1000),
            mesh_message_deliveries_activation: Duration::from_secs(15),
            mesh_failure_penalty_weight: -0.5,
            mesh_failure_penalty_decay: 0.95,
            invalid_message_deliveries_weight: INVALID_MESSAGE_WEIGHT,
            invalid_message_deliveries_decay: 0.9,
        },
        // Sharded tx topics inherit Transaction-level scoring.
        Topic::ShardedTransaction(_) => TopicScoreParams {
            topic_weight: 0.8,
            time_in_mesh_weight: 0.3,
            time_in_mesh_quantum: Duration::from_secs(1),
            time_in_mesh_cap: 60.0,
            first_message_deliveries_weight: 1.5,
            first_message_deliveries_decay: 0.97,
            first_message_deliveries_cap: 200.0,
            mesh_message_deliveries_weight: -0.5,
            mesh_message_deliveries_decay: 0.97,
            mesh_message_deliveries_cap: 200.0,
            mesh_message_deliveries_threshold: 3.0,
            mesh_message_deliveries_window: Duration::from_millis(500),
            mesh_message_deliveries_activation: Duration::from_secs(10),
            mesh_failure_penalty_weight: -1.0,
            mesh_failure_penalty_decay: 0.95,
            invalid_message_deliveries_weight: INVALID_MESSAGE_WEIGHT,
            invalid_message_deliveries_decay: 0.9,
        },
        // Sharded certificate topics inherit Consensus-level scoring.
        Topic::ShardedCertificate(_) => TopicScoreParams {
            topic_weight: 1.0,
            time_in_mesh_weight: 0.5,
            time_in_mesh_quantum: Duration::from_secs(1),
            time_in_mesh_cap: 60.0,
            first_message_deliveries_weight: 2.0,
            first_message_deliveries_decay: 0.97,
            first_message_deliveries_cap: 100.0,
            mesh_message_deliveries_weight: -1.0,
            mesh_message_deliveries_decay: 0.97,
            mesh_message_deliveries_cap: 100.0,
            mesh_message_deliveries_threshold: 5.0,
            mesh_message_deliveries_window: Duration::from_millis(500),
            mesh_message_deliveries_activation: Duration::from_secs(5),
            mesh_failure_penalty_weight: -2.0,
            mesh_failure_penalty_decay: 0.95,
            invalid_message_deliveries_weight: INVALID_MESSAGE_WEIGHT,
            invalid_message_deliveries_decay: 0.9,
        },
    }
}

// ── Peer-level Score Params ──────────────────────────────────────────────────

/// Build the peer-level score parameters for GossipSub.
pub fn peer_score_params() -> PeerScoreParams {
    PeerScoreParams {
        decay_interval: DECAY_INTERVAL,
        decay_to_zero: 0.01,
        retain_score: Duration::from_secs(RETAIN_SCORE_SECS),
        app_specific_weight: 1.0,
        ip_colocation_factor_weight: IP_COLOCATION_WEIGHT,
        ip_colocation_factor_threshold: IP_COLOCATION_THRESHOLD,
        ..Default::default()
    }
}

/// Build score thresholds that control peer treatment.
pub fn peer_score_thresholds() -> PeerScoreThresholds {
    PeerScoreThresholds {
        gossip_threshold: -100.0,
        publish_threshold: -200.0,
        graylist_threshold: -300.0,
        accept_px_threshold: 10.0,
        opportunistic_graft_threshold: 5.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consensus_topic_has_highest_weight() {
        let consensus = topic_score_params(&Topic::Consensus);
        let tx = topic_score_params(&Topic::Transaction);
        let intent = topic_score_params(&Topic::Intent);

        assert!(consensus.topic_weight > tx.topic_weight);
        assert!(tx.topic_weight > intent.topic_weight);
    }

    #[test]
    fn invalid_message_penalty_is_negative() {
        for topic in &[
            Topic::Consensus,
            Topic::Transaction,
            Topic::Intent,
            Topic::StateSync,
        ] {
            let params = topic_score_params(topic);
            assert!(
                params.invalid_message_deliveries_weight < 0.0,
                "invalid messages should always be penalized for {:?}",
                topic
            );
        }
    }

    #[test]
    fn peer_score_params_are_valid() {
        let params = peer_score_params();
        assert!(params.decay_interval > Duration::ZERO);
        assert!(params.ip_colocation_factor_weight < 0.0);
        assert!(params.ip_colocation_factor_threshold > 0.0);
    }

    #[test]
    fn thresholds_are_ordered() {
        let t = peer_score_thresholds();
        // More negative = more severe
        assert!(t.gossip_threshold > t.publish_threshold);
        assert!(t.publish_threshold > t.graylist_threshold);
    }

    #[test]
    fn mesh_delivery_activation_is_positive() {
        for topic in &[
            Topic::Consensus,
            Topic::Transaction,
            Topic::Intent,
            Topic::StateSync,
        ] {
            let params = topic_score_params(topic);
            assert!(params.mesh_message_deliveries_activation > Duration::ZERO);
        }
    }
}
