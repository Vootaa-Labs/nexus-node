//! Intent engine configuration.
//!
//! Controls intent compilation limits, AI-agent gas budgets,
//! caching sizes, and contract registry refresh intervals.

use serde::{Deserialize, Serialize};

/// Configuration for the Nexus intent layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct IntentConfig {
    /// Maximum serialized intent size in bytes. Default: 65_536 (64 KiB).
    pub max_intent_size_bytes: usize,

    /// Time limit (ms) for compiling a single intent. Default: 500.
    pub compile_timeout_ms: u64,

    /// Maximum number of execution steps per intent. Default: 16.
    pub max_steps_per_intent: usize,

    /// LRU cache capacity for gas estimates. Default: 10_000.
    pub gas_estimate_cache_size: usize,

    /// Maximum gas budget an AI-agent may spend per action. Default: 1_000_000.
    pub agent_max_gas_budget: u64,

    /// Maximum token value an AI-agent may transfer per action (in base units).
    /// Default: 1_000_000_000_000 (10^12).
    pub agent_max_value_per_action: u64,

    /// Interval (ms) for refreshing the on-chain contract registry. Default: 10_000.
    pub contract_registry_refresh_ms: u64,

    /// LRU cache capacity for transaction nonces. Default: 100_000.
    pub nonce_cache_size: usize,
}

impl Default for IntentConfig {
    fn default() -> Self {
        Self {
            max_intent_size_bytes: 65_536,
            compile_timeout_ms: 500,
            max_steps_per_intent: 16,
            gas_estimate_cache_size: 10_000,
            agent_max_gas_budget: 1_000_000,
            agent_max_value_per_action: 1_000_000_000_000u64,
            contract_registry_refresh_ms: 10_000,
            nonce_cache_size: 100_000,
        }
    }
}

impl IntentConfig {
    /// Minimal configuration suitable for tests.
    pub fn for_testing() -> Self {
        Self {
            max_intent_size_bytes: 4096,
            compile_timeout_ms: 100,
            max_steps_per_intent: 4,
            gas_estimate_cache_size: 100,
            agent_max_gas_budget: 10_000,
            agent_max_value_per_action: 1_000_000u64,
            contract_registry_refresh_ms: 1_000,
            nonce_cache_size: 100,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_values() {
        let cfg = IntentConfig::default();
        assert_eq!(cfg.max_intent_size_bytes, 65_536);
        assert_eq!(cfg.compile_timeout_ms, 500);
        assert_eq!(cfg.max_steps_per_intent, 16);
        assert_eq!(cfg.gas_estimate_cache_size, 10_000);
        assert_eq!(cfg.agent_max_gas_budget, 1_000_000);
        assert_eq!(cfg.nonce_cache_size, 100_000);
    }

    #[test]
    fn testing_config_is_small() {
        let cfg = IntentConfig::for_testing();
        assert!(cfg.max_intent_size_bytes < 65_536);
        assert!(cfg.max_steps_per_intent < 16);
        assert!(cfg.nonce_cache_size < 1000);
    }

    #[test]
    fn serialization_roundtrip() {
        let cfg = IntentConfig::default();
        let json = serde_json::to_string(&cfg).expect("serialize");
        let restored: IntentConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored.max_intent_size_bytes, cfg.max_intent_size_bytes);
        assert_eq!(
            restored.agent_max_value_per_action,
            cfg.agent_max_value_per_action
        );
    }
}
