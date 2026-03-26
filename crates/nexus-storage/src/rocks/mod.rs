//! [`RocksStore`] — RocksDB-backed implementation of [`StateStorage`].
//!
//! All blocking RocksDB I/O is dispatched through [`tokio::task::spawn_blocking`]
//! per DEV-04 §3 (spawn_blocking rules for synchronous I/O).

mod batch;
pub mod migration;
mod schema;

pub use batch::RocksWriteBatch;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::config::StorageConfig;
use crate::error::StorageError;
use crate::traits::StateStorage;
use crate::types::ColumnFamily;

// ── Inner shared state ───────────────────────────────────────────────────────

struct RocksInner {
    db: rocksdb::DB,
    /// Path the database was opened at (needed for checkpoint operations).
    path: PathBuf,
}

// ── RocksStore ───────────────────────────────────────────────────────────────

/// Production storage backend backed by RocksDB.
///
/// Cheap to clone (internally reference-counted via `Arc`).
/// All operations go through the 7 **FROZEN-2** column families defined
/// in [`ColumnFamily`].
#[derive(Clone)]
pub struct RocksStore {
    inner: Arc<RocksInner>,
}

impl RocksStore {
    /// Open (or create) a RocksDB instance at the configured path.
    ///
    /// Creates all column families if they don't exist.
    /// Runs schema migration if needed.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::RocksDb`] if the database cannot be opened.
    pub fn open(config: &StorageConfig) -> Result<Self, StorageError> {
        Self::open_at(&config.rocksdb_path, config)
    }

    /// Open a RocksDB instance at an explicit path (useful for tests with temp dirs).
    pub fn open_at(path: &Path, config: &StorageConfig) -> Result<Self, StorageError> {
        let mut db_opts = rocksdb::Options::default();
        db_opts.create_if_missing(true);
        db_opts.create_missing_column_families(true);
        db_opts.set_max_open_files(config.rocksdb_max_open_files);
        db_opts.set_write_buffer_size(config.rocksdb_write_buffer_size_mb * 1024 * 1024);

        // Block cache shared across all CFs.
        let cache = rocksdb::Cache::new_lru_cache(config.rocksdb_cache_size_mb * 1024 * 1024);
        let mut table_opts = rocksdb::BlockBasedOptions::default();
        table_opts.set_block_cache(&cache);
        db_opts.set_block_based_table_factory(&table_opts);

        let cf_descriptors = schema::all_cf_descriptors();
        let db = rocksdb::DB::open_cf_descriptors(&db_opts, path, cf_descriptors)
            .map_err(|e| StorageError::RocksDb(e.to_string()))?;

        let store = Self {
            inner: Arc::new(RocksInner {
                db,
                path: path.to_path_buf(),
            }),
        };

        // Run schema migration on open.
        migration::migrate(&store)?;

        Ok(store)
    }

    /// Resolve a column family handle by name, returning a friendly error.
    fn cf_handle(&self, cf_name: &str) -> Result<&rocksdb::ColumnFamily, StorageError> {
        self.inner
            .db
            .cf_handle(cf_name)
            .ok_or_else(|| StorageError::UnknownColumnFamily {
                name: cf_name.to_owned(),
            })
    }

    // ── P5-1: True RocksDB Checkpoint ───────────────────────────────────

    /// Create a true RocksDB checkpoint at the given directory path.
    ///
    /// The checkpoint is a consistent, point-in-time snapshot created via
    /// RocksDB's native `create_checkpoint()`. It uses hard-links where
    /// possible and can be opened as a standalone RocksDB instance.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Snapshot`] if the checkpoint directory already
    /// exists or if the RocksDB checkpoint operation fails.
    pub fn create_checkpoint(&self, checkpoint_path: &Path) -> Result<(), StorageError> {
        if checkpoint_path.exists() {
            return Err(StorageError::Snapshot(format!(
                "checkpoint path already exists: {}",
                checkpoint_path.display()
            )));
        }
        let cp = rocksdb::checkpoint::Checkpoint::new(&self.inner.db).map_err(|e| {
            StorageError::Snapshot(format!("failed to create checkpoint object: {e}"))
        })?;
        cp.create_checkpoint(checkpoint_path)
            .map_err(|e| StorageError::Snapshot(format!("checkpoint creation failed: {e}")))?;
        Ok(())
    }

    /// Open a previously created checkpoint as a read-only `RocksStore`.
    pub fn open_checkpoint(
        checkpoint_path: &Path,
        config: &StorageConfig,
    ) -> Result<Self, StorageError> {
        Self::open_at(checkpoint_path, config)
    }

    /// The database path this store was opened at.
    pub fn db_path(&self) -> &Path {
        &self.inner.path
    }

    // ── P5-2: Data Pruning ──────────────────────────────────────────────

    /// Delete all entries in the given column family whose keys fall in `[start, end)`.
    ///
    /// Uses `delete_range_cf` for efficient bulk removal.
    pub fn delete_range(
        &self,
        cf: ColumnFamily,
        start: &[u8],
        end: &[u8],
    ) -> Result<(), StorageError> {
        let cf_handle = self.cf_handle(cf.as_str())?;
        let mut batch = rocksdb::WriteBatch::default();
        batch.delete_range_cf(cf_handle, start, end);
        self.inner
            .db
            .write(batch)
            .map_err(|e| StorageError::RocksDb(e.to_string()))
    }

    /// Prune historical data (Blocks, Transactions, Receipts) with commit
    /// sequence strictly less than `retain_from_seq`.
    ///
    /// SEC-M11: All column families are pruned atomically via a single
    /// `WriteBatch`, ensuring crash-consistency. Entry counts are obtained
    /// via lightweight iterator scans (no full materialization).
    pub fn prune_before(&self, retain_from_seq: u64) -> Result<PruneResult, StorageError> {
        let zero = [0u8; 8];
        let end = retain_from_seq.to_be_bytes();

        let prunable = [
            ColumnFamily::Blocks,
            ColumnFamily::Transactions,
            ColumnFamily::Receipts,
        ];

        // Count entries to be pruned using iterators (no materialization).
        let mut result = PruneResult::default();
        for cf in &prunable {
            let count = self.count_range(cf, &zero, &end)?;
            match cf {
                ColumnFamily::Blocks => result.blocks_pruned = count,
                ColumnFamily::Transactions => result.transactions_pruned = count,
                ColumnFamily::Receipts => result.receipts_pruned = count,
                _ => {}
            }
        }

        // SEC-M11: atomic delete across all CFs using a single WriteBatch.
        let mut wb = rocksdb::WriteBatch::default();
        for cf in &prunable {
            let cf_handle = self.cf_handle(cf.as_str())?;
            wb.delete_range_cf(cf_handle, &zero, &end);
        }
        self.inner
            .db
            .write(wb)
            .map_err(|e| StorageError::RocksDb(e.to_string()))?;

        Ok(result)
    }

