// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Database schema migration framework (P5-5).
//!
//! Tracks a `SchemaVersion` in a reserved key within the default column
//! family.  On each `RocksStore::open`, [`migrate`] compares the on-disk
//! version against [`CURRENT_SCHEMA_VERSION`] and runs any pending
//! migrations sequentially.
//!
//! # Adding a new migration
//!
//! 1. Bump [`CURRENT_SCHEMA_VERSION`].
//! 2. Add a new arm in [`run_migration`] for the previous version.
//! 3. Each migration function receives a `&RocksStore` and must be
//!    idempotent (safe to re-run if the node crashes mid-migration).

use crate::error::StorageError;
use crate::traits::StateStorage;
use crate::types::ColumnFamily;

/// Key used to store the schema version in `cf_state`.
const SCHEMA_VERSION_KEY: &[u8] = b"__nexus_schema_version__";

/// Current schema version.  Increment when introducing a migration.
pub const CURRENT_SCHEMA_VERSION: SchemaVersion = SchemaVersion(3);

/// Schema version identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SchemaVersion(pub u32);

impl SchemaVersion {
    fn to_bytes(self) -> Vec<u8> {
        self.0.to_le_bytes().to_vec()
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self, StorageError> {
        if bytes.len() != 4 {
            return Err(StorageError::Serialization(format!(
                "schema version expects 4 bytes, got {}",
                bytes.len()
            )));
        }
        let arr: [u8; 4] = bytes.try_into().unwrap();
        Ok(Self(u32::from_le_bytes(arr)))
    }
}

/// Read the on-disk schema version.  Returns `SchemaVersion(1)` for
/// databases created before the migration framework existed (they
/// implicitly have schema version 1).
fn read_version(store: &super::RocksStore) -> Result<SchemaVersion, StorageError> {
    let val = store.get_sync(ColumnFamily::State.as_str(), SCHEMA_VERSION_KEY)?;
    match val {
        Some(ref bytes) => SchemaVersion::from_bytes(bytes),
        None => Ok(SchemaVersion(1)), // pre-migration DB
    }
}

/// Write the schema version to disk.
fn write_version(store: &super::RocksStore, version: SchemaVersion) -> Result<(), StorageError> {
    store.put_sync(
        ColumnFamily::State.as_str(),
        SCHEMA_VERSION_KEY.to_vec(),
        version.to_bytes(),
    )
}

/// Run all pending migrations from `current` up to (but not including)
/// `CURRENT_SCHEMA_VERSION`, then write the new version.
pub(crate) fn migrate(store: &super::RocksStore) -> Result<(), StorageError> {
    let mut version = read_version(store)?;

    if version > CURRENT_SCHEMA_VERSION {
        return Err(StorageError::Snapshot(format!(
            "database was created by a newer version (schema v{}, max supported v{})",
            version.0, CURRENT_SCHEMA_VERSION.0
        )));
    }

    while version < CURRENT_SCHEMA_VERSION {
        tracing::info!(
            from = version.0,
            to = version.0 + 1,
            "running schema migration"
        );
        run_migration(store, version)?;
        version = SchemaVersion(version.0 + 1);
        write_version(store, version)?;
    }

    Ok(())
}

/// Dispatch a single migration step.
fn run_migration(_store: &super::RocksStore, from: SchemaVersion) -> Result<(), StorageError> {
    match from.0 {
        // v1 → v2: Sessions + Provenance column families were added in
        // Phase 2.  Existing DBs already have them (created at open time
        // via `create_missing_column_families`), so no data migration is
        // needed — we just record the version bump.
        1 => Ok(()),
        // v2 → v3: commitment persistence column families are added for
        // Phase M. Existing DBs receive them automatically via
        // `create_missing_column_families`, so the migration is metadata-only.
        2 => Ok(()),
        other => Err(StorageError::Snapshot(format!(
            "no migration path from schema version {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::StorageConfig;
    use crate::rocks::RocksStore;

    fn test_store() -> (RocksStore, tempfile::TempDir) {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = StorageConfig::for_testing(tmp.path().to_path_buf());
        let store = RocksStore::open_at(tmp.path(), &config).unwrap();
        (store, tmp)
    }

    #[test]
    fn fresh_db_gets_current_version() {
        let (store, _tmp) = test_store();
        let v = read_version(&store).unwrap();
        assert_eq!(v, CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn version_roundtrip() {
        let (store, _tmp) = test_store();
        let v = SchemaVersion(42);
        write_version(&store, v).unwrap();
        assert_eq!(read_version(&store).unwrap(), v);
    }

    #[test]
    fn reopen_preserves_version() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = StorageConfig::for_testing(tmp.path().to_path_buf());

        {
            let store = RocksStore::open_at(tmp.path(), &config).unwrap();
            assert_eq!(read_version(&store).unwrap(), CURRENT_SCHEMA_VERSION);
        }

        // Re-open — migration should be a no-op.
        {
            let store = RocksStore::open_at(tmp.path(), &config).unwrap();
            assert_eq!(read_version(&store).unwrap(), CURRENT_SCHEMA_VERSION);
        }
    }
}
