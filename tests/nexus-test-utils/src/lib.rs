// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! `nexus-test-utils` — Shared test utilities for the Nexus workspace.
//!
//! Provides common fixtures, assertion helpers, and test infrastructure
//! used across all crate test suites. Not included in production builds.
//!
//! # Modules
//!
//! - [`fixtures`] — Deterministic test data builders (keypairs, digests, peers)
//! - [`assert_helpers`] — Domain-specific assertion functions
//! - [`tracing_init`] — One-call test tracing subscriber setup

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod assert_helpers;
#[cfg(test)]
mod canonical_root_tests;
#[cfg(test)]
mod chaos_readiness_tests;
#[cfg(test)]
mod cold_restart_tests;
#[cfg(test)]
mod commitment_recovery_tests;
#[cfg(test)]
mod cross_node_election_tests;
#[cfg(test)]
mod cross_shard_determinism_tests;
#[cfg(test)]
mod determinism_tests;
#[cfg(test)]
mod economic_foundation_tests;
#[cfg(test)]
mod epoch_e2e_consistency_tests;
#[cfg(test)]
mod epoch_stress_tests;
#[cfg(test)]
mod epoch_tests;
#[cfg(test)]
mod fault_injection_tests;
pub mod fixtures;
#[cfg(test)]
mod fv_differential_runner;
#[cfg(test)]
mod governance_recovery_tests;
#[cfg(test)]
mod htlc_tests;
mod integration;
#[cfg(test)]
mod lifecycle_tests;
#[cfg(test)]
#[cfg(not(feature = "move-vm"))]
mod move_integration_tests;
#[cfg(test)]
mod multi_shard_tests;
#[cfg(test)]
mod multinode_tests;
#[cfg(test)]
mod network_integration_tests;
#[cfg(test)]
mod network_shard_tests;
#[cfg(test)]
mod node_e2e_tests;
#[cfg(test)]
mod node_integration_tests;
#[cfg(test)]
mod persistence_tests;
#[cfg(test)]
mod pipeline_tests;
#[cfg(test)]
mod precision_tests;
#[cfg(test)]
mod proof_smoke_tests;
#[cfg(test)]
mod proof_tests;
#[cfg(test)]
mod readiness_tests;
#[cfg(test)]
mod recovery_tests;
#[cfg(test)]
mod release_regression_tests;
#[cfg(test)]
mod resilience_tests;
#[cfg(test)]
mod rpc_integration;
pub mod scenario_tests;
#[cfg(test)]
mod shard_failure_tests;
#[cfg(test)]
mod soak_tests;
#[cfg(test)]
mod stake_weighted_cert_tests;
#[cfg(test)]
mod staking_failure_tests;
#[cfg(test)]
mod staking_regression_tests;
#[cfg(test)]
mod staking_rotation_tests;
#[cfg(test)]
mod toolchain_tests;
pub mod tracing_init;

// Convenience re-exports.
pub use fixtures::crypto as crypto_fixtures;
pub use fixtures::network as network_fixtures;
pub use fixtures::primitives as primitive_fixtures;
pub use fixtures::storage as storage_fixtures;