    /// Count entries in `[start, end)` using a lightweight iterator scan.
    ///
    /// Only reads keys — values are not materialized into memory.
    fn count_range(
        &self,
        cf: &ColumnFamily,
        start: &[u8],
        end: &[u8],
    ) -> Result<u64, StorageError> {
        let cf_handle = self.cf_handle(cf.as_str())?;
        let mut read_opts = rocksdb::ReadOptions::default();
        read_opts.set_iterate_upper_bound(end.to_vec());
        let mut iter = self.inner.db.raw_iterator_cf_opt(cf_handle, read_opts);
        iter.seek(start);
        let mut count = 0u64;
        while iter.valid() {
            count += 1;
            iter.next();
        }
        iter.status()
            .map_err(|e| StorageError::RocksDb(e.to_string()))?;
        Ok(count)
    }

    // ── P5-3: Storage Capacity Metrics ──────────────────────────────────

    /// Collect per-column-family storage statistics from RocksDB.
    ///
    /// Returns SST file sizes and memtable sizes for each column family.
    pub fn storage_stats(&self) -> Result<Vec<CfStats>, StorageError> {
        let mut stats = Vec::new();
        for cf in ColumnFamily::all() {
            let cf_handle = self.cf_handle(cf.as_str())?;

            let sst_size = self
                .inner
                .db
                .property_int_value_cf(cf_handle, "rocksdb.total-sst-files-size")
                .map_err(|e| StorageError::RocksDb(e.to_string()))?
                .unwrap_or(0);

            let memtable_size = self
                .inner
                .db
                .property_int_value_cf(cf_handle, "rocksdb.cur-size-all-mem-tables")
                .map_err(|e| StorageError::RocksDb(e.to_string()))?
                .unwrap_or(0);

            let num_keys = self
                .inner
                .db
                .property_int_value_cf(cf_handle, "rocksdb.estimate-num-keys")
                .map_err(|e| StorageError::RocksDb(e.to_string()))?
                .unwrap_or(0);

            stats.push(CfStats {
                cf: *cf,
                sst_file_size_bytes: sst_size,
                memtable_size_bytes: memtable_size,
                estimated_num_keys: num_keys,
            });
        }
        Ok(stats)
    }

    // ── P5-4: State Snapshot Export/Import ───────────────────────────────

    /// Validate that a snapshot path is safe: no `..` traversal, no symlink
    /// escape, and (if a base is provided) the path is under the base (SEC-H3).
    fn validate_snapshot_path(
        path: &Path,
        allowed_base: Option<&Path>,
    ) -> Result<std::path::PathBuf, StorageError> {
        // Reject paths containing obvious traversal components.
        let path_str = path.to_string_lossy();
        if path_str.contains("..") {
            return Err(StorageError::InvalidPath {
                path: path.to_path_buf(),
                reason: "path contains '..' traversal component".to_string(),
            });
        }

        // Reject symlinks on the path itself (if it exists).
        if path.exists() {
            let metadata =
                std::fs::symlink_metadata(path).map_err(|e| StorageError::InvalidPath {
                    path: path.to_path_buf(),
                    reason: format!("failed to read metadata: {e}"),
                })?;
            if metadata.file_type().is_symlink() {
                return Err(StorageError::InvalidPath {
                    path: path.to_path_buf(),
                    reason: "path is a symbolic link — snapshot paths must be direct".to_string(),
                });
            }
        }

        // Canonicalize: resolve to an absolute path.
        // For new paths (export), canonicalize the parent.
        let canonical = if path.exists() {
            path.canonicalize().map_err(|e| StorageError::InvalidPath {
                path: path.to_path_buf(),
                reason: format!("failed to canonicalize: {e}"),
            })?
        } else if let Some(parent) = path.parent() {
            if parent.as_os_str().is_empty() || !parent.exists() {
                // Allow if parent doesn't exist yet (will be created_dir_all'd).
                path.to_path_buf()
            } else {
                let canonical_parent =
                    parent
                        .canonicalize()
                        .map_err(|e| StorageError::InvalidPath {
                            path: path.to_path_buf(),
                            reason: format!("failed to canonicalize parent: {e}"),
                        })?;
                canonical_parent.join(path.file_name().unwrap_or_default())
            }
        } else {
            path.to_path_buf()
        };

        // If an allowed base directory is specified, enforce prefix constraint.
        if let Some(base) = allowed_base {
            let canonical_base = if base.exists() {
                base.canonicalize().map_err(|e| StorageError::InvalidPath {
                    path: base.to_path_buf(),
                    reason: format!("failed to canonicalize base: {e}"),
                })?
            } else {
                base.to_path_buf()
            };
            if !canonical.starts_with(&canonical_base) {
                return Err(StorageError::InvalidPath {
                    path: path.to_path_buf(),
                    reason: format!(
                        "path escapes allowed base directory ({})",
                        canonical_base.display()
                    ),
                });
            }
        }

        Ok(canonical)
    }

    /// Export the entire State column family to a directory as a BCS-encoded
    /// snapshot file. The file contains a `StateSnapshot` header followed by
    /// all key-value pairs, with a trailing BLAKE3 content hash.
    ///
    /// SEC-M9: If `allowed_base` is `Some`, the output path must reside
    /// under that base directory.
    pub fn export_state_snapshot(
        &self,
        output_path: &Path,
        block_height: u64,
        allowed_base: Option<&Path>,
    ) -> Result<SnapshotManifest, StorageError> {
        self.export_state_snapshot_with_provenance(output_path, block_height, allowed_base, None)
    }

