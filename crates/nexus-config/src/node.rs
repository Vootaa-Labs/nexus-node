// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Top-level node configuration.
//!
//! [`NodeConfig`] aggregates every subsystem's config into one struct
//! that can be loaded from a TOML file with optional environment
//! variable overrides.

use std::path::{Path, PathBuf};

use nexus_network::NetworkConfig;
use nexus_storage::StorageConfig;
use serde::{Deserialize, Serialize};

use crate::consensus::ConsensusConfig;
use crate::error::ConfigError;
use crate::execution::ExecutionConfig;
use crate::intent::IntentConfig;
use crate::rpc::RpcConfig;
use crate::telemetry::TelemetryConfig;

/// Complete node configuration — one value per subsystem.
///
/// # Loading order
/// 1. Built-in defaults ([`Default`])
/// 2. TOML file overrides ([`NodeConfig::from_file`])
/// 3. Environment variable overrides ([`NodeConfig::load`])
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodeConfig {
    /// P2P network settings.
    #[serde(default)]
    pub network: NetworkConfig,

    /// RocksDB and state storage settings.
    #[serde(default)]
    pub storage: StorageConfig,

    /// Narwhal + Shoal++ consensus parameters.
    #[serde(default)]
    pub consensus: ConsensusConfig,

    /// Block-STM execution engine parameters.
    #[serde(default)]
    pub execution: ExecutionConfig,

    /// Intent engine parameters.
    #[serde(default)]
    pub intent: IntentConfig,

    /// RPC / API gateway settings.
    #[serde(default)]
    pub rpc: RpcConfig,

    /// Logging and metrics settings.
    #[serde(default)]
    pub telemetry: TelemetryConfig,

    /// Optional path to a genesis JSON file.
    /// When set, the node loads genesis state at first boot.
    #[serde(default)]
    pub genesis_path: Option<PathBuf>,

    /// Optional path to the validator key directory (output of `nexus-keygen validator`).
    ///
    /// When set, the node loads persistent Falcon signing keys from this
    /// directory instead of generating ephemeral dev keys. The directory
    /// must contain `falcon-secret.json` (or `falcon.sk` for hex format).
    ///
    /// **Required for production / devnet deployments.** When `None`, the
    /// node falls back to ephemeral key generation with a warning.
    #[serde(default)]
    pub validator_key_path: Option<PathBuf>,

    /// Run in development mode — allows empty committee / no genesis.
    ///
    /// When `false` (the default), the node **requires** a `genesis_path`
    /// and will refuse to start without one. Set to `true` only for local
    /// development or unit tests.
    ///
    /// **Must never be enabled in production or testnet deployments.**
    #[serde(default)]
    pub dev_mode: bool,
}

impl NodeConfig {
    /// Minimal configuration suitable for tests.
    pub fn for_testing() -> Self {
        Self {
            network: NetworkConfig::for_testing(),
            storage: StorageConfig::for_testing(std::env::temp_dir().join("nexus-test-db")),
            consensus: ConsensusConfig::for_testing(),
            execution: ExecutionConfig::for_testing(),
            intent: IntentConfig::for_testing(),
            rpc: RpcConfig::for_testing(),
            telemetry: TelemetryConfig::for_testing(),
            genesis_path: None,
            validator_key_path: None,
            dev_mode: true,
        }
    }

    /// Load configuration from a TOML file at `path`.
    ///
    /// Missing sections fall back to their [`Default`] values via `#[serde(default)]`.
    pub fn from_file(path: &Path) -> Result<Self, ConfigError> {
        let content = std::fs::read_to_string(path).map_err(|e| ConfigError::FileRead {
            path: path.to_path_buf(),
            source: e,
        })?;
        let cfg: NodeConfig = toml::from_str(&content)?;
        Ok(cfg)
    }

    /// Primary entry point: load from an optional TOML file, then apply
    /// environment variable overrides.
    ///
    /// If `toml_path` is `None`, starts from [`Default`] values.
    pub fn load(toml_path: Option<&Path>) -> Result<Self, ConfigError> {
        let mut cfg = match toml_path {
            Some(p) => Self::from_file(p)?,
            None => Self::default(),
        };
        Self::apply_env_overrides(&mut cfg)?;
        Ok(cfg)
    }

