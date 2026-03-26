// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Unified error type for all storage operations.
//!
//! [`StorageError`] covers RocksDB failures, serialization issues,
//! schema violations, and state commitment problems.

use std::path::PathBuf;

/// Unified error type for the nexus-storage crate.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// RocksDB returned an internal error.
    #[error("rocksdb error: {0}")]
    RocksDb(String),

    /// BCS serialization or deserialization failed.
    #[error("serialization error: {0}")]
    Serialization(String),

    /// An unknown or invalid column family was referenced.
    #[error("unknown column family: {name}")]
    UnknownColumnFamily {
        /// The column family name that was not found.
        name: String,
    },

    /// The database path does not exist or is inaccessible.
    #[error("database path error: {path}: {reason}")]
    InvalidPath {
        /// The offending path.
        path: PathBuf,
        /// What went wrong.
        reason: String,
    },

    /// A key encoding or decoding error.
    #[error("key codec error: {0}")]
    KeyCodec(String),

    /// The storage subsystem is shutting down.
    #[error("storage is shutting down")]
    ShuttingDown,

    /// A snapshot operation failed.
    #[error("snapshot error: {0}")]
    Snapshot(String),

    /// State commitment (primary/backup tree) mismatch or failure.
    #[error("state commitment error: {0}")]
    StateCommitment(String),
}

impl StorageError {
    /// Returns `true` if this error is transient and the operation may succeed
    /// on retry (e.g., temporary I/O pressure).
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::RocksDb(_))
    }

    /// Returns `true` if this error indicates the storage subsystem cannot
    /// continue operating.
    pub fn is_fatal(&self) -> bool {
        matches!(self, Self::ShuttingDown | Self::InvalidPath { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display() {
        let e = StorageError::RocksDb("io timeout".into());
        assert!(e.to_string().contains("rocksdb error"));
        assert!(e.is_retryable());
        assert!(!e.is_fatal());
    }

    #[test]
    fn shutting_down_is_fatal() {
        let e = StorageError::ShuttingDown;
        assert!(e.is_fatal());
        assert!(!e.is_retryable());
    }

    #[test]
    fn unknown_cf_display() {
        let e = StorageError::UnknownColumnFamily {
            name: "cf_foo".into(),
        };
        assert!(e.to_string().contains("cf_foo"));
    }
}