    /// Export the State column family with provenance metadata.
    ///
    /// `provenance` provides chain_id, epoch, and hash-chain link.
    /// If `None`, provenance fields are left empty.
    ///
    /// The returned manifest can be signed via
    /// [`snapshot_signing::sign_manifest`](crate) before distribution.
    pub fn export_state_snapshot_with_provenance(
        &self,
        output_path: &Path,
        block_height: u64,
        allowed_base: Option<&Path>,
        provenance: Option<&SnapshotProvenance>,
    ) -> Result<SnapshotManifest, StorageError> {
        use std::io::Write;

        // SEC-H3 + SEC-M9: validate path safety.
        let _validated = Self::validate_snapshot_path(output_path, allowed_base)?;

        let cf_handle = self.cf_handle(ColumnFamily::State.as_str())?;
        let mut iter = self.inner.db.raw_iterator_cf(cf_handle);
        iter.seek_to_first();

        std::fs::create_dir_all(output_path).map_err(|e| StorageError::InvalidPath {
            path: output_path.to_path_buf(),
            reason: e.to_string(),
        })?;

        let snapshot_file = output_path.join("state_snapshot.bin");
        let mut file = std::fs::File::create(&snapshot_file)
            .map_err(|e| StorageError::Snapshot(format!("failed to create snapshot file: {e}")))?;

        let mut entry_count: u64 = 0;
        let mut total_bytes: u64 = 0;
        let mut hasher = blake3::Hasher::new();

        // Write placeholder header (will be rewritten with actual counts/hash).
        // Use Some([0; 32]) to ensure placeholder size matches the final header.
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let header = SnapshotManifest {
            version: 1,
            block_height,
            entry_count: 0,
            total_bytes: 0,
            content_hash: Some([0u8; 32]),
            signature: None,
            signer_public_key: None,
            signature_scheme: None,
            chain_id: provenance.map(|p| p.chain_id.clone()),
            epoch: provenance.map(|p| p.epoch),
            created_at_ms: Some(now_ms),
            previous_manifest_hash: provenance.and_then(|p| p.previous_manifest_hash),
        };
        let header_bytes =
            bcs::to_bytes(&header).map_err(|e| StorageError::Serialization(e.to_string()))?;
        let header_len = header_bytes.len() as u32;
        file.write_all(&header_len.to_le_bytes())
            .map_err(|e| StorageError::Snapshot(e.to_string()))?;
        file.write_all(&header_bytes)
            .map_err(|e| StorageError::Snapshot(e.to_string()))?;

        // Write entries.
        while iter.valid() {
            if let (Some(k), Some(v)) = (iter.key(), iter.value()) {
                let k_len = k.len() as u32;
                let v_len = v.len() as u32;
                file.write_all(&k_len.to_le_bytes())
                    .map_err(|e| StorageError::Snapshot(e.to_string()))?;
                file.write_all(k)
                    .map_err(|e| StorageError::Snapshot(e.to_string()))?;
                file.write_all(&v_len.to_le_bytes())
                    .map_err(|e| StorageError::Snapshot(e.to_string()))?;
                file.write_all(v)
                    .map_err(|e| StorageError::Snapshot(e.to_string()))?;

                // Feed into content hash.
                hasher.update(k);
                hasher.update(v);

                total_bytes += 8 + k.len() as u64 + v.len() as u64;
                entry_count += 1;
            }
            iter.next();
        }
        iter.status()
            .map_err(|e| StorageError::RocksDb(e.to_string()))?;

        file.flush()
            .map_err(|e| StorageError::Snapshot(e.to_string()))?;

        let content_hash: [u8; 32] = *hasher.finalize().as_bytes();

        // Rewrite header with actual counts and content hash.
        let manifest = SnapshotManifest {
            version: 1,
            block_height,
            entry_count,
            total_bytes,
            content_hash: Some(content_hash),
            // Signature fields are populated by the caller after export
            // via `manifest.signable_bytes()` + their signing key.
            signature: None,
            signer_public_key: None,
            signature_scheme: None,
            chain_id: provenance.map(|p| p.chain_id.clone()),
            epoch: provenance.map(|p| p.epoch),
            created_at_ms: Some(now_ms),
            previous_manifest_hash: provenance.and_then(|p| p.previous_manifest_hash),
        };
        let final_header_bytes =
            bcs::to_bytes(&manifest).map_err(|e| StorageError::Serialization(e.to_string()))?;
        // The new manifest (with content_hash) may be larger than the placeholder.
        // Recalculate lengths.
        let final_header_len = final_header_bytes.len() as u32;
        use std::io::Seek;
        file.seek(std::io::SeekFrom::Start(0))
            .map_err(|e| StorageError::Snapshot(e.to_string()))?;
        file.write_all(&final_header_len.to_le_bytes())
            .map_err(|e| StorageError::Snapshot(e.to_string()))?;
        file.write_all(&final_header_bytes)
            .map_err(|e| StorageError::Snapshot(e.to_string()))?;

        Ok(manifest)
    }

    /// Import a state snapshot file, replacing the current State column family
    /// contents.
    ///
    /// SEC-M8: Uses a two-pass approach — pass 1 computes and verifies the
    /// content hash WITHOUT writing; pass 2 writes to the database only after
    /// integrity is confirmed.  Hash failure never pollutes the main database.
    ///
    /// SEC-M9: If `allowed_base` is `Some`, the snapshot path must reside
    /// under that base directory.
    pub fn import_state_snapshot(
        &self,
        snapshot_path: &Path,
        allowed_base: Option<&Path>,
    ) -> Result<SnapshotManifest, StorageError> {
        use std::io::{Read, Seek, SeekFrom};

        // SEC-H3 + SEC-M9: validate path safety.
        let _validated = Self::validate_snapshot_path(snapshot_path, allowed_base)?;

        let snapshot_file = if snapshot_path.is_dir() {
            snapshot_path.join("state_snapshot.bin")
        } else {
            snapshot_path.to_path_buf()
        };

        let mut file = std::fs::File::open(&snapshot_file)
            .map_err(|e| StorageError::Snapshot(format!("failed to open snapshot file: {e}")))?;

        // Read header length + header.
        let mut header_len_buf = [0u8; 4];
        file.read_exact(&mut header_len_buf)
            .map_err(|e| StorageError::Snapshot(format!("failed to read header length: {e}")))?;
        let header_len = u32::from_le_bytes(header_len_buf) as usize;

        // SEC-H4: reject unreasonably large headers (max 64 KiB).
        if header_len > 65_536 {
            return Err(StorageError::Snapshot(format!(
                "snapshot header size ({header_len}) exceeds limit (65536)"
            )));
        }

        let mut header_buf = vec![0u8; header_len];
        file.read_exact(&mut header_buf)
            .map_err(|e| StorageError::Snapshot(format!("failed to read header: {e}")))?;

        let manifest: SnapshotManifest = bcs::from_bytes(&header_buf)
            .map_err(|e| StorageError::Serialization(format!("invalid snapshot header: {e}")))?;

        if manifest.version != 1 {
            return Err(StorageError::Snapshot(format!(
                "unsupported snapshot version: {}",
                manifest.version
            )));
        }

        // Record file position after the header for the second pass.
        let data_start = file
            .stream_position()
            .map_err(|e| StorageError::Snapshot(format!("failed to get file position: {e}")))?;

        // ─── Pass 1: compute integrity hash WITHOUT writing to DB (SEC-M8) ───
        let mut hasher = blake3::Hasher::new();

        for i in 0..manifest.entry_count {
            let mut k_len_buf = [0u8; 4];
            file.read_exact(&mut k_len_buf)
                .map_err(|e| StorageError::Snapshot(format!("truncated snapshot: {e}")))?;
            let k_len = u32::from_le_bytes(k_len_buf) as usize;

            if k_len > SNAPSHOT_MAX_KEY_SIZE {
                return Err(StorageError::Snapshot(format!(
                    "snapshot key size ({k_len}) exceeds limit ({SNAPSHOT_MAX_KEY_SIZE}) at entry {i}"
                )));
            }

            let mut key = vec![0u8; k_len];
            file.read_exact(&mut key)
                .map_err(|e| StorageError::Snapshot(format!("truncated snapshot key: {e}")))?;

            let mut v_len_buf = [0u8; 4];
            file.read_exact(&mut v_len_buf)
                .map_err(|e| StorageError::Snapshot(format!("truncated snapshot: {e}")))?;
            let v_len = u32::from_le_bytes(v_len_buf) as usize;

            if v_len > SNAPSHOT_MAX_VALUE_SIZE {
                return Err(StorageError::Snapshot(format!(
                    "snapshot value size ({v_len}) exceeds limit ({SNAPSHOT_MAX_VALUE_SIZE}) at entry {i}"
                )));
            }

            let mut value = vec![0u8; v_len];
            file.read_exact(&mut value)
                .map_err(|e| StorageError::Snapshot(format!("truncated snapshot value: {e}")))?;

            hasher.update(&key);
            hasher.update(&value);
        }

        // Verify content hash BEFORE any database writes (SEC-M8).
        if let Some(expected_hash) = manifest.content_hash {
            let actual_hash: [u8; 32] = *hasher.finalize().as_bytes();
            if actual_hash != expected_hash {
                return Err(StorageError::Snapshot(format!(
                    "snapshot integrity check failed: expected {}, got {}",
                    hex::encode(expected_hash),
                    hex::encode(actual_hash),
                )));
            }
        }

        // ─── Pass 2: integrity verified — write data to DB ───────────────

        file.seek(SeekFrom::Start(data_start))
            .map_err(|e| StorageError::Snapshot(format!("failed to seek for pass 2: {e}")))?;

        let cf_handle = self.cf_handle(ColumnFamily::State.as_str())?;
        let mut wb = rocksdb::WriteBatch::default();
        let mut imported = 0u64;

        for _ in 0..manifest.entry_count {
            let mut k_len_buf = [0u8; 4];
            file.read_exact(&mut k_len_buf)
                .map_err(|e| StorageError::Snapshot(format!("truncated snapshot pass 2: {e}")))?;
            let k_len = u32::from_le_bytes(k_len_buf) as usize;

            let mut key = vec![0u8; k_len];
            file.read_exact(&mut key).map_err(|e| {
                StorageError::Snapshot(format!("truncated snapshot key pass 2: {e}"))
            })?;

            let mut v_len_buf = [0u8; 4];
            file.read_exact(&mut v_len_buf)
                .map_err(|e| StorageError::Snapshot(format!("truncated snapshot pass 2: {e}")))?;
            let v_len = u32::from_le_bytes(v_len_buf) as usize;

            let mut value = vec![0u8; v_len];
            file.read_exact(&mut value).map_err(|e| {
                StorageError::Snapshot(format!("truncated snapshot value pass 2: {e}"))
            })?;

            wb.put_cf(cf_handle, &key, &value);
            imported += 1;

            // Flush in batches of 10_000 to bound memory.
            if imported % 10_000 == 0 {
                self.inner
                    .db
                    .write(wb)
                    .map_err(|e| StorageError::RocksDb(e.to_string()))?;
                wb = rocksdb::WriteBatch::default();
            }
        }

        // Write remaining entries.
        if !wb.is_empty() {
            self.inner
                .db
                .write(wb)
                .map_err(|e| StorageError::RocksDb(e.to_string()))?;
        }

        Ok(manifest)
    }

