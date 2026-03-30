// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Contract query endpoints.
//!
//! `POST /v2/contract/query` — execute a read-only view function.

use std::sync::Arc;
use std::time::Instant;

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};

use super::AppState;
use crate::dto::{ContractQueryRequest, ContractQueryResponse};
use crate::error::{RpcError, RpcResult};

/// Build the contract router fragment.
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/v2/contract/query", post(contract_query))
}

/// `POST /v2/contract/query`
///
/// Execute a read-only view function on a deployed contract.
/// Does not create a transaction — no signing required.
async fn contract_query(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ContractQueryRequest>,
) -> RpcResult<Json<ContractQueryResponse>> {
    let started_at = Instant::now();
    let query = Arc::clone(&state.query);
    let request_for_exec = request.clone();
    let timeout_ms = state.query_timeout_ms;

    let resp = tokio::time::timeout(
        std::time::Duration::from_millis(timeout_ms),
        tokio::task::spawn_blocking(move || query.contract_query(&request_for_exec)),
    )
    .await
    .map_err(|_| {
        tracing::warn!(
            contract = %request.contract,
            function = %request.function,
            timeout_ms,
            "contract query timed out"
        );
        RpcError::Unavailable(format!("contract query timed out after {timeout_ms} ms"))
    })
    .and_then(|join_result| {
        join_result
            .map_err(|err| RpcError::Internal(format!("contract query task join failed: {err}")))
    })??;

    if resp.gas_used > state.query_gas_budget {
        tracing::warn!(
            contract = %request.contract,
            function = %request.function,
            gas_used = resp.gas_used,
            gas_budget = state.query_gas_budget,
            elapsed_ms = started_at.elapsed().as_millis() as u64,
            "contract query exceeded configured gas budget"
        );
        return Err(RpcError::BadRequest(format!(
            "contract query exceeded gas budget: used {} > budget {}",
            resp.gas_used, state.query_gas_budget
        )));
    }

    tracing::info!(
        contract = %request.contract,
        function = %request.function,
        gas_used = resp.gas_used,
        gas_budget = state.query_gas_budget,
        elapsed_ms = started_at.elapsed().as_millis() as u64,
        "contract query completed"
    );

    Ok(Json(resp))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rest::{AppState, QueryBackend};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    struct QueryBackendMock {
        response: ContractQueryResponse,
        delay_ms: u64,
    }

    impl QueryBackend for QueryBackendMock {
        fn account_balance(
            &self,
            _address: &nexus_primitives::AccountAddress,
            _token: &nexus_primitives::TokenId,
        ) -> Result<nexus_primitives::Amount, RpcError> {
            Err(RpcError::Unavailable("not used in test".into()))
        }

        fn transaction_receipt(
            &self,
            _digest: &nexus_primitives::TxDigest,
        ) -> Result<Option<crate::dto::TransactionReceiptDto>, RpcError> {
            Err(RpcError::Unavailable("not used in test".into()))
        }

        fn health_status(&self) -> crate::dto::HealthResponse {
            crate::dto::HealthResponse {
                status: "healthy",
                version: "test",
                peers: 0,
                epoch: nexus_primitives::EpochNumber(0),
                latest_commit: nexus_primitives::CommitSequence(0),
                uptime_seconds: 0,
                subsystems: Vec::new(),
                reason: None,
            }
        }

        fn contract_query(
            &self,
            _request: &crate::dto::ContractQueryRequest,
        ) -> Result<crate::dto::ContractQueryResponse, RpcError> {
            if self.delay_ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(self.delay_ms));
            }
            Ok(self.response.clone())
        }
    }

    fn make_request() -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/v2/contract/query")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&ContractQueryRequest {
                    contract: "00".repeat(32),
                    function: "counter::get".into(),
                    type_args: Vec::new(),
                    args: Vec::new(),
                })
                .unwrap(),
            ))
            .unwrap()
    }

    fn test_state(
        response: ContractQueryResponse,
        query_gas_budget: u64,
        query_timeout_ms: u64,
    ) -> Arc<AppState> {
        Arc::new(AppState {
            query: Arc::new(QueryBackendMock {
                response,
                delay_ms: 0,
            }),
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
            max_ws_connections: 8,
            ws_connection_count: std::sync::atomic::AtomicUsize::new(0),
            intent_tracker: None,
            session_provenance: None,
            state_proof: None,
            mcp_dispatcher: None,
            mcp_call_index: std::sync::atomic::AtomicU64::new(0),
            quota_manager: None,
            query_gas_budget,
            query_timeout_ms,
            num_shards: 1,
            tx_lifecycle: None,
            htlc: None,
            block: None,
            event_backend: None,
        })
    }

    #[tokio::test]
    async fn contract_query_rejects_when_gas_budget_exceeded() {
        let state = test_state(
            ContractQueryResponse {
                return_value: None,
                gas_used: 25,
                gas_budget: 0,
            },
            10,
            1_000,
        );
        let app = router().with_state(state);

        let resp = app.oneshot(make_request()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn contract_query_times_out() {
        let state = Arc::new(AppState {
            query: Arc::new(QueryBackendMock {
                response: ContractQueryResponse {
                    return_value: None,
                    gas_used: 1,
                    gas_budget: 0,
                },
                delay_ms: 25,
            }),
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
            max_ws_connections: 8,
            ws_connection_count: std::sync::atomic::AtomicUsize::new(0),
            intent_tracker: None,
            session_provenance: None,
            state_proof: None,
            mcp_dispatcher: None,
            mcp_call_index: std::sync::atomic::AtomicU64::new(0),
            quota_manager: None,
            query_gas_budget: 100,
            query_timeout_ms: 1,
            num_shards: 1,
            tx_lifecycle: None,
            htlc: None,
            block: None,
            event_backend: None,
        });
        let app = router().with_state(state);

        let resp = app.oneshot(make_request()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
