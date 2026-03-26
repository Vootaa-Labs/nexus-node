// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Storage-layer test fixtures: temp stores, KV helpers.

use std::path::PathBuf;

use nexus_storage::config::StorageConfig;
use nexus_storage::memory::MemoryStore;
use nexus_storage::types::ColumnFamily;

/// Create an in-memory [`MemoryStore`] ready for test use.
pub fn make_memory_store() -> MemoryStore {
    MemoryStore::new()
}

/// Create a temporary directory and return a [`StorageConfig`] pointing to it.
///
/// The caller owns the returned [`tempfile::TempDir`] handle — dropping it
/// removes the temporary directory.
pub fn make_temp_storage_config() -> (StorageConfig, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("failed to create tempdir");
    let cfg = StorageConfig::for_testing(tmp.path().to_path_buf());
    (cfg, tmp)
}

/// Return a list of all defined [`ColumnFamily`] variants.
pub fn all_column_families() -> Vec<ColumnFamily> {
    vec![
        ColumnFamily::Blocks,
        ColumnFamily::Transactions,
        ColumnFamily::Receipts,
        ColumnFamily::State,
        ColumnFamily::Certificates,
        ColumnFamily::Batches,
        ColumnFamily::Sessions,
        ColumnFamily::Provenance,
        ColumnFamily::CommitmentMeta,
        ColumnFamily::CommitmentLeaves,
        ColumnFamily::CommitmentNodes,
    ]
}

/// Create a deterministic KV pair from an index.
///
/// Key = `b"key-{index}"`, Value = `b"value-{index}"`.
pub fn make_kv_pair(index: u32) -> (Vec<u8>, Vec<u8>) {
    (
        format!("key-{index}").into_bytes(),
        format!("value-{index}").into_bytes(),
    )
}

/// Generate `n` distinct KV pairs suitable for batch write tests.
pub fn make_kv_pairs(n: u32) -> Vec<(Vec<u8>, Vec<u8>)> {
    (0..n).map(make_kv_pair).collect()
}

/// Return a test-suitable RocksDB path inside the system temp directory.
///
/// The path includes a random suffix via the process ID and a counter.
/// **Note**: The directory is NOT automatically cleaned up.
pub fn make_temp_db_path(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!("nexus-test-{label}-{}", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_created() {
        let _store = make_memory_store();
    }

    #[test]
    fn temp_config_valid() {
        let (cfg, _tmp) = make_temp_storage_config();
        assert!(cfg.rocksdb_path.exists());
        assert_eq!(cfg.rocksdb_cache_size_mb, 8);
    }

    #[test]
    fn kv_pairs_distinct() {
        let pairs = make_kv_pairs(10);
        assert_eq!(pairs.len(), 10);
        for i in 0..pairs.len() {
            for j in (i + 1)..pairs.len() {
                assert_ne!(pairs[i].0, pairs[j].0);
            }
        }
    }

    #[test]
    fn all_cfs_count() {
        assert_eq!(all_column_families().len(), 11);
    }
}
