//! `nexus-storage` — Persistent storage backend for Nexus.
//!
//! Provides a typed key-value interface over RocksDB and trait contracts
//! for the Verkle Tree state commitment layer.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────┐
//! │  StateStorage trait  (SEALED, FROZEN-3)      │
//! │  ┌──────────────┐  ┌──────────────────────┐ │
//! │  │ MemoryStore   │  │ RocksStore (RocksDB) │ │
//! │  │ (testing)     │  │ (production)         │ │
//! │  └──────────────┘  └──────────────────────┘ │
//! └─────────────────────────────────────────────┘
//! ```
//!
//! # Modules
//!
//! - [`error`]   — [`StorageError`] unified error type
//! - [`types`]   — Column families, key encodings, write operations
//! - [`traits`]  — [`StateStorage`], [`WriteBatchOps`], [`StateCommitment`], [`BackupHashTree`]
//! - [`config`]  — [`StorageConfig`] with production and testing presets
//! - [`rocks`]   — RocksDB-backed [`RocksStore`] implementation
//! - [`memory`]  — In-memory [`MemoryStore`] for testing
//!
//! # Example (testing)
//!
//! ```ignore
//! use nexus_storage::{MemoryStore, StateStorage, WriteBatchOps};
//!
//! let store = MemoryStore::new();
//! let mut batch = store.new_batch();
//! batch.put(b"key".to_vec(), b"value".to_vec());
//! store.write_batch(batch).await.unwrap();
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod backup_tree;
pub mod commitment;
pub mod commitment_persist;
pub mod config;
pub mod error;
pub mod memory;
pub mod rocks;
pub mod traits;
pub mod types;

// ── Convenience re-exports ───────────────────────────────────────────────────

pub use backup_tree::Blake3BackupTree;
pub use commitment::{canonical_empty_root, Blake3SmtCommitment, CommitmentRoot, MerkleProof};
pub use commitment_persist::{
    CommitmentMetaRecord, CommitmentMutationKind, CommitmentPersistence, IncrementalCommitmentPlan,
    PersistedLeafRecord, PersistedNodePosition,
};
pub use config::StorageConfig;
pub use error::StorageError;
pub use memory::MemoryStore;
pub use rocks::migration::{SchemaVersion, CURRENT_SCHEMA_VERSION};
pub use rocks::RocksStore;
pub use rocks::{CfStats, PruneResult, SnapshotManifest, SnapshotProvenance, SNAPSHOT_SIGN_DOMAIN};
pub use traits::{BackupHashTree, StateCommitment, StateStorage, WriteBatchOps};
pub use types::{AccountKey, ColumnFamily, ResourceKey, WriteOp};
