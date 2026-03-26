// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Execution engine configuration.
//!
//! Parameters for Block-STM parallel execution, sharding, gas limits,
//! and optional VM features (WASM, ZK proofs).

use serde::{Deserialize, Serialize};

/// Configuration for the Nexus execution layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ExecutionConfig {
    /// Initial number of execution shards. Default: 4.
    pub shard_count: u16,

    /// Hard ceiling on shard count. Default: 64.
    pub max_shard_count: u16,

    /// Number of threads for Block-STM parallel execution.
    /// Default: number of available CPUs.
    pub block_stm_threads: usize,

    /// Maximum transaction re-executions inside Block-STM. Default: 5.
    pub block_stm_max_retries: usize,

    /// Maximum gas consumable per block. Default: 10_000_000.
    pub max_block_gas: u64,

    /// HTLC expiry duration in epochs. Default: 10.
    pub htlc_timeout_epochs: u64,

    /// Maximum Move bytecode binary size in bytes. Default: 524_288 (512 KiB).
    pub move_vm_max_binary_size: usize,

    /// Whether the WASM execution backend is enabled. Default: false.
    pub wasm_enabled: bool,

    /// Whether zero-knowledge proof verification is enabled. Default: false.
    pub zk_proof_enabled: bool,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            shard_count: 4,
            max_shard_count: 64,
            block_stm_threads: num_cpus(),
            block_stm_max_retries: 5,
            max_block_gas: 10_000_000,
            htlc_timeout_epochs: 10,
            move_vm_max_binary_size: 524_288,
            wasm_enabled: false,
            zk_proof_enabled: false,
        }
    }
}

impl ExecutionConfig {
    /// Minimal configuration suitable for tests.
    pub fn for_testing() -> Self {
        Self {
            shard_count: 1,
            max_shard_count: 4,
            block_stm_threads: 2,
            block_stm_max_retries: 2,
            max_block_gas: 1_000_000,
            htlc_timeout_epochs: 2,
            move_vm_max_binary_size: 65_536,
            wasm_enabled: false,
            zk_proof_enabled: false,
        }
    }
}

/// Returns the number of available CPUs, falling back to 1.
fn num_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_values() {
        let cfg = ExecutionConfig::default();
        assert_eq!(cfg.shard_count, 4);
        assert_eq!(cfg.max_shard_count, 64);
        assert!(cfg.block_stm_threads >= 1);
        assert_eq!(cfg.max_block_gas, 10_000_000);
        assert!(!cfg.wasm_enabled);
        assert!(!cfg.zk_proof_enabled);
    }

    #[test]
    fn shard_invariant() {
        let cfg = ExecutionConfig::default();
        assert!(cfg.shard_count <= cfg.max_shard_count);
    }

    #[test]
    fn testing_config_is_small() {
        let cfg = ExecutionConfig::for_testing();
        assert_eq!(cfg.shard_count, 1);
        assert_eq!(cfg.block_stm_threads, 2);
        assert!(cfg.max_block_gas < 10_000_000);
    }

    #[test]
    fn serialization_roundtrip() {
        let cfg = ExecutionConfig::default();
        let json = serde_json::to_string(&cfg).expect("serialize");
        let restored: ExecutionConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored.shard_count, cfg.shard_count);
        assert_eq!(restored.max_block_gas, cfg.max_block_gas);
    }
}
