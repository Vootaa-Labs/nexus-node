// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Online MCP adapter endpoints.
//!
//! Exposes a minimal HTTP entry point for the MCP registry and tool calls,
//! backed by the existing MCP schema/handler stack.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;

use super::AppState;
use crate::error::{RpcError, RpcResult};
use crate::mcp::handler::handle_tool_call;
use crate::mcp::registry::McpToolKind;
use crate::mcp::schema::{McpToolCall, McpToolResult};

#[derive(Debug, Clone, Serialize)]
struct McpToolDescriptor {
    name: &'static str,
    description: &'static str,
    mutating: bool,
    may_require_confirmation: bool,
}

/// Build the MCP router fragment.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v2/mcp/tools", get(list_tools))
        .route("/v2/mcp/call", post(call_tool))
}

async fn list_tools() -> Json<Vec<McpToolDescriptor>> {
    Json(
        McpToolKind::all()
            .iter()
            .map(|tool| McpToolDescriptor {
                name: tool.tool_name(),
                description: tool.description(),
                mutating: tool.is_mutating(),
                may_require_confirmation: tool.may_require_confirmation(),
            })
            .collect(),
    )
}

async fn call_tool(
    State(state): State<Arc<AppState>>,
    Json(call): Json<McpToolCall>,
) -> RpcResult<Json<McpToolResult>> {
    let dispatcher = state
        .mcp_dispatcher
        .as_ref()
        .ok_or_else(|| RpcError::Unavailable("mcp dispatcher not available".into()))?;
    let call_index = state.mcp_call_index.fetch_add(1, Ordering::Relaxed);
    let deadline_ms = nexus_primitives::TimestampMs(
        nexus_primitives::TimestampMs::now()
            .0
            .saturating_add(state.query_timeout_ms),
    );

    Ok(Json(handle_tool_call(
        &call,
        dispatcher,
        call_index,
        deadline_ms,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rest::test_helpers::MockQueryBackend;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use nexus_crypto::{DilithiumSigner, Signer};
    use nexus_intent::agent_core::dispatcher::{DispatchBackend, DispatchOutcome};
    use nexus_intent::agent_core::envelope::AgentEnvelope;
    use nexus_intent::agent_core::planner::SimulationResult;
    use tower::ServiceExt;

    struct MockDispatcher;

    impl DispatchBackend for MockDispatcher {
        fn dispatch(
            &self,
            _envelope: &AgentEnvelope,
        ) -> nexus_intent::IntentResult<DispatchOutcome> {
            Ok(DispatchOutcome::Simulated(SimulationResult {
                session_id: nexus_primitives::Blake3Digest([0x01; 32]),
                plan_hash: nexus_primitives::Blake3Digest([0x02; 32]),
                estimated_gas: 123,
                step_count: 1,
                requires_cross_shard: false,
                simulated_at_ms: nexus_primitives::TimestampMs(1),
                summary: String::new(),
            }))
        }
    }

    fn test_state() -> Arc<AppState> {
        Arc::new(AppState {
            query: Arc::new(MockQueryBackend::new()),
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
            mcp_dispatcher: Some(Arc::new(MockDispatcher)),
            mcp_call_index: std::sync::atomic::AtomicU64::new(0),
            quota_manager: None,
            query_gas_budget: 10_000_000,
            query_timeout_ms: 5_000,
            num_shards: 1,
            tx_lifecycle: None,
            htlc: None,
        })
    }

    fn signed_call(tool: &str, arguments: serde_json::Value) -> serde_json::Value {
        let (sk, vk) = DilithiumSigner::generate_keypair();
        let caller = nexus_primitives::AccountAddress::from_dilithium_pubkey(vk.as_bytes());
        let args_bytes = serde_json::to_vec(&arguments).unwrap();
        let mut payload = Vec::new();
        payload.extend_from_slice(&caller.0);
        payload.extend_from_slice(tool.as_bytes());
        payload.extend_from_slice(&args_bytes);
        let sig = DilithiumSigner::sign(&sk, b"nexus::mcp::caller_auth::v1", &payload);

        serde_json::json!({
            "tool": tool,
            "arguments": arguments,
            "caller": hex::encode(caller.0),
            "caller_public_key": hex::encode(vk.as_bytes()),
            "caller_signature": hex::encode(sig.as_bytes()),
            "mcp_session_id": "test-session"
        })
    }

    #[tokio::test]
    async fn list_tools_returns_registry() {
        let app = router().with_state(test_state());
        let resp = app
            .oneshot(Request::get("/v2/mcp/tools").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn call_tool_routes_to_dispatcher() {
        let app = router().with_state(test_state());
        let call = signed_call(
            "simulate_intent",
            serde_json::json!({
                "intent_type": "transfer",
                "params": { "to": hex::encode([0x11; 32]), "amount": 1 }
            }),
        );
        let resp = app
            .oneshot(
                Request::post("/v2/mcp/call")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&call).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
