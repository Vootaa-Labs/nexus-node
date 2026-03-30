// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Structured startup report.
//!
//! Captures the outcome of every initialisation step so operators and
//! orchestrators can verify that a node started correctly — not just
//! that the process is alive.

#![forbid(unsafe_code)]

use crate::validator_discovery::ValidatorDiscoveryResult;
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

#[derive(Debug, Clone)]
pub struct StartupReportInputs {
    pub dev_mode: bool,
    pub chain_id: String,
    pub genesis: GenesisOutcome,
    pub committee_size: usize,
    pub local_validator_index: u32,
    pub num_shards: u16,
    pub storage_path: String,
    pub session_recovery: RecoveryOutcome,
    pub provenance_recovery: RecoveryOutcome,
    pub network_discovery: Option<DiscoveryOutcome>,
    pub readiness_status: &'static str,
    pub proof_backend: ProofBackendStatus,
}

pub fn genesis_outcome_from_boot(genesis_already_applied: bool) -> GenesisOutcome {
    if genesis_already_applied {
        GenesisOutcome::AlreadyApplied
    } else {
        GenesisOutcome::Applied
    }
}

impl From<ValidatorDiscoveryResult> for DiscoveryOutcome {
    fn from(value: ValidatorDiscoveryResult) -> Self {
        Self {
            validators_seeded: value.validators_seeded,
            boot_nodes_added: value.boot_nodes_added,
            bootstrap_initiated: value.bootstrap_initiated,
        }
    }
}

