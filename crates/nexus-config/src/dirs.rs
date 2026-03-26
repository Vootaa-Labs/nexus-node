//! Container directory contract and validation.
//!
//! Defines the standard directory layout for Nexus node deployments,
//! separating data, configuration, and key material into distinct
//! mount points. This enables safe container volume management and
//! ensures restart-safe persistence.
//!
//! # Standard layout
//!
//! ```text
//! /nexus/
//!   config/    ← read-only: node TOML + genesis JSON
//!   keys/      ← read-only: validator key files (0600)
//!   data/      ← read-write: RocksDB, WAL, chain state
//! ```

use std::path::{Path, PathBuf};

/// Default base directory inside containers.
pub const DEFAULT_BASE_DIR: &str = "/nexus";

/// Subdirectory for read-only configuration files (TOML, genesis JSON).
pub const CONFIG_SUBDIR: &str = "config";

/// Subdirectory for validator key material (Falcon, Dilithium, Kyber).
pub const KEYS_SUBDIR: &str = "keys";

/// Subdirectory for persistent state (RocksDB, chain data).
pub const DATA_SUBDIR: &str = "data";

/// Resolved directory paths for a Nexus node deployment.
#[derive(Debug, Clone)]
pub struct NodeDirs {
    /// Root base directory.
    pub base: PathBuf,
    /// Configuration directory (node TOML, genesis JSON).
    pub config: PathBuf,
    /// Key material directory (validator signing keys).
    pub keys: PathBuf,
    /// Persistent data directory (RocksDB, chain state).
    pub data: PathBuf,
}

impl NodeDirs {
    /// Construct paths from a base directory.
    pub fn from_base(base: impl Into<PathBuf>) -> Self {
        let base = base.into();
        Self {
            config: base.join(CONFIG_SUBDIR),
            keys: base.join(KEYS_SUBDIR),
            data: base.join(DATA_SUBDIR),
            base,
        }
    }

    /// Construct the default container layout (`/nexus`).
    pub fn container_default() -> Self {
        Self::from_base(DEFAULT_BASE_DIR)
    }

    /// Validate that all required directories exist and are accessible.
    ///
    /// - `config/` must exist and be readable.
    /// - `keys/` must exist and be readable.
    /// - `data/` must exist and be writable.
    ///
    /// Returns the first error encountered.
    pub fn validate(&self) -> Result<(), DirValidationError> {
        Self::check_dir_readable(&self.config, "config")?;
        Self::check_dir_readable(&self.keys, "keys")?;
        Self::check_dir_writable(&self.data, "data")?;
        Ok(())
    }

    /// Ensure all directories exist, creating `data/` if needed.
    /// `config/` and `keys/` must already exist (they are externally mounted).
    pub fn ensure_data_dir(&self) -> Result<(), DirValidationError> {
        if !self.data.exists() {
            std::fs::create_dir_all(&self.data).map_err(|e| DirValidationError::CreateFailed {
                dir: "data".to_owned(),
                path: self.data.clone(),
                source: e,
            })?;
        }
        Ok(())
    }

    fn check_dir_readable(path: &Path, name: &str) -> Result<(), DirValidationError> {
        if !path.exists() {
            return Err(DirValidationError::Missing {
                dir: name.to_owned(),
                path: path.to_path_buf(),
            });
        }
        if !path.is_dir() {
            return Err(DirValidationError::NotADirectory {
                dir: name.to_owned(),
                path: path.to_path_buf(),
            });
        }
        Ok(())
    }

    fn check_dir_writable(path: &Path, name: &str) -> Result<(), DirValidationError> {
        Self::check_dir_readable(path, name)?;
        // Probe writability by attempting to create and remove a temp file.
        let probe = path.join(".nexus-write-probe");
        std::fs::write(&probe, b"probe").map_err(|e| DirValidationError::NotWritable {
            dir: name.to_owned(),
            path: path.to_path_buf(),
            source: e,
        })?;
        let _ = std::fs::remove_file(&probe);
        Ok(())
    }
}

