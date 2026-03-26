//! Storage subsystem configuration.
//!
//! [`StorageConfig`] controls RocksDB paths, cache sizes, compression,
//! and epoch retention for the nexus-storage crate.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Configuration for the Nexus storage subsystem.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    /// Path to the RocksDB data directory.
    pub rocksdb_path: PathBuf,

    /// RocksDB block cache size in MiB.
    pub rocksdb_cache_size_mb: usize,

    /// Maximum number of open file descriptors for RocksDB.
    pub rocksdb_max_open_files: i32,

    /// RocksDB write buffer (memtable) size in MiB.
    pub rocksdb_write_buffer_size_mb: usize,

    /// Number of hot state-commitment tree nodes to cache in memory.
    #[serde(alias = "verkle_cache_size")]
    pub commitment_cache_size: usize,

    /// Whether the BLAKE3 backup tree is enabled.
    pub backup_tree_enabled: bool,

    /// ZK proof LRU cache capacity.
    pub proof_cache_capacity: usize,

    /// Number of past epochs to retain consensus certificates.
    pub epoch_retention_count: u64,

    /// Enable automatic data pruning of historical blocks/transactions/receipts.
    pub pruning_enabled: bool,

    /// Number of most recent blocks to retain (older data is pruned).
    /// Only effective when `pruning_enabled` is `true`.
    pub pruning_retention_blocks: u64,

    /// Interval in seconds between automatic pruning runs.
    pub pruning_interval_secs: u64,

    /// Directory for checkpoint/snapshot output. Defaults to `<rocksdb_path>/../snapshots`.
    pub snapshot_dir: Option<PathBuf>,
}

/// Production defaults per TLD-02 §8.
impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            rocksdb_path: PathBuf::from("data/db"),
            rocksdb_cache_size_mb: 512,
            rocksdb_max_open_files: 1024,
            rocksdb_write_buffer_size_mb: 128,
            commitment_cache_size: 100_000,
            backup_tree_enabled: true,
            proof_cache_capacity: 10_000,
            epoch_retention_count: 100,
            pruning_enabled: false,
            pruning_retention_blocks: 100_000,
            pruning_interval_secs: 3600,
            snapshot_dir: None,
        }
    }
}

impl StorageConfig {
    /// Minimal configuration suitable for unit and integration tests.
    ///
    /// Uses a caller-provided temporary directory and small cache sizes.
    pub fn for_testing(tmp_path: PathBuf) -> Self {
        Self {
            rocksdb_path: tmp_path,
            rocksdb_cache_size_mb: 8,
            rocksdb_max_open_files: 64,
            rocksdb_write_buffer_size_mb: 4,
            commitment_cache_size: 100,
            backup_tree_enabled: false,
            proof_cache_capacity: 100,
            epoch_retention_count: 2,
            pruning_enabled: false,
            pruning_retention_blocks: 100,
            pruning_interval_secs: 60,
            snapshot_dir: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_values() {
        let cfg = StorageConfig::default();
        assert_eq!(cfg.rocksdb_cache_size_mb, 512);
        assert_eq!(cfg.rocksdb_max_open_files, 1024);
        assert_eq!(cfg.rocksdb_write_buffer_size_mb, 128);
        assert!(cfg.backup_tree_enabled);
        assert_eq!(cfg.epoch_retention_count, 100);
    }

    #[test]
    fn testing_config() {
        let cfg = StorageConfig::for_testing(PathBuf::from("/tmp/test-db"));
        assert_eq!(cfg.rocksdb_cache_size_mb, 8);
        assert!(!cfg.backup_tree_enabled);
        assert_eq!(cfg.epoch_retention_count, 2);
    }

    #[test]
    fn config_serialization_roundtrip() {
        let cfg = StorageConfig::default();
        let json = serde_json::to_string(&cfg).unwrap();
        let decoded: StorageConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.rocksdb_cache_size_mb, cfg.rocksdb_cache_size_mb);
    }
}
