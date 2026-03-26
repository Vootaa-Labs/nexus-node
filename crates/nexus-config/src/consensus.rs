//! Consensus subsystem configuration.
//!
//! Parameters for Narwhal DAG workers, batch sizing, Shoal++ anchor
//! intervals, reputation scoring, and slashing economics.

use serde::{Deserialize, Serialize};

/// Configuration for the Nexus consensus layer (Narwhal + Shoal++).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ConsensusConfig {
    /// Maximum batch payload size in bytes. Default: 512 KiB.
    pub max_batch_size_bytes: usize,

    /// Time limit (ms) before a partially-filled batch is sealed. Default: 200.
    pub batch_timeout_ms: u64,

    /// Number of Narwhal worker threads per validator. Default: 4.
    pub num_workers: usize,

    /// DAG garbage-collection depth (rounds). Default: 100.
    pub gc_depth: u64,

    /// Sliding window (rounds) for reputation scoring. Default: 300.
    pub reputation_window_rounds: u64,

    /// Exponential decay factor applied each round. Default: 0.99.
    pub reputation_decay: f64,

    /// Shoal++ anchor election interval (rounds). Default: 2.
    pub anchor_interval_rounds: u64,

    /// Percentage of stake slashed for double-signing. Default: 50.
    pub slashing_double_sign_pct: u8,

    /// Percentage of stake slashed for prolonged offline. Default: 10.
    pub slashing_offline_pct: u8,

    /// Number of rounds per epoch. Default: 1000.
    pub epoch_length_rounds: u64,

    /// Interval (epochs) between validator election rotations. Default: 1.
    pub validator_election_epoch_interval: u64,

    /// Interval (ms) between empty batch proposals.  When the mempool has no
    /// pending transactions the proposer sleeps this long instead of the
    /// standard `batch_timeout_ms`.  A larger value prevents the DAG from
    /// racing ahead of the Shoal commit anchor during idle periods.
    /// Default: 200 (same as `batch_timeout_ms`).
    pub empty_proposal_interval_ms: u64,
}

impl Default for ConsensusConfig {
    fn default() -> Self {
        Self {
            max_batch_size_bytes: 524_288, // 512 KiB
            batch_timeout_ms: 200,
            num_workers: 4,
            gc_depth: 100,
            reputation_window_rounds: 300,
            reputation_decay: 0.99,
            anchor_interval_rounds: 2,
            slashing_double_sign_pct: 50,
            slashing_offline_pct: 10,
            epoch_length_rounds: 1000,
            validator_election_epoch_interval: 1,
            empty_proposal_interval_ms: 200,
        }
    }
}

impl ConsensusConfig {
    /// Minimal configuration suitable for tests.
    pub fn for_testing() -> Self {
        Self {
            max_batch_size_bytes: 4096,
            batch_timeout_ms: 50,
            num_workers: 1,
            gc_depth: 10,
            reputation_window_rounds: 10,
            reputation_decay: 0.95,
            anchor_interval_rounds: 1,
            slashing_double_sign_pct: 50,
            slashing_offline_pct: 10,
            epoch_length_rounds: 10,
            validator_election_epoch_interval: 1,
            empty_proposal_interval_ms: 50,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_values() {
        let cfg = ConsensusConfig::default();
        assert_eq!(cfg.max_batch_size_bytes, 524_288);
        assert_eq!(cfg.batch_timeout_ms, 200);
        assert_eq!(cfg.num_workers, 4);
        assert_eq!(cfg.gc_depth, 100);
        assert_eq!(cfg.epoch_length_rounds, 1000);
        assert!(cfg.reputation_decay > 0.0 && cfg.reputation_decay <= 1.0);
    }

    #[test]
    fn testing_config_is_small() {
        let cfg = ConsensusConfig::for_testing();
        assert!(cfg.max_batch_size_bytes < 524_288);
        assert!(cfg.epoch_length_rounds < 100);
        assert_eq!(cfg.num_workers, 1);
    }

    #[test]
    fn slashing_percentages_valid() {
        let cfg = ConsensusConfig::default();
        assert!(cfg.slashing_double_sign_pct <= 100);
        assert!(cfg.slashing_offline_pct <= 100);
    }

    #[test]
    fn serialization_roundtrip() {
        let cfg = ConsensusConfig::default();
        let json = serde_json::to_string(&cfg).expect("serialize");
        let restored: ConsensusConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored.max_batch_size_bytes, cfg.max_batch_size_bytes);
        assert_eq!(restored.epoch_length_rounds, cfg.epoch_length_rounds);
    }
}