/// Errors detected during directory validation.
#[derive(Debug, thiserror::Error)]
pub enum DirValidationError {
    /// Required directory does not exist.
    #[error("{dir} directory missing: {path}")]
    Missing {
        /// Which logical directory (config/keys/data).
        dir: String,
        /// The filesystem path.
        path: PathBuf,
    },

    /// Path exists but is not a directory.
    #[error("{dir} path is not a directory: {path}")]
    NotADirectory {
        /// Which logical directory.
        dir: String,
        /// The filesystem path.
        path: PathBuf,
    },

    /// Directory exists but is not writable.
    #[error("{dir} directory not writable: {path}: {source}")]
    NotWritable {
        /// Which logical directory.
        dir: String,
        /// The filesystem path.
        path: PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },

    /// Could not create the directory.
    #[error("failed to create {dir} directory at {path}: {source}")]
    CreateFailed {
        /// Which logical directory.
        dir: String,
        /// The filesystem path.
        path: PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_base_builds_correct_paths() {
        let dirs = NodeDirs::from_base("/opt/nexus");
        assert_eq!(dirs.base, PathBuf::from("/opt/nexus"));
        assert_eq!(dirs.config, PathBuf::from("/opt/nexus/config"));
        assert_eq!(dirs.keys, PathBuf::from("/opt/nexus/keys"));
        assert_eq!(dirs.data, PathBuf::from("/opt/nexus/data"));
    }

    #[test]
    fn container_default_uses_slash_nexus() {
        let dirs = NodeDirs::container_default();
        assert_eq!(dirs.base, PathBuf::from("/nexus"));
    }

    #[test]
    fn validate_missing_config_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = NodeDirs::from_base(tmp.path().join("nonexistent"));
        let err = dirs.validate().unwrap_err();
        assert!(matches!(err, DirValidationError::Missing { dir, .. } if dir == "config"));
    }

    #[test]
    fn validate_missing_keys_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("node");
        std::fs::create_dir_all(base.join(CONFIG_SUBDIR)).unwrap();
        let dirs = NodeDirs::from_base(&base);
        let err = dirs.validate().unwrap_err();
        assert!(matches!(err, DirValidationError::Missing { dir, .. } if dir == "keys"));
    }

    #[test]
    fn validate_missing_data_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("node");
        std::fs::create_dir_all(base.join(CONFIG_SUBDIR)).unwrap();
        std::fs::create_dir_all(base.join(KEYS_SUBDIR)).unwrap();
        let dirs = NodeDirs::from_base(&base);
        let err = dirs.validate().unwrap_err();
        assert!(matches!(err, DirValidationError::Missing { dir, .. } if dir == "data"));
    }

    #[test]
    fn validate_all_present_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("node");
        std::fs::create_dir_all(base.join(CONFIG_SUBDIR)).unwrap();
        std::fs::create_dir_all(base.join(KEYS_SUBDIR)).unwrap();
        std::fs::create_dir_all(base.join(DATA_SUBDIR)).unwrap();
        let dirs = NodeDirs::from_base(&base);
        dirs.validate().unwrap();
    }

    #[test]
    fn ensure_data_dir_creates_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = NodeDirs::from_base(tmp.path().join("fresh"));
        assert!(!dirs.data.exists());
        dirs.ensure_data_dir().unwrap();
        assert!(dirs.data.is_dir());
    }

    #[test]
    fn not_a_directory_detected() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("node");
        // Create config as a file, not a directory
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join(CONFIG_SUBDIR), "not a dir").unwrap();
        let dirs = NodeDirs::from_base(&base);
        let err = dirs.validate().unwrap_err();
        assert!(matches!(err, DirValidationError::NotADirectory { dir, .. } if dir == "config"));
    }
}