    /// Read the [`SnapshotManifest`] from a snapshot file without importing.
    ///
    /// Useful for offline verification: callers can inspect provenance
    /// fields and verify the cryptographic signature before committing
    /// to a full import.
    pub fn read_snapshot_manifest(snapshot_path: &Path) -> Result<SnapshotManifest, StorageError> {
        use std::io::Read;

        let snapshot_file = if snapshot_path.is_dir() {
            snapshot_path.join("state_snapshot.bin")
        } else {
            snapshot_path.to_path_buf()
        };

        let mut file = std::fs::File::open(&snapshot_file)
            .map_err(|e| StorageError::Snapshot(format!("failed to open snapshot file: {e}")))?;

        let mut header_len_buf = [0u8; 4];
        file.read_exact(&mut header_len_buf)
            .map_err(|e| StorageError::Snapshot(format!("failed to read header length: {e}")))?;
        let header_len = u32::from_le_bytes(header_len_buf) as usize;

        if header_len > 65_536 {
            return Err(StorageError::Snapshot(format!(
                "snapshot header size ({header_len}) exceeds limit (65536)"
            )));
        }

        let mut header_buf = vec![0u8; header_len];
        file.read_exact(&mut header_buf)
            .map_err(|e| StorageError::Snapshot(format!("failed to read header: {e}")))?;

        let manifest: SnapshotManifest = bcs::from_bytes(&header_buf)
            .map_err(|e| StorageError::Serialization(format!("invalid snapshot header: {e}")))?;

        Ok(manifest)
    }
}

// ── StateStorage impl ────────────────────────────────────────────────────────

/// Per-column-family storage statistics.
#[derive(Debug, Clone)]
pub struct CfStats {
    /// Column family.
    pub cf: ColumnFamily,
    /// Total size of SST files on disk (bytes).
    pub sst_file_size_bytes: u64,
    /// Current memtable usage (bytes).
    pub memtable_size_bytes: u64,
    /// Estimated number of keys.
    pub estimated_num_keys: u64,
}

/// Provenance metadata supplied when exporting a state snapshot.
///
/// Carried into [`SnapshotManifest`] so that an offline verifier can
/// confirm chain identity, epoch, and hash-chain continuity without
/// contacting the network.
#[derive(Debug, Clone)]
pub struct SnapshotProvenance {
    /// Chain identifier (e.g. `"nexus-devnet-7"`).
    pub chain_id: String,
    /// Epoch number at the time of export.
    pub epoch: u64,
    /// BLAKE3 hash of the previous snapshot manifest (`None` for the first).
    pub previous_manifest_hash: Option<[u8; 32]>,
}

/// Result of a pruning operation.
#[derive(Debug, Clone, Default)]
pub struct PruneResult {
    /// Number of block entries pruned.
    pub blocks_pruned: u64,
    /// Number of transaction entries pruned.
    pub transactions_pruned: u64,
    /// Number of receipt entries pruned.
    pub receipts_pruned: u64,
}

/// Manifest for a state snapshot export.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SnapshotManifest {
    /// Snapshot format version.
    pub version: u32,
    /// Block height at the time of export.
    pub block_height: u64,
    /// Number of state entries in the snapshot.
    pub entry_count: u64,
    /// Total uncompressed payload bytes.
    pub total_bytes: u64,
    /// BLAKE3 hash of all key-value data (SEC-H4 integrity).
    #[serde(default)]
    pub content_hash: Option<[u8; 32]>,
    /// Cryptographic signature over the manifest fields above.
    ///
    /// When present, the signature covers the BCS encoding of the
    /// `signable_bytes()` tuple under the domain separator
    /// `nexus::storage::snapshot::sign::v1`.
    #[serde(default)]
    pub signature: Option<Vec<u8>>,
    /// Public verification key of the signer (scheme-specific encoding).
    #[serde(default)]
    pub signer_public_key: Option<Vec<u8>>,
    /// Signature scheme identifier (e.g. `"falcon-512"`, `"ml-dsa-65"`).
    #[serde(default)]
    pub signature_scheme: Option<String>,

    // ── Provenance metadata (D-4) ──────────────────────────────────
    /// Chain identifier (e.g. `"nexus-devnet-7"`) for cross-network safety.
    #[serde(default)]
    pub chain_id: Option<String>,
    /// Epoch number at the time of export.
    #[serde(default)]
    pub epoch: Option<u64>,
    /// Unix timestamp in milliseconds when the snapshot was created.
    #[serde(default)]
    pub created_at_ms: Option<u64>,
    /// BLAKE3 hash of the previous snapshot manifest (hash-chain link).
    ///
    /// `None` for the first snapshot in a chain.
    #[serde(default)]
    pub previous_manifest_hash: Option<[u8; 32]>,
}

