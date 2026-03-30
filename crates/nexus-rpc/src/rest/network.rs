// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Network status RPC endpoints (T-7007).
//!
//! | Method | Path | Description |
//! |--------|------|-------------|
//! | GET | `/v2/network/peers` | List known peers |
//! | GET | `/v2/network/status` | Routing table health |
//! | GET | `/v2/network/health` | P2P liveness check |

use std::sync::Arc;

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};

use crate::dto::{NetworkHealthResponse, NetworkPeersResponse, NetworkStatusResponse};
use crate::error::RpcError;
use crate::rest::AppState;

/// Build the network status router fragment.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v2/network/peers", get(network_peers))
        .route("/v2/network/status", get(network_status))
        .route("/v2/network/health", get(network_health))
}

/// `GET /v2/network/peers` — list all known peers.
async fn network_peers(
    State(state): State<Arc<AppState>>,
) -> Result<Json<NetworkPeersResponse>, RpcError> {
    let backend = state
        .network
        .as_ref()
        .ok_or_else(|| RpcError::Unavailable("network backend not available".into()))?;
    let response = backend.network_peers()?;
    Ok(Json(response))
}

/// `GET /v2/network/status` — routing table health and peer statistics.
async fn network_status(
    State(state): State<Arc<AppState>>,
) -> Result<Json<NetworkStatusResponse>, RpcError> {
    let backend = state
        .network
        .as_ref()
        .ok_or_else(|| RpcError::Unavailable("network backend not available".into()))?;
    let response = backend.network_status()?;
    Ok(Json(response))
}

/// `GET /v2/network/health` — P2P connection health (liveness).
async fn network_health(
    State(state): State<Arc<AppState>>,
) -> Result<Json<NetworkHealthResponse>, RpcError> {
    let backend = state
        .network
        .as_ref()
        .ok_or_else(|| RpcError::Unavailable("network backend not available".into()))?;
    let response = backend.network_health()?;
    Ok(Json(response))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::{
        NetworkHealthResponse, NetworkPeerDto, NetworkPeersResponse, NetworkStatusResponse,
    };
    use crate::rest::test_helpers::mock_state;
    use crate::rest::NetworkBackend;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    struct MockNetworkBackend;

    impl NetworkBackend for MockNetworkBackend {
        fn network_peers(&self) -> Result<NetworkPeersResponse, RpcError> {
            Ok(NetworkPeersResponse {
                peers: vec![NetworkPeerDto {
                    peer_id: "abcd1234".into(),
                    is_validator: true,
                    stake: Some(1000),
                    reputation: 50,
                }],
                total: 1,
            })
        }

        fn network_status(&self) -> Result<NetworkStatusResponse, RpcError> {
            Ok(NetworkStatusResponse {
                known_peers: 10,
                known_validators: 4,
                filled_buckets: 5,
                total_buckets: 256,
                routing_healthy: true,
            })
        }

        fn network_health(&self) -> Result<NetworkHealthResponse, RpcError> {
            Ok(NetworkHealthResponse {
                status: "healthy".into(),
                peer_count: 10,
                routing_healthy: true,
            })
        }
    }

    fn test_state_with_network() -> Arc<AppState> {
        let base = mock_state();
        Arc::new(AppState {
            query: base.query.clone(),
            intent: None,
            consensus: None,
            network: Some(Arc::new(MockNetworkBackend)),
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
            block: None,
            event_backend: None,
        })
    }

    #[tokio::test]
    async fn peers_endpoint_returns_json() {
        let app = router().with_state(test_state_with_network());
        let req = Request::get("/v2/network/peers")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let parsed: NetworkPeersResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed.total, 1);
        assert!(parsed.peers[0].is_validator);
    }

    #[tokio::test]
    async fn status_endpoint_returns_json() {
        let app = router().with_state(test_state_with_network());
        let req = Request::get("/v2/network/status")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let parsed: NetworkStatusResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed.known_peers, 10);
        assert!(parsed.routing_healthy);
    }

    #[tokio::test]
    async fn health_endpoint_returns_json() {
        let app = router().with_state(test_state_with_network());
        let req = Request::get("/v2/network/health")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let parsed: NetworkHealthResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed.status, "healthy");
    }

    #[tokio::test]
    async fn endpoints_return_503_without_backend() {
        let state = mock_state(); // no network backend
        let app = router().with_state(state);

        for path in &[
            "/v2/network/peers",
            "/v2/network/status",
            "/v2/network/health",
        ] {
            let req = Request::get(*path).body(Body::empty()).unwrap();
            let resp = app.clone().oneshot(req).await.unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::SERVICE_UNAVAILABLE,
                "expected 503 for {path} without network backend"
            );
        }
    }
}
