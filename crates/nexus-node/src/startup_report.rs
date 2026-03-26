// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Structured startup report.
//!
//! Captures the outcome of every initialisation step so operators and
//! orchestrators can verify that a node started correctly — not just
//! that the process is alive.

#![forbid(unsafe_code)]

use serde::Serialize;

/// Summary of node startup, emitted as structured JSON to the log and
/// optionally queryable via RPC.
#[derive(Debug, Clone, Serialize)]
pub struct StartupReport {
    /// Software version.
    pub version: &'static str,
    /// Whether the node is running in development mode.
    pub dev_mode: bool,
    /// Chain identifier (from genesis or default).
    pub chain_id: String,
    /// Genesis loading outcome.
    pub genesis: GenesisOutcome,
    /// Number of validators in the committee.
    pub committee_size: usize,
    /// Local validator index in the committee.
    pub local_validator_index: u32,
    /// Number of execution shards.
    pub num_shards: u16,
    /// Storage path.
    pub storage_path: String,
    /// Session recovery outcome.
    pub session_recovery: RecoveryOutcome,
    /// Provenance recovery outcome.
    pub provenance_recovery: RecoveryOutcome,
    /// Network peer discovery outcome.
    pub network_discovery: Option<DiscoveryOutcome>,
    /// Aggregate readiness status at startup completion.
    pub readiness_status: &'static str,
    /// Proof backend assembly status.
    pub proof_backend: ProofBackendStatus,
}

/// Outcome of genesis loading.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GenesisOutcome {
    /// Genesis was loaded and applied for the first time.
    Applied,
    /// Genesis marker was already present — allocations were skipped.
    AlreadyApplied,
    /// No genesis file was provided (dev mode).
    Skipped,
}

/// Outcome of a recovery step (sessions or provenance).
#[derive(Debug, Clone, Serialize)]
pub struct RecoveryOutcome {
    /// Whether the recovery succeeded.
    pub success: bool,
    /// Number of records recovered (0 on failure).
    pub count: u64,
}

/// Outcome of validator peer discovery.
#[derive(Debug, Clone, Serialize)]
pub struct DiscoveryOutcome {
    /// Number of validators seeded into the routing table.
    pub validators_seeded: usize,
    /// Number of boot nodes added.
    pub boot_nodes_added: usize,
    /// Whether the bootstrap was initiated.
    pub bootstrap_initiated: bool,
}

/// Status of the proof backend at startup.
#[derive(Debug, Clone, Serialize)]
pub struct ProofBackendStatus {
    /// Whether the commitment tracker was created and wired.
    pub enabled: bool,
    /// Whether the proof RPC endpoints are registered.
    pub rpc_registered: bool,
    /// Whether the commitment tracker is connected to the execution bridge.
    pub execution_bridge_connected: bool,
}

impl StartupReport {
    /// Emit the report as a structured tracing event.
    pub fn log(&self) {
        match serde_json::to_string(self) {
            Ok(json) => {
                tracing::info!(
                    startup_report = %json,
                    "node startup complete"
                );
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to serialize startup report");
            }
        }
    }
}
