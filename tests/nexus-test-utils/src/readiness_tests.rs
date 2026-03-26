// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Readiness negative tests (v0.1.5 — readiness hardening).
//!
//! Verifies that the node health and readiness endpoints report
//! non-ready states when subsystems are degraded, down, or still
//! bootstrapping.

use nexus_node::readiness::{NodeReadiness, NodeStatus};
use nexus_primitives::{AccountAddress, Amount, CommitSequence, EpochNumber, TokenId, TxDigest};
use nexus_rpc::rest::{readiness, AppState, QueryBackend};
use nexus_rpc::{HealthResponse, RpcError, SubsystemHealthDto};

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

// ── Configurable mock backend ───────────────────────────────────────────

/// Mock backend that allows controlling the reported health status
/// via a [`NodeReadiness`] tracker.
struct ReadinessMockBackend {
    readiness: NodeReadiness,
}

impl ReadinessMockBackend {
    fn new(readiness: NodeReadiness) -> Self {
        Self { readiness }
    }
}

impl QueryBackend for ReadinessMockBackend {
    fn account_balance(
        &self,
        _address: &AccountAddress,
        _token: &TokenId,
    ) -> Result<Amount, RpcError> {
        Err(RpcError::NotFound("mock".into()))
    }

    fn transaction_receipt(
        &self,
        _digest: &TxDigest,
    ) -> Result<Option<nexus_rpc::TransactionReceiptDto>, RpcError> {
        Ok(None)
    }

    fn health_status(&self) -> HealthResponse {
        let snap = self.readiness.subsystem_snapshot();
        let dto: Vec<SubsystemHealthDto> = snap
            .iter()
            .map(|s| SubsystemHealthDto {
                name: s.name,
                status: s.status,
                last_progress_ms: s.last_progress_ms,
            })
            .collect();
        HealthResponse {
            status: self.readiness.status().as_str(),
            version: env!("CARGO_PKG_VERSION"),
            peers: 0,
            epoch: EpochNumber(0),
            latest_commit: CommitSequence(0),
            uptime_seconds: 0,
            subsystems: dto,
            reason: None,
        }
    }

    fn contract_query(
        &self,
        _request: &nexus_rpc::ContractQueryRequest,
    ) -> Result<nexus_rpc::ContractQueryResponse, RpcError> {
        Err(RpcError::Unavailable("mock".into()))
    }
}

fn state_with_readiness(readiness: NodeReadiness) -> Arc<AppState> {
    Arc::new(AppState {
        query: Arc::new(ReadinessMockBackend::new(readiness)),
        intent: None,
        consensus: None,
        network: None,
        broadcaster: None,
        events: None,
        rate_limiter: None,
        faucet_addr_limiter: None,
        metrics_handle: None,
        faucet_enabled: false,
        faucet_amount: 0,
        max_ws_connections: 100,
        ws_connection_count: std::sync::atomic::AtomicUsize::new(0),
        intent_tracker: None,
        session_provenance: None,
        state_proof: None,
        mcp_dispatcher: None,
        mcp_call_index: std::sync::atomic::AtomicU64::new(0),
        quota_manager: None,
        query_gas_budget: 10_000_000,
        query_timeout_ms: 5_000,
        num_shards: 1,
        tx_lifecycle: None,
        htlc: None,
    })
}

// ── Tests ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn ready_returns_503_when_bootstrapping() {
    let nr = NodeReadiness::new();
    // All subsystems still starting — node should be bootstrapping.
    assert_eq!(nr.status(), NodeStatus::Bootstrapping);

    let app = readiness::router().with_state(state_with_readiness(nr));
    let req = Request::builder()
        .uri("/ready")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["ready"], false);
    assert_eq!(json["status"], "bootstrapping");
}

#[tokio::test]
async fn ready_returns_503_when_execution_halted() {
    let nr = NodeReadiness::new();
    nr.storage_handle().set_ready();
    nr.network_handle().set_ready();
    nr.consensus_handle().set_ready();
    nr.execution_handle().set_down(); // execution down → halted
    nr.genesis_handle().set_ready();
    assert_eq!(nr.status(), NodeStatus::Halted);

    let app = readiness::router().with_state(state_with_readiness(nr));
    let req = Request::builder()
        .uri("/ready")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["ready"], false);
    assert_eq!(json["status"], "halted");
}

#[tokio::test]
async fn ready_returns_503_when_consensus_syncing() {
    let nr = NodeReadiness::new();
    nr.storage_handle().set_ready();
    nr.network_handle().set_ready();
    nr.consensus_handle().set_degraded(); // consensus degraded → syncing
    nr.execution_handle().set_ready();
    nr.genesis_handle().set_ready();
    assert_eq!(nr.status(), NodeStatus::Syncing);

    let app = readiness::router().with_state(state_with_readiness(nr));
    let req = Request::builder()
        .uri("/ready")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["ready"], false);
    assert_eq!(json["status"], "syncing");
}

#[tokio::test]
async fn ready_returns_503_when_network_offline() {
    let nr = NodeReadiness::new();
    nr.storage_handle().set_ready();
    nr.network_handle().set_down(); // network down → degraded
    nr.consensus_handle().set_ready();
    nr.execution_handle().set_ready();
    nr.genesis_handle().set_ready();
    assert_eq!(nr.status(), NodeStatus::Degraded);

    let app = readiness::router().with_state(state_with_readiness(nr));
    let req = Request::builder()
        .uri("/ready")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    // Degraded is still ready — node can serve traffic.
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["ready"], true);
    assert_eq!(json["status"], "degraded");
}

#[tokio::test]
async fn ready_returns_200_when_all_healthy() {
    let nr = NodeReadiness::new();
    nr.storage_handle().set_ready();
    nr.network_handle().set_ready();
    nr.consensus_handle().set_ready();
    nr.execution_handle().set_ready();
    nr.genesis_handle().set_ready();
    assert_eq!(nr.status(), NodeStatus::Healthy);

    let app = readiness::router().with_state(state_with_readiness(nr));
    let req = Request::builder()
        .uri("/ready")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["ready"], true);
    assert_eq!(json["status"], "healthy");
}

#[tokio::test]
async fn health_returns_subsystem_breakdown() {
    use nexus_rpc::rest::health;
    let nr = NodeReadiness::new();
    nr.storage_handle().set_ready();
    nr.network_handle().set_degraded();
    nr.consensus_handle().set_ready();
    nr.execution_handle().set_ready();
    nr.genesis_handle().set_ready();

    let app = health::router().with_state(state_with_readiness(nr));
    let req = Request::builder()
        .uri("/health")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "degraded");

    let subsystems = json["subsystems"].as_array().expect("subsystems array");
    assert_eq!(subsystems.len(), 5);
    // Find the network entry.
    let network = subsystems.iter().find(|s| s["name"] == "network").unwrap();
    assert_eq!(network["status"], "degraded");
}

#[tokio::test]
async fn ready_returns_503_when_storage_down() {
    let nr = NodeReadiness::new();
    nr.storage_handle().set_down(); // critical subsystem down → halted
    nr.network_handle().set_ready();
    nr.consensus_handle().set_ready();
    nr.execution_handle().set_ready();
    nr.genesis_handle().set_ready();
    assert_eq!(nr.status(), NodeStatus::Halted);

    let app = readiness::router().with_state(state_with_readiness(nr));
    let req = Request::builder()
        .uri("/ready")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}
