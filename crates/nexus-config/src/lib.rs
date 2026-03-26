//! `nexus-config` — Configuration parsing and validation for Nexus.
//!
//! Loads validator configuration from TOML files and environment variable
//! overrides. All configuration structs are typed and validated at startup;
//! invalid configuration causes a structured error rather than a runtime panic.
//!
//! # Subsystem configs
//!
//! Each subsystem has its own config struct with production-ready defaults
//! and a `for_testing()` constructor.  [`NodeConfig`] aggregates them all
//! and provides TOML file loading + env-var overrides.
//!
//! Network and storage configs are re-exported from their own crates.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod consensus;
pub mod dirs;
pub mod error;
pub mod execution;
pub mod genesis;
pub mod intent;
pub mod node;
pub mod rpc;
pub mod telemetry;

// Re-exports for convenience — consumers can reach all config types via `nexus_config::*`.
pub use consensus::ConsensusConfig;
pub use dirs::{DirValidationError, NodeDirs};
pub use error::ConfigError;
pub use execution::ExecutionConfig;
pub use genesis::{
    GenesisAllocation, GenesisConfig, GenesisValidationError, GenesisValidatorEntry,
};
pub use intent::IntentConfig;
pub use nexus_network::NetworkConfig;
pub use nexus_storage::StorageConfig;
pub use node::NodeConfig;
pub use rpc::RpcConfig;
pub use telemetry::TelemetryConfig;
