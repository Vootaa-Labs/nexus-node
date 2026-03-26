//! `nexus-node` — Nexus validator node assembly crate.
//!
//! The library re-exports backend adapters and node wiring utilities.
//! The binary (`main.rs`) is the thin entry point that parses configuration
//! and calls into these modules.

#![forbid(unsafe_code)]

pub mod anchor_batch;
pub mod backends;
pub mod batch_persist;
pub mod batch_proposer;
pub mod batch_store;
pub mod cert_aggregator;
pub mod chain_identity;
pub mod commitment_tracker;
pub mod consensus_bridge;
pub mod epoch_network_bridge;
pub mod epoch_store;
pub mod execution_bridge;
pub mod genesis_boot;
pub mod gossip_bridge;
pub mod intent_watcher;
pub mod mempool;
pub mod node_metrics;
pub mod readiness;
pub mod session_cleanup;
pub mod snapshot_provider;
pub mod snapshot_signing;
pub mod staking_snapshot;
pub mod startup_report;
pub mod state_sync;
pub mod storage_maintenance;
pub mod validator_discovery;
pub mod validator_identity;
pub mod validator_keys;
