// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! VM configuration for the Move adapter.
//!
//! [`VmConfig`] is derived from [`ExecutionConfig`](nexus_config::ExecutionConfig)
//! and contains only the parameters relevant to Move VM execution.

/// Configuration for the Move VM adapter.
#[derive(Debug, Clone)]
pub(crate) struct VmConfig {
    /// Maximum compiled bytecode binary size in bytes.
    pub max_binary_size: usize,
    /// Base gas cost for a Move function call.
    pub call_base_gas: u64,
    /// Base gas cost for publishing modules.
    pub publish_base_gas: u64,
    /// Per-byte gas cost for storing module bytecode.
    pub publish_per_byte_gas: u64,
    /// Per-byte gas cost for reading from state.
    #[allow(dead_code)]
    pub read_per_byte_gas: u64,
    /// Per-byte gas cost for writing to state.
    #[allow(dead_code)]
    pub write_per_byte_gas: u64,
}

impl Default for VmConfig {
    fn default() -> Self {
        Self {
            max_binary_size: 524_288, // 512 KiB
            call_base_gas: 5_000,
            publish_base_gas: 10_000,
            publish_per_byte_gas: 1,
            read_per_byte_gas: 1,
            write_per_byte_gas: 5,
        }
    }
}

impl VmConfig {
    /// Create a VM config from the execution config.
    #[allow(dead_code)]
    pub fn from_execution_config(cfg: &nexus_config::ExecutionConfig) -> Self {
        Self {
            max_binary_size: cfg.move_vm_max_binary_size,
            ..Self::default()
        }
    }

    /// Minimal configuration suitable for unit tests.
    #[allow(dead_code)]
    pub fn for_testing() -> Self {
        Self {
            max_binary_size: 65_536,
            call_base_gas: 1_000,
            publish_base_gas: 2_000,
            publish_per_byte_gas: 1,
            read_per_byte_gas: 1,
            write_per_byte_gas: 5,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_values() {
        let cfg = VmConfig::default();
        assert_eq!(cfg.max_binary_size, 524_288);
        assert_eq!(cfg.call_base_gas, 5_000);
        assert_eq!(cfg.publish_base_gas, 10_000);
    }

    #[test]
    fn from_execution_config() {
        let ecfg = nexus_config::ExecutionConfig {
            move_vm_max_binary_size: 100_000,
            ..nexus_config::ExecutionConfig::default()
        };
        let vcfg = VmConfig::from_execution_config(&ecfg);
        assert_eq!(vcfg.max_binary_size, 100_000);
        // Other fields remain at defaults.
        assert_eq!(vcfg.call_base_gas, 5_000);
    }

    #[test]
    fn for_testing_values() {
        let cfg = VmConfig::for_testing();
        assert_eq!(cfg.max_binary_size, 65_536);
        assert_eq!(cfg.call_base_gas, 1_000);
    }
}