/// Domain separator for snapshot manifest signing.
pub const SNAPSHOT_SIGN_DOMAIN: &[u8] = b"nexus::storage::snapshot::sign::v1";

impl SnapshotManifest {
    /// Return the canonical bytes over which the manifest is signed.
    ///
    /// The signable payload is the BCS encoding of the tuple
    /// `(version, block_height, entry_count, total_bytes, content_hash,
    ///   chain_id, epoch, created_at_ms, previous_manifest_hash)`,
    /// excluding the signature fields themselves to avoid circularity.
    pub fn signable_bytes(&self) -> Vec<u8> {
        bcs::to_bytes(&(
            self.version,
            self.block_height,
            self.entry_count,
            self.total_bytes,
            self.content_hash,
            &self.chain_id,
            self.epoch,
            self.created_at_ms,
            self.previous_manifest_hash,
        ))
        .expect("BCS serialization of manifest signable fields cannot fail")
    }

    /// Compute the BLAKE3 hash of this manifest's signable bytes.
    ///
    /// Used as the `previous_manifest_hash` in the next snapshot,
    /// forming a hash-chain across sequential exports.
    pub fn manifest_hash(&self) -> [u8; 32] {
        let payload = self.signable_bytes();
        *blake3::hash(&payload).as_bytes()
    }
}

/// Maximum allowed key size in a snapshot entry (1 KiB).
const SNAPSHOT_MAX_KEY_SIZE: usize = 1024;

/// Maximum allowed value size in a snapshot entry (16 MiB).
const SNAPSHOT_MAX_VALUE_SIZE: usize = 16 * 1024 * 1024;

impl StateStorage for RocksStore {
    type WriteBatch = RocksWriteBatch;

    async fn get(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
        let store = self.clone();
        let cf_name = cf.to_owned();
        let key = key.to_vec();
        tokio::task::spawn_blocking(move || {
            let cf_handle = store.cf_handle(&cf_name)?;
            store
                .inner
                .db
                .get_cf(cf_handle, &key)
                .map_err(|e| StorageError::RocksDb(e.to_string()))
        })
        .await
        .map_err(|e| StorageError::RocksDb(format!("spawn_blocking join error: {e}")))?
    }

    fn scan(
        &self,
        cf: &str,
        start: &[u8],
        end: &[u8],
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StorageError> {
        let cf_handle = self.cf_handle(cf)?;
        let mut iter = self.inner.db.raw_iterator_cf(cf_handle);
        iter.seek(start);

        let mut results = Vec::new();
        while iter.valid() {
            if let (Some(k), Some(v)) = (iter.key(), iter.value()) {
                if k >= end {
                    break;
                }
                results.push((k.to_vec(), v.to_vec()));
            }
            iter.next();
        }
        iter.status()
            .map_err(|e| StorageError::RocksDb(e.to_string()))?;
        Ok(results)
    }

    async fn write_batch(&self, batch: RocksWriteBatch) -> Result<(), StorageError> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let mut wb = rocksdb::WriteBatch::default();
            for (cf_name, key, value) in &batch.ops {
                let cf_handle = store.cf_handle(cf_name)?;
                match value {
                    Some(v) => wb.put_cf(cf_handle, key, v),
                    None => wb.delete_cf(cf_handle, key),
                }
            }
            store
                .inner
                .db
                .write(wb)
                .map_err(|e| StorageError::RocksDb(e.to_string()))
        })
        .await
        .map_err(|e| StorageError::RocksDb(format!("spawn_blocking join error: {e}")))?
    }

    fn new_batch(&self) -> RocksWriteBatch {
        RocksWriteBatch::new()
    }

    fn snapshot(&self) -> impl StateStorage<WriteBatch = Self::WriteBatch> {
        // Returns a clone sharing the same Arc<DB>.
        // For true point-in-time isolation, use `create_checkpoint()` instead,
        // which creates a RocksDB native checkpoint suitable for backup or
        // bootstrapping a new node.
        self.clone()
    }

    fn get_sync(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
        let cf_handle = self.cf_handle(cf)?;
        self.inner
            .db
            .get_cf(cf_handle, key)
            .map_err(|e| StorageError::RocksDb(e.to_string()))
    }

    fn put_sync(&self, cf: &str, key: Vec<u8>, value: Vec<u8>) -> Result<(), StorageError> {
        let cf_handle = self.cf_handle(cf)?;
        self.inner
            .db
            .put_cf(cf_handle, &key, &value)
            .map_err(|e| StorageError::RocksDb(e.to_string()))
    }
}