    /// Apply well-known `NEXUS_*` environment variable overrides.
    fn apply_env_overrides(cfg: &mut NodeConfig) -> Result<(), ConfigError> {
        if let Ok(val) = std::env::var("NEXUS_LOG_LEVEL") {
            cfg.telemetry.log_level = val;
        }
        if let Ok(val) = std::env::var("NEXUS_NETWORK_PORT") {
            let port: u16 = val.parse().map_err(|_| ConfigError::EnvOverride {
                key: "NEXUS_NETWORK_PORT".to_owned(),
                reason: format!("not a valid u16: {val}"),
            })?;
            cfg.network.listen_addr.set_port(port);
        }
        if let Ok(val) = std::env::var("NEXUS_STORAGE_PATH") {
            cfg.storage.rocksdb_path = val.into();
        }
        if let Ok(val) = std::env::var("NEXUS_GRPC_PORT") {
            let port: u16 = val.parse().map_err(|_| ConfigError::EnvOverride {
                key: "NEXUS_GRPC_PORT".to_owned(),
                reason: format!("not a valid u16: {val}"),
            })?;
            cfg.rpc.grpc_listen_addr.set_port(port);
        }
        if let Ok(val) = std::env::var("NEXUS_REST_PORT") {
            let port: u16 = val.parse().map_err(|_| ConfigError::EnvOverride {
                key: "NEXUS_REST_PORT".to_owned(),
                reason: format!("not a valid u16: {val}"),
            })?;
            cfg.rpc.rest_listen_addr.set_port(port);
        }
        if let Ok(val) = std::env::var("NEXUS_GENESIS_PATH") {
            cfg.genesis_path = Some(PathBuf::from(val));
        }
        if let Ok(val) = std::env::var("NEXUS_VALIDATOR_KEY_PATH") {
            cfg.validator_key_path = Some(PathBuf::from(val));
        }
        if let Ok(val) = std::env::var("NEXUS_DEV_MODE") {
            cfg.dev_mode = matches!(val.as_str(), "1" | "true" | "yes");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn default_node_config() {
        let cfg = NodeConfig::default();
        assert_eq!(cfg.network.max_peers, 200);
        assert_eq!(cfg.consensus.epoch_length_rounds, 1000);
        assert_eq!(cfg.execution.shard_count, 4);
        assert_eq!(cfg.telemetry.log_level, "info");
    }

    #[test]
    fn testing_node_config() {
        let cfg = NodeConfig::for_testing();
        assert!(cfg.network.max_peers < 200);
        assert!(cfg.consensus.epoch_length_rounds < 100);
        assert_eq!(cfg.execution.shard_count, 1);
        assert_eq!(cfg.telemetry.log_level, "warn");
    }

    #[test]
    fn from_file_empty_toml_uses_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("empty.toml");
        std::fs::write(&path, "").expect("write");
        let cfg = NodeConfig::from_file(&path).expect("parse");
        assert_eq!(cfg.network.max_peers, 200);
        assert_eq!(cfg.consensus.epoch_length_rounds, 1000);
    }

    #[test]
    fn from_file_partial_overrides() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("partial.toml");
        let mut f = std::fs::File::create(&path).expect("create");
        writeln!(
            f,
            r#"
[consensus]
epoch_length_rounds = 500

[telemetry]
log_level = "debug"
"#
        )
        .expect("write");

        let cfg = NodeConfig::from_file(&path).expect("parse");
        assert_eq!(cfg.consensus.epoch_length_rounds, 500);
        assert_eq!(cfg.telemetry.log_level, "debug");
        // Other sections keep defaults
        assert_eq!(cfg.network.max_peers, 200);
    }

    #[test]
    fn from_file_not_found() {
        let result = NodeConfig::from_file(Path::new("/nonexistent/path.toml"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ConfigError::FileRead { .. }));
    }

    #[test]
    fn serialization_roundtrip() {
        let cfg = NodeConfig::default();
        let toml_str = toml::to_string_pretty(&cfg).expect("serialize");
        let restored: NodeConfig = toml::from_str(&toml_str).expect("deserialize");
        assert_eq!(restored.network.max_peers, cfg.network.max_peers);
        assert_eq!(
            restored.consensus.epoch_length_rounds,
            cfg.consensus.epoch_length_rounds
        );
        assert_eq!(restored.execution.shard_count, cfg.execution.shard_count);
    }
}
