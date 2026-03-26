// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Configuration error types.

use std::path::PathBuf;

/// Errors that may occur while loading or validating configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// TOML file could not be read from disk.
    #[error("failed to read config file {path}: {source}")]
    FileRead {
        /// Path that was attempted.
        path: PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },

    /// TOML content failed to parse.
    #[error("failed to parse TOML: {0}")]
    TomlParse(#[from] toml::de::Error),

    /// A configuration value is out of its valid range.
    #[error("invalid config value: {0}")]
    InvalidValue(String),

    /// An environment variable override contained a bad value.
    #[error("invalid env override for {key}: {reason}")]
    EnvOverride {
        /// The environment variable name.
        key: String,
        /// What was wrong.
        reason: String,
    },
}