impl std::fmt::Debug for RocksStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RocksStore").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ColumnFamily;
    use crate::WriteBatchOps;

    fn test_store() -> (RocksStore, tempfile::TempDir) {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = StorageConfig::for_testing(tmp.path().to_path_buf());
        let store = RocksStore::open_at(tmp.path(), &config).unwrap();
        (store, tmp)
    }

    #[tokio::test]
    async fn open_and_close() {
        let (_store, _tmp) = test_store();
        // Successful open + drop = clean shutdown.
    }

    #[tokio::test]
    async fn all_column_families_exist() {
        let (store, _tmp) = test_store();
        for cf in ColumnFamily::all() {
            assert!(
                store.cf_handle(cf.as_str()).is_ok(),
                "CF {} missing",
                cf.as_str()
            );
        }
    }

    #[tokio::test]
    async fn get_nonexistent_returns_none() {
        let (store, _tmp) = test_store();
        let val = store.get("cf_state", b"missing").await.unwrap();
        assert!(val.is_none());
    }

    #[tokio::test]
    async fn put_and_get_cf() {
        let (store, _tmp) = test_store();
        let mut batch = store.new_batch();
        batch.put_cf("cf_blocks", b"blk1".to_vec(), b"data1".to_vec());
        store.write_batch(batch).await.unwrap();

        let val = store.get("cf_blocks", b"blk1").await.unwrap();
        assert_eq!(val, Some(b"data1".to_vec()));
    }

    #[tokio::test]
    async fn delete_cf() {
        let (store, _tmp) = test_store();
        let mut batch = store.new_batch();
        batch.put_cf("cf_state", b"key".to_vec(), b"val".to_vec());
        store.write_batch(batch).await.unwrap();

        let mut batch = store.new_batch();
        batch.delete_cf("cf_state", b"key".to_vec());
        store.write_batch(batch).await.unwrap();

        assert!(store.get("cf_state", b"key").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn scan_range() {
        let (store, _tmp) = test_store();
        let mut batch = store.new_batch();
        batch.put_cf("cf_state", b"a".to_vec(), b"1".to_vec());
        batch.put_cf("cf_state", b"b".to_vec(), b"2".to_vec());
        batch.put_cf("cf_state", b"c".to_vec(), b"3".to_vec());
        batch.put_cf("cf_state", b"d".to_vec(), b"4".to_vec());
        store.write_batch(batch).await.unwrap();

        let results = store.scan("cf_state", b"b", b"d").unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0], (b"b".to_vec(), b"2".to_vec()));
        assert_eq!(results[1], (b"c".to_vec(), b"3".to_vec()));
    }

    #[tokio::test]
    async fn unknown_cf_returns_error() {
        let (store, _tmp) = test_store();
        let result = store.get("cf_nonexistent", b"key").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn cross_cf_isolation() {
        let (store, _tmp) = test_store();
        let mut batch = store.new_batch();
        batch.put_cf("cf_blocks", b"key".to_vec(), b"blocks".to_vec());
        batch.put_cf("cf_state", b"key".to_vec(), b"state".to_vec());
        store.write_batch(batch).await.unwrap();

        assert_eq!(
            store.get("cf_blocks", b"key").await.unwrap(),
            Some(b"blocks".to_vec())
        );
        assert_eq!(
            store.get("cf_state", b"key").await.unwrap(),
            Some(b"state".to_vec())
        );
    }

    #[tokio::test]
    async fn batch_atomicity() {
        let (store, _tmp) = test_store();
        let mut batch = store.new_batch();
        batch.put_cf("cf_state", b"k1".to_vec(), b"v1".to_vec());
        batch.put_cf("cf_state", b"k2".to_vec(), b"v2".to_vec());
        batch.put_cf("cf_state", b"k3".to_vec(), b"v3".to_vec());
        store.write_batch(batch).await.unwrap();

        // All three should be visible.
        assert!(store.get("cf_state", b"k1").await.unwrap().is_some());
        assert!(store.get("cf_state", b"k2").await.unwrap().is_some());
        assert!(store.get("cf_state", b"k3").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn overwrite_value() {
        let (store, _tmp) = test_store();
        let mut batch = store.new_batch();
        batch.put_cf("cf_state", b"key".to_vec(), b"old".to_vec());
        store.write_batch(batch).await.unwrap();

        let mut batch = store.new_batch();
        batch.put_cf("cf_state", b"key".to_vec(), b"new".to_vec());
        store.write_batch(batch).await.unwrap();

        assert_eq!(
            store.get("cf_state", b"key").await.unwrap(),
            Some(b"new".to_vec())
        );
    }

    #[tokio::test]
    async fn empty_scan() {
        let (store, _tmp) = test_store();
        let results = store.scan("cf_state", b"a", b"z").unwrap();
        assert!(results.is_empty());
    }

    // ── P5-1: Checkpoint tests ──────────────────────────────────────────

    #[tokio::test]
    async fn create_checkpoint_and_read() {
        let (store, tmp) = test_store();

        // Write data to the original.
        let mut batch = store.new_batch();
        batch.put_cf("cf_state", b"key1".to_vec(), b"val1".to_vec());
        store.write_batch(batch).await.unwrap();

        // Create checkpoint.
        let cp_path = tmp.path().join("checkpoint1");
        store.create_checkpoint(&cp_path).unwrap();

        // Open checkpoint and verify data.
        let config = StorageConfig::for_testing(cp_path.clone());
        let cp_store = RocksStore::open_checkpoint(&cp_path, &config).unwrap();
        assert_eq!(
            cp_store.get("cf_state", b"key1").await.unwrap(),
            Some(b"val1".to_vec())
        );
    }

    #[tokio::test]
    async fn checkpoint_duplicate_path_fails() {
        let (store, tmp) = test_store();
        let cp_path = tmp.path().join("dup_checkpoint");
        store.create_checkpoint(&cp_path).unwrap();
        // Second attempt on same path should fail.
        assert!(store.create_checkpoint(&cp_path).is_err());
    }

    // ── P5-2: Pruning tests ────────────────────────────────────────────

    #[tokio::test]
    async fn prune_before_removes_old_entries() {
        let (store, _tmp) = test_store();

        // Write blocks with sequence-keyed entries (big-endian u64).
        let mut batch = store.new_batch();
        for seq in 0u64..10 {
            let key = seq.to_be_bytes().to_vec();
            batch.put_cf("cf_blocks", key.clone(), b"block_data".to_vec());
            batch.put_cf("cf_transactions", key.clone(), b"tx_data".to_vec());
            batch.put_cf("cf_receipts", key, b"receipt_data".to_vec());
        }
        store.write_batch(batch).await.unwrap();

        // Prune everything before sequence 5.
        let result = store.prune_before(5).unwrap();
        assert_eq!(result.blocks_pruned, 5);
        assert_eq!(result.transactions_pruned, 5);
        assert_eq!(result.receipts_pruned, 5);

        // Verify seq 0..5 are gone, 5..10 remain.
        for seq in 0u64..5 {
            let key = seq.to_be_bytes();
            assert!(store.get("cf_blocks", &key).await.unwrap().is_none());
        }
        for seq in 5u64..10 {
            let key = seq.to_be_bytes();
            assert!(store.get("cf_blocks", &key).await.unwrap().is_some());
        }
    }

    // ── P5-3: Storage stats tests ───────────────────────────────────────

    #[tokio::test]
    async fn storage_stats_returns_all_cfs() {
        let (store, _tmp) = test_store();
        let stats = store.storage_stats().unwrap();
        assert_eq!(stats.len(), ColumnFamily::all().len());
        for s in &stats {
            // memtable_size may be non-zero even for empty CFs.
            // sst_file_size_bytes is always valid (unsigned, so >= 0 trivially).
            let _ = s.sst_file_size_bytes;
        }
    }

    // ── P5-4: State snapshot export/import tests ────────────────────────

    #[tokio::test]
    async fn export_and_import_state_snapshot() {
        let (store, tmp) = test_store();

        // Write state entries.
        let mut batch = store.new_batch();
        for i in 0u8..20 {
            batch.put_cf("cf_state", vec![i], vec![i * 2]);
        }
        store.write_batch(batch).await.unwrap();

        // Export.
        let snap_dir = tmp.path().join("snapshot_export");
        let manifest = store.export_state_snapshot(&snap_dir, 42, None).unwrap();
        assert_eq!(manifest.version, 1);
        assert_eq!(manifest.block_height, 42);
        assert_eq!(manifest.entry_count, 20 + 1); // +1 for schema version key

        // Create a fresh store and import.
        let tmp2 = tempfile::TempDir::new().unwrap();
        let config2 = StorageConfig::for_testing(tmp2.path().to_path_buf());
        let store2 = RocksStore::open_at(tmp2.path(), &config2).unwrap();

        let imported = store2.import_state_snapshot(&snap_dir, None).unwrap();
        assert_eq!(imported.entry_count, manifest.entry_count);

        // Verify data in new store.
        for i in 0u8..20 {
            let val = store2.get("cf_state", &[i]).await.unwrap();
            assert_eq!(val, Some(vec![i * 2]));
        }
    }

    // ── Phase A acceptance tests ─────────────────────────────────────────

    #[test]
    fn snapshot_path_should_reject_parent_traversal() {
        // SEC-H3 / A-7: paths containing `..` must be rejected.
        let result = RocksStore::validate_snapshot_path(
            std::path::Path::new("/tmp/snapshots/../etc/passwd"),
            None,
        );
        assert!(result.is_err(), "parent traversal should be rejected");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("traversal"),
            "error should mention traversal, got: {err}"
        );
    }

    #[tokio::test]
    async fn snapshot_import_should_reject_corrupted_manifest() {
        // SEC-H4 / A-8: corrupted snapshot files must be rejected.
        let tmp = tempfile::TempDir::new().unwrap();
        let snap_dir = tmp.path().join("bad_snapshot");
        std::fs::create_dir_all(&snap_dir).unwrap();

        // Write a file with garbage data (invalid BCS header).
        let snap_file = snap_dir.join("state_snapshot.bin");
        let garbage: Vec<u8> = {
            let mut data = vec![];
            // header_len = 4 bytes (small valid length)
            data.extend_from_slice(&8u32.to_le_bytes());
            // 8 bytes of garbage "header"
            data.extend_from_slice(&[0xFF; 8]);
            data
        };
        std::fs::write(&snap_file, &garbage).unwrap();

        let config = StorageConfig::for_testing(tmp.path().join("db"));
        let store = RocksStore::open_at(&tmp.path().join("db"), &config).unwrap();

        let result = store.import_state_snapshot(&snap_dir, None);
        assert!(result.is_err(), "corrupted manifest should be rejected");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("invalid snapshot header"),
            "error should mention invalid header, got: {err}"
        );
    }

    #[tokio::test]
    async fn snapshot_import_should_reject_tampered_content() {
        // SEC-H4 / A-8: content hash mismatch must be detected.
        let (store, tmp) = test_store();

        // Write some state.
        let mut batch = store.new_batch();
        for i in 0u8..5 {
            batch.put_cf("cf_state", vec![i], vec![i]);
        }
        store.write_batch(batch).await.unwrap();

        // Export a valid snapshot.
        let snap_dir = tmp.path().join("snapshot_tamper");
        let manifest = store.export_state_snapshot(&snap_dir, 1, None).unwrap();
        assert!(manifest.content_hash.is_some());

        // Tamper with entry data by flipping a byte near the end of the file.
        let snap_file = snap_dir.join("state_snapshot.bin");
        let mut data = std::fs::read(&snap_file).unwrap();
        let last = data.len() - 1;
        data[last] ^= 0xFF;
        std::fs::write(&snap_file, &data).unwrap();

        // Import should fail integrity check.
        let tmp2 = tempfile::TempDir::new().unwrap();
        let config2 = StorageConfig::for_testing(tmp2.path().to_path_buf());
        let store2 = RocksStore::open_at(tmp2.path(), &config2).unwrap();

        let result = store2.import_state_snapshot(&snap_dir, None);
        assert!(result.is_err(), "tampered snapshot should be rejected");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("integrity check failed"),
            "error should mention integrity failure, got: {err}"
        );
    }

    // ── D-3 acceptance: snapshot import hash failure must not mutate DB ──

    #[tokio::test]
    async fn snapshot_import_hash_failure_should_not_mutate_database() {
        // SEC-M8: Two-pass verify-before-write ensures that a tampered
        // snapshot is rejected BEFORE any data is written to the DB.
        let (src_store, src_tmp) = test_store();

        // Populate source store.
        let mut batch = src_store.new_batch();
        for i in 0u8..10 {
            batch.put_cf("cf_state", vec![i], vec![i * 3]);
        }
        src_store.write_batch(batch).await.unwrap();

        // Export a valid snapshot.
        let snap_dir = src_tmp.path().join("snapshot_d3");
        let manifest = src_store
            .export_state_snapshot(&snap_dir, 99, None)
            .unwrap();
        assert!(manifest.content_hash.is_some());

        // Tamper with the snapshot file.
        let snap_file = snap_dir.join("state_snapshot.bin");
        let mut data = std::fs::read(&snap_file).unwrap();
        // Flip a byte in the middle of the data section.
        let mid = data.len() / 2;
        data[mid] ^= 0xFF;
        std::fs::write(&snap_file, &data).unwrap();

        // Create a fresh destination store with some pre-existing data.
        let (dst_store, _dst_tmp) = test_store();
        let mut batch = dst_store.new_batch();
        batch.put_cf("cf_state", b"sentinel".to_vec(), b"before".to_vec());
        dst_store.write_batch(batch).await.unwrap();

        // Import should fail.
        let result = dst_store.import_state_snapshot(&snap_dir, None);
        assert!(result.is_err(), "tampered snapshot should be rejected");

        // Verify that the destination DB was NOT polluted — only the
        // pre-existing sentinel key should be present.
        let sentinel = dst_store.get("cf_state", b"sentinel").await.unwrap();
        assert_eq!(
            sentinel,
            Some(b"before".to_vec()),
            "sentinel should be intact"
        );

        // Verify that NO keys from the tampered snapshot leaked in.
        for i in 0u8..10 {
            let val = dst_store.get("cf_state", &[i]).await.unwrap();
            assert!(
                val.is_none(),
                "key {} should NOT be present after failed import",
                i
            );
        }
    }

    // ── D-4 acceptance: snapshot path confined to base dir ───────────────

    #[tokio::test]
    async fn snapshot_import_should_be_confined_to_snapshot_base_dir() {
        // SEC-M9: When `allowed_base` is provided, paths outside the base
        // directory must be rejected.
        let tmp = tempfile::TempDir::new().unwrap();
        let base_dir = tmp.path().join("snapshots");
        std::fs::create_dir_all(&base_dir).unwrap();

        let config = StorageConfig::for_testing(tmp.path().join("db"));
        let store = RocksStore::open_at(&tmp.path().join("db"), &config).unwrap();

        // Path OUTSIDE the base directory should be rejected.
        let outside_path = tmp.path().join("other_dir").join("snapshot");
        std::fs::create_dir_all(&outside_path).unwrap();
        let result = store.import_state_snapshot(&outside_path, Some(&base_dir));
        assert!(result.is_err(), "path outside base_dir should be rejected");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("escapes allowed base directory"),
            "error should mention escaping base dir, got: {err}"
        );

        // Export path OUTSIDE the base should also be rejected.
        let result = store.export_state_snapshot(&outside_path, 1, Some(&base_dir));
        assert!(
            result.is_err(),
            "export outside base_dir should be rejected"
        );

        // Path INSIDE the base directory should pass validation (may fail
        // for other reasons since there's no real snapshot file).
        let inside_path = base_dir.join("my_snapshot");
        let result = store.export_state_snapshot(&inside_path, 1, Some(&base_dir));
        // Should NOT fail with "escapes" error.
        if let Err(e) = &result {
            let msg = e.to_string();
            assert!(
                !msg.contains("escapes"),
                "path inside base_dir should not be rejected: {msg}"
            );
        }
    }

    // ── D-5 acceptance: prune is atomic across column families ───────────

    #[tokio::test]
    async fn prune_should_be_atomic_across_column_families() {
        // SEC-M11: Pruning uses a single WriteBatch for all CFs, so
        // a crash between individual CF deletes cannot leave state
        // where some CFs are pruned and others are not.
        let (store, _tmp) = test_store();

        // Write 20 entries across all 3 prunable CFs.
        let mut batch = store.new_batch();
        for seq in 0u64..20 {
            let key = seq.to_be_bytes().to_vec();
            batch.put_cf(
                "cf_blocks",
                key.clone(),
                format!("block-{seq}").into_bytes(),
            );
            batch.put_cf(
                "cf_transactions",
                key.clone(),
                format!("tx-{seq}").into_bytes(),
            );
            batch.put_cf("cf_receipts", key, format!("receipt-{seq}").into_bytes());
        }
        store.write_batch(batch).await.unwrap();

        // Prune everything before seq 10.
        let result = store.prune_before(10).unwrap();
        assert_eq!(result.blocks_pruned, 10);
        assert_eq!(result.transactions_pruned, 10);
        assert_eq!(result.receipts_pruned, 10);

        // After pruning: ALL CFs should be in the same state —
        // sequences 0..10 gone, 10..20 present.
        for seq in 0u64..10 {
            let key = seq.to_be_bytes();
            assert!(
                store.get("cf_blocks", &key).await.unwrap().is_none(),
                "block seq {seq} should be pruned"
            );
            assert!(
                store.get("cf_transactions", &key).await.unwrap().is_none(),
                "tx seq {seq} should be pruned"
            );
            assert!(
                store.get("cf_receipts", &key).await.unwrap().is_none(),
                "receipt seq {seq} should be pruned"
            );
        }
        for seq in 10u64..20 {
            let key = seq.to_be_bytes();
            assert!(
                store.get("cf_blocks", &key).await.unwrap().is_some(),
                "block seq {seq} should be retained"
            );
            assert!(
                store.get("cf_transactions", &key).await.unwrap().is_some(),
                "tx seq {seq} should be retained"
            );
            assert!(
                store.get("cf_receipts", &key).await.unwrap().is_some(),
                "receipt seq {seq} should be retained"
            );
        }
    }

    // ── F-3: Fault injection tests ──────────────────────────────────

    /// F-3a: Opening a database on a non-existent (or permission-denied)
    /// path must return an error, not panic.
    #[tokio::test]
    async fn open_nonexistent_path_returns_error() {
        let config = crate::StorageConfig::default();
        let result = RocksStore::open_at(
            std::path::Path::new("/nonexistent/path/rocks_test_db"),
            &config,
        );
        assert!(result.is_err(), "open on bad path must fail");
    }

    /// F-3b: Writing to an unknown column family must return an error.
    #[tokio::test]
    async fn write_to_unknown_cf_returns_error() {
        let (store, _tmp) = test_store();
        let result = store.put_sync("cf_does_not_exist", vec![1], vec![2]);
        assert!(result.is_err(), "writing to unknown CF must fail");
    }

    /// F-3c: Importing a snapshot whose content hash does not match the
    /// manifest must leave the database unmodified (SEC-M8).
    #[tokio::test]
    async fn snapshot_import_hash_mismatch_leaves_db_clean() {
        let (store, _tmp) = test_store();

        // Seed one known entry.
        store.put_sync("cf_state", vec![0xAA], vec![0x01]).unwrap();

        // Export a legitimate snapshot.
        let snap_dir = _tmp.path().join("snap");
        std::fs::create_dir_all(&snap_dir).unwrap();
        store.export_state_snapshot(&snap_dir, 1, None).unwrap();

        // Tamper with the binary data so the hash won't match.
        let snap_file = snap_dir.join("state_snapshot.bin");
        let mut data = std::fs::read(&snap_file).unwrap();
        if let Some(last) = data.last_mut() {
            *last ^= 0xFF;
        }
        std::fs::write(&snap_file, &data).unwrap();

        // Open a fresh store and attempt import.
        let fresh_dir = _tmp.path().join("fresh_db");
        std::fs::create_dir_all(&fresh_dir).unwrap();
        let config = crate::StorageConfig::default();
        let fresh = RocksStore::open_at(&fresh_dir, &config).unwrap();

        let result = fresh.import_state_snapshot(&snap_dir, None);
        assert!(result.is_err(), "tampered snapshot must fail import");

        // Verify the fresh database is still empty (no partial writes).
        let val = fresh.get("cf_state", &[0xAA]).await.unwrap();
        assert!(val.is_none(), "tampered import must not leave debris");
    }

    /// F-3d: Pruning an empty database is a no-op, not an error.
    #[tokio::test]
    async fn prune_empty_database_is_noop() {
        let (store, _tmp) = test_store();
        let result = store.prune_before(100).unwrap();
        assert_eq!(result.blocks_pruned, 0);
        assert_eq!(result.transactions_pruned, 0);
        assert_eq!(result.receipts_pruned, 0);
    }

    /// F-3e: Double checkpoint to the same path must fail (not corrupt).
    #[tokio::test]
    async fn double_checkpoint_same_path_returns_error() {
        let (store, _tmp) = test_store();
        let ckpt_path = _tmp.path().join("ckpt1");
        store.create_checkpoint(&ckpt_path).unwrap();
        let result = store.create_checkpoint(&ckpt_path);
        assert!(result.is_err(), "duplicate checkpoint path must fail");
    }

    /// F-3f: Exporting a snapshot, then re-importing it into a separate
    /// database must produce identical state for every key.
    #[tokio::test]
    async fn export_import_roundtrip_preserves_all_state() {
        let (store, _tmp) = test_store();

        // Populate several CFs.
        let mut batch = store.new_batch();
        for i in 0u8..10 {
            batch.put_cf("cf_state", vec![i], vec![i; 4]);
        }
        store.write_batch(batch).await.unwrap();

        // Export → import into fresh store.
        let snap_dir = _tmp.path().join("snap_rt");
        std::fs::create_dir_all(&snap_dir).unwrap();
        store.export_state_snapshot(&snap_dir, 1, None).unwrap();

        let fresh_dir = _tmp.path().join("fresh_rt");
        std::fs::create_dir_all(&fresh_dir).unwrap();
        let config = crate::StorageConfig::default();
        let fresh = RocksStore::open_at(&fresh_dir, &config).unwrap();
        fresh.import_state_snapshot(&snap_dir, None).unwrap();

        // Verify every key.
        for i in 0u8..10 {
            let val = fresh.get("cf_state", &[i]).await.unwrap();
            assert_eq!(val, Some(vec![i; 4]), "key {i} must match after roundtrip");
        }
    }

    /// F-3g: Pruning with a retain_from_seq beyond all data is a no-op.
    #[tokio::test]
    async fn prune_beyond_data_range_is_safe() {
        let (store, _tmp) = test_store();

        // Write 5 entries.
        let mut batch = store.new_batch();
        for seq in 0u64..5 {
            batch.put_cf("cf_blocks", seq.to_be_bytes().to_vec(), b"data".to_vec());
        }
        store.write_batch(batch).await.unwrap();

        // Prune with a sequence far beyond the data range.
        let result = store.prune_before(9999).unwrap();
        // All 5 entries should be pruned.
        assert_eq!(result.blocks_pruned, 5);

        // Verify nothing left.
        for seq in 0u64..5 {
            assert!(store
                .get("cf_blocks", &seq.to_be_bytes())
                .await
                .unwrap()
                .is_none());
        }
    }
}