impl StartupReport {
    pub fn from_inputs(inputs: StartupReportInputs) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            dev_mode: inputs.dev_mode,
            chain_id: inputs.chain_id,
            genesis: inputs.genesis,
            committee_size: inputs.committee_size,
            local_validator_index: inputs.local_validator_index,
            num_shards: inputs.num_shards,
            storage_path: inputs.storage_path,
            session_recovery: inputs.session_recovery,
            provenance_recovery: inputs.provenance_recovery,
            network_discovery: inputs.network_discovery,
            readiness_status: inputs.readiness_status,
            proof_backend: inputs.proof_backend,
        }
    }

    fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Emit the report as a structured tracing event.
    pub fn log(&self) {
        match self.to_json() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_report() -> StartupReport {
        StartupReport {
            version: env!("CARGO_PKG_VERSION"),
            dev_mode: true,
            chain_id: "nexus-devnet".to_string(),
            genesis: GenesisOutcome::Applied,
            committee_size: 7,
            local_validator_index: 2,
            num_shards: 4,
            storage_path: "data/db".to_string(),
            session_recovery: RecoveryOutcome {
                success: true,
                count: 12,
            },
            provenance_recovery: RecoveryOutcome {
                success: false,
                count: 0,
            },
            network_discovery: Some(DiscoveryOutcome {
                validators_seeded: 7,
                boot_nodes_added: 3,
                bootstrap_initiated: true,
            }),
            readiness_status: "healthy",
            proof_backend: ProofBackendStatus {
                enabled: true,
                rpc_registered: true,
                execution_bridge_connected: false,
            },
        }
    }

    #[test]
    fn startup_report_json_snapshot_is_stable() {
        let report = sample_report();
        let value: serde_json::Value = serde_json::from_str(&report.to_json().unwrap()).unwrap();

        assert_eq!(
            value,
            json!({
                "version": env!("CARGO_PKG_VERSION"),
                "dev_mode": true,
                "chain_id": "nexus-devnet",
                "genesis": "applied",
                "committee_size": 7,
                "local_validator_index": 2,
                "num_shards": 4,
                "storage_path": "data/db",
                "session_recovery": {
                    "success": true,
                    "count": 12
                },
                "provenance_recovery": {
                    "success": false,
                    "count": 0
                },
                "network_discovery": {
                    "validators_seeded": 7,
                    "boot_nodes_added": 3,
                    "bootstrap_initiated": true
                },
                "readiness_status": "healthy",
                "proof_backend": {
                    "enabled": true,
                    "rpc_registered": true,
                    "execution_bridge_connected": false
                }
            })
        );
    }

    #[test]
    fn startup_report_serializes_optional_discovery_as_null() {
        let mut report = sample_report();
        report.network_discovery = None;

        let value: serde_json::Value = serde_json::from_str(&report.to_json().unwrap()).unwrap();
        assert_eq!(value["network_discovery"], serde_json::Value::Null);
    }

    #[test]
    fn startup_outcome_variants_use_snake_case_strings() {
        assert_eq!(
            serde_json::to_string(&GenesisOutcome::Applied).unwrap(),
            "\"applied\""
        );
        assert_eq!(
            serde_json::to_string(&GenesisOutcome::AlreadyApplied).unwrap(),
            "\"already_applied\""
        );
        assert_eq!(
            serde_json::to_string(&GenesisOutcome::Skipped).unwrap(),
            "\"skipped\""
        );
    }

    // ── genesis_outcome_from_boot ────────────────────────────────

    #[test]
    fn genesis_outcome_from_boot_already_applied() {
        let outcome = genesis_outcome_from_boot(true);
        assert!(matches!(outcome, GenesisOutcome::AlreadyApplied));
    }

    #[test]
    fn genesis_outcome_from_boot_newly_applied() {
        let outcome = genesis_outcome_from_boot(false);
        assert!(matches!(outcome, GenesisOutcome::Applied));
    }

    // ── StartupReport::log ──────────────────────────────────────────

    #[test]
    fn startup_report_log_does_not_panic() {
        let report = sample_report();
        // Just verify it doesn't panic; it logs via tracing.
        report.log();
    }

    #[test]
    fn recovery_and_proof_backend_shapes_are_stable() {
        let recovery = RecoveryOutcome {
            success: true,
            count: 9,
        };
        let proof_backend = ProofBackendStatus {
            enabled: true,
            rpc_registered: false,
            execution_bridge_connected: true,
        };

        assert_eq!(
            serde_json::to_value(recovery).unwrap(),
            json!({
                "success": true,
                "count": 9
            })
        );
        assert_eq!(
            serde_json::to_value(proof_backend).unwrap(),
            json!({
                "enabled": true,
                "rpc_registered": false,
                "execution_bridge_connected": true
            })
        );
    }

    #[test]
    fn startup_report_from_inputs_populates_all_fields() {
        let report = StartupReport::from_inputs(StartupReportInputs {
            dev_mode: false,
            chain_id: "nexus-mainnet".to_string(),
            genesis: GenesisOutcome::AlreadyApplied,
            committee_size: 11,
            local_validator_index: 5,
            num_shards: 8,
            storage_path: "/var/lib/nexus/db".to_string(),
            session_recovery: RecoveryOutcome {
                success: true,
                count: 99,
            },
            provenance_recovery: RecoveryOutcome {
                success: true,
                count: 7,
            },
            network_discovery: None,
            readiness_status: "healthy",
            proof_backend: ProofBackendStatus {
                enabled: true,
                rpc_registered: true,
                execution_bridge_connected: true,
            },
        });

        assert_eq!(report.version, env!("CARGO_PKG_VERSION"));
        assert!(!report.dev_mode);
        assert_eq!(report.chain_id, "nexus-mainnet");
        assert!(matches!(report.genesis, GenesisOutcome::AlreadyApplied));
        assert_eq!(report.committee_size, 11);
        assert_eq!(report.local_validator_index, 5);
        assert_eq!(report.num_shards, 8);
        assert_eq!(report.storage_path, "/var/lib/nexus/db");
        assert_eq!(report.session_recovery.count, 99);
        assert_eq!(report.provenance_recovery.count, 7);
        assert!(report.network_discovery.is_none());
        assert_eq!(report.readiness_status, "healthy");
        assert!(report.proof_backend.execution_bridge_connected);
    }

    #[test]
    fn validator_discovery_result_maps_to_discovery_outcome() {
        let outcome = DiscoveryOutcome::from(ValidatorDiscoveryResult {
            validators_seeded: 9,
            boot_nodes_added: 2,
            bootstrap_initiated: true,
        });

        assert_eq!(outcome.validators_seeded, 9);
        assert_eq!(outcome.boot_nodes_added, 2);
        assert!(outcome.bootstrap_initiated);
    }

    #[test]
    fn genesis_outcome_from_boot_matches_persistence_state() {
        assert!(matches!(
            genesis_outcome_from_boot(true),
            GenesisOutcome::AlreadyApplied
        ));
        assert!(matches!(
            genesis_outcome_from_boot(false),
            GenesisOutcome::Applied
        ));
    }
}
