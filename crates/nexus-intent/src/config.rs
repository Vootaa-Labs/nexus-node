// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Intent layer configuration.
//!
//! [`IntentConfig`] controls timeouts, size limits, cache policies,
//! and AI agent constraints for the intent compilation pipeline.

use nexus_primitives::Amount;
use serde::{Deserialize, Serialize};

/// Configuration for the intent layer.
///
/// All values have sensible defaults via [`IntentConfig::default()`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IntentConfig {
    /// Maximum intent payload size in bytes (default: 65,536 = 64 KiB).
    pub max_intent_size_bytes: usize,

    /// Maximum time to compile a single intent, in milliseconds (default: 500).
    pub compile_timeout_ms: u64,

    /// Maximum execution steps in a single compiled plan (default: 16).
    pub max_steps_per_intent: usize,

    /// LRU cache size for gas estimations (default: 10,000 entries).
    pub gas_estimate_cache_size: usize,

    /// Maximum gas budget for a single AI agent action (default: 1,000,000).
    pub agent_max_gas_budget: u64,

    /// Maximum value an AI agent can transfer per action (default: 1,000,000 units).
    pub agent_max_value_per_action: Amount,

    /// How often to refresh the contract registry cache, in ms (default: 10,000).
    pub contract_registry_refresh_ms: u64,

    /// Nonce tracking cache size (default: 100,000 accounts).
    pub nonce_cache_size: usize,

    /// Actor mailbox capacity (default: 256 pending intents).
    pub mailbox_capacity: usize,

    /// HTLC timeout duration in epochs for cross-shard transfers (default: 10).
    pub htlc_timeout_epochs: u64,
}

impl Default for IntentConfig {
    fn default() -> Self {
        Self {
            max_intent_size_bytes: 64 * 1024,
            compile_timeout_ms: 500,
            max_steps_per_intent: 16,
            gas_estimate_cache_size: 10_000,
            agent_max_gas_budget: 1_000_000,
            agent_max_value_per_action: Amount(1_000_000),
            contract_registry_refresh_ms: 10_000,
            nonce_cache_size: 100_000,
            mailbox_capacity: 256,
            htlc_timeout_epochs: 10,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_sane() {
        let cfg = IntentConfig::default();
        assert_eq!(cfg.max_intent_size_bytes, 64 * 1024);
        assert_eq!(cfg.compile_timeout_ms, 500);
        assert_eq!(cfg.max_steps_per_intent, 16);
        assert_eq!(cfg.gas_estimate_cache_size, 10_000);
        assert_eq!(cfg.agent_max_gas_budget, 1_000_000);
        assert_eq!(cfg.agent_max_value_per_action, Amount(1_000_000));
        assert_eq!(cfg.contract_registry_refresh_ms, 10_000);
        assert_eq!(cfg.nonce_cache_size, 100_000);
        assert_eq!(cfg.mailbox_capacity, 256);
    }

    #[test]
    fn config_json_round_trip() {
        let cfg = IntentConfig::default();
        let json = serde_json::to_string(&cfg).unwrap();
        let decoded: IntentConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, decoded);
    }

    #[test]
    fn config_custom_values() {
        let cfg = IntentConfig {
            max_intent_size_bytes: 128 * 1024,
            compile_timeout_ms: 1000,
            max_steps_per_intent: 32,
            gas_estimate_cache_size: 50_000,
            agent_max_gas_budget: 5_000_000,
            agent_max_value_per_action: Amount(10_000_000),
            contract_registry_refresh_ms: 30_000,
            nonce_cache_size: 500_000,
            mailbox_capacity: 512,
            htlc_timeout_epochs: 20,
        };
        assert_eq!(cfg.max_intent_size_bytes, 128 * 1024);
        assert_eq!(cfg.mailbox_capacity, 512);
    }
}
