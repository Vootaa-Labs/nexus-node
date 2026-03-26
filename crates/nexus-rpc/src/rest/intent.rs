//! Intent REST endpoints.
//!
//! `POST /v2/intent/submit` — submit a signed user intent.
//! `POST /v2/intent/estimate-gas` — estimate gas for an intent.
//! `GET  /v2/intent/:id/status` — query intent lifecycle status.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};

use super::AppState;
use crate::dto::{GasEstimateDto, IntentStatusDto, IntentSubmitResponse};
use crate::error::{RpcError, RpcResult};
use nexus_intent::types::SignedUserIntent;

async fn broadcast_compiled_plan(
    state: &AppState,
    plan: &nexus_intent::types::CompiledIntentPlan,
) -> RpcResult<()> {
    let broadcaster = state.broadcaster.as_ref().ok_or_else(|| {
        RpcError::Unavailable("transaction broadcast service not available".into())
    })?;

    for step in &plan.steps {
        let encoded = bcs::to_bytes(&step.transaction)
            .map_err(|e| RpcError::Serialization(format!("BCS encode failed: {e}")))?;
        broadcaster.broadcast_tx(encoded).await?;
    }

    Ok(())
}

/// Build the intent router fragment.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v2/intent/submit", post(submit_intent))
        .route("/v2/intent/estimate-gas", post(estimate_gas))
        .route("/v2/intent/:id/status", get(intent_status))
}

/// `POST /v2/intent/submit`
///
/// Accepts a JSON-encoded [`SignedUserIntent`], compiles it, and returns
/// the compiled plan summary.
async fn submit_intent(
    State(state): State<Arc<AppState>>,
    Json(intent): Json<SignedUserIntent>,
) -> RpcResult<Json<IntentSubmitResponse>> {
    let intent_id = intent.digest;
    let backend = state
        .intent
        .as_ref()
        .ok_or_else(|| RpcError::Unavailable("intent service not available".into()))?;
    let result = backend.submit_intent(intent).await;

    match result {
        Ok(plan) => {
            broadcast_compiled_plan(&state, &plan).await?;

            // Register intent in the lifecycle tracker
            let tx_hashes: Vec<_> = plan.steps.iter().map(|s| s.transaction.digest).collect();
            if let Some(tracker) = &state.intent_tracker {
                tracker.register(plan.intent_id, tx_hashes);
            }

            if let Some(events) = &state.events {
                let _ = events.send(crate::ws::NodeEvent::IntentStatusChanged(IntentStatusDto {
                    intent_id: plan.intent_id,
                    status: nexus_intent::types::IntentStatus::Submitted {
                        steps: plan.steps.len(),
                    },
                }));
            }
            Ok(Json(plan.into()))
        }
        Err(err) => {
            if let Some(events) = &state.events {
                let _ = events.send(crate::ws::NodeEvent::IntentStatusChanged(IntentStatusDto {
                    intent_id,
                    status: nexus_intent::types::IntentStatus::Failed {
                        reason: err.to_string(),
                    },
                }));
            }
            Err(err)
        }
    }
}

/// `POST /v2/intent/estimate-gas`
///
/// Accepts a JSON-encoded [`SignedUserIntent`] and returns a gas estimate
/// without executing anything.
async fn estimate_gas(
    State(state): State<Arc<AppState>>,
    Json(intent): Json<SignedUserIntent>,
) -> RpcResult<Json<GasEstimateDto>> {
    let backend = state
        .intent
        .as_ref()
        .ok_or_else(|| RpcError::Unavailable("intent service not available".into()))?;
    let estimate = backend.estimate_gas(intent).await?;
    Ok(Json(estimate.into()))
}

/// `GET /v2/intent/:id/status`
///
/// Returns the current lifecycle status of a tracked intent.
async fn intent_status(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> RpcResult<Json<IntentStatusDto>> {
    let tracker = state
        .intent_tracker
        .as_ref()
        .ok_or_else(|| RpcError::Unavailable("intent tracking not available".into()))?;

    let intent_id = nexus_primitives::Blake3Digest::from_hex(&id)
        .map_err(|e| RpcError::BadRequest(format!("invalid intent id: {e}")))?;

    let record = tracker
        .status(&intent_id)
        .ok_or_else(|| RpcError::NotFound(format!("intent {id} not tracked")))?;

    Ok(Json(IntentStatusDto {
        intent_id: record.intent_id,
        status: record.status,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rest::test_helpers::{mock_state_with_intent, MockBroadcaster, MockIntentBackend};
    use crate::ws::NodeEvent;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use nexus_crypto::{DilithiumSigner, Signer};
    use nexus_execution::types::{
        compute_tx_digest, SignedTransaction, TransactionBody, TransactionPayload, TX_DOMAIN,
    };
    use nexus_intent::types::{CompiledIntentPlan, GasEstimate, IntentStep, UserIntent};
    use nexus_primitives::*;
    use tower::ServiceExt;

    fn sample_signed_intent() -> SignedUserIntent {
        let (sk, vk) = DilithiumSigner::generate_keypair();
        let intent = UserIntent::Transfer {
            to: AccountAddress([0xBB; 32]),
            token: TokenId::Native,
            amount: Amount(1_000),
        };
        let intent_bytes = serde_json::to_vec(&intent).unwrap();
        let sig = DilithiumSigner::sign(&sk, b"nexus-intent", &intent_bytes);
        let digest = nexus_primitives::Blake3Digest([0xCC; 32]);
        SignedUserIntent {
            intent,
            sender: AccountAddress([0xAA; 32]),
            signature: sig,
            sender_pk: vk,
            nonce: 1,
            created_at: TimestampMs(1_700_000_000_000),
            digest,
        }
    }

    fn sample_signed_tx(sequence: u64) -> SignedTransaction {
        let (sk, pk) = DilithiumSigner::generate_keypair();
        let body = TransactionBody {
            sender: AccountAddress([0xAA; 32]),
            sequence_number: sequence,
            expiry_epoch: EpochNumber(100),
            gas_limit: 50_000,
            gas_price: 1,
            target_shard: Some(ShardId(0)),
            payload: TransactionPayload::Transfer {
                recipient: AccountAddress([0xBB; 32]),
                amount: Amount(100),
                token: TokenId::Native,
            },
            chain_id: 1,
        };
        let digest = compute_tx_digest(&body).unwrap();
        let signature = DilithiumSigner::sign(&sk, TX_DOMAIN, digest.as_bytes());
        SignedTransaction {
            body,
            signature,
            sender_pk: pk,
            digest,
        }
    }

    #[tokio::test]
    async fn submit_intent_returns_200() {
        let plan = CompiledIntentPlan {
            intent_id: Blake3Digest([0xCC; 32]),
            steps: vec![IntentStep {
                shard_id: ShardId(0),
                transaction: sample_signed_tx(1),
                depends_on: vec![],
            }],
            requires_htlc: false,
            estimated_gas: 10_000,
            expires_at: EpochNumber(100),
        };
        let backend = MockIntentBackend::new().with_submit_result(Ok(plan));
        let mut state = mock_state_with_intent(backend);
        Arc::get_mut(&mut state).unwrap().broadcaster = Some(Arc::new(MockBroadcaster::new()));
        let app = router().with_state(state);

        let intent = sample_signed_intent();
        let body = serde_json::to_string(&intent).unwrap();
        let req = Request::builder()
            .method("POST")
            .uri("/v2/intent/submit")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let response: IntentSubmitResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(response.intent_id, Blake3Digest([0xCC; 32]));
        assert_eq!(response.estimated_gas, 10_000);
    }

    #[tokio::test]
    async fn submit_intent_emits_submitted_event() {
        let plan = CompiledIntentPlan {
            intent_id: Blake3Digest([0xCC; 32]),
            steps: vec![IntentStep {
                shard_id: ShardId(0),
                transaction: sample_signed_tx(1),
                depends_on: vec![],
            }],
            requires_htlc: false,
            estimated_gas: 10_000,
            expires_at: EpochNumber(100),
        };
        let backend = MockIntentBackend::new().with_submit_result(Ok(plan));
        let (tx, mut rx) = crate::ws::event_channel();
        let mut state = mock_state_with_intent(backend);
        Arc::get_mut(&mut state).unwrap().events = Some(tx);
        Arc::get_mut(&mut state).unwrap().broadcaster = Some(Arc::new(MockBroadcaster::new()));
        let app = router().with_state(state);

        let intent = sample_signed_intent();
        let body = serde_json::to_string(&intent).unwrap();
        let req = Request::builder()
            .method("POST")
            .uri("/v2/intent/submit")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let event = rx.try_recv().unwrap();
        match event {
            NodeEvent::IntentStatusChanged(dto) => {
                assert_eq!(dto.intent_id, Blake3Digest([0xCC; 32]));
                assert_eq!(
                    dto.status,
                    nexus_intent::types::IntentStatus::Submitted { steps: 1 }
                );
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn submit_intent_returns_422_on_error() {
        let backend = MockIntentBackend::new().with_submit_result(Err(
            nexus_intent::IntentError::InsufficientBalance {
                account: AccountAddress([0xAA; 32]),
                token: TokenId::Native,
                available: 100,
                required: 1_000,
            }
            .into(),
        ));
        let state = mock_state_with_intent(backend);
        let app = router().with_state(state);

        let intent = sample_signed_intent();
        let body = serde_json::to_string(&intent).unwrap();
        let req = Request::builder()
            .method("POST")
            .uri("/v2/intent/submit")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn submit_intent_broadcasts_compiled_transactions() {
        let tx = sample_signed_tx(7);
        let expected = bcs::to_bytes(&tx).unwrap();
        let plan = CompiledIntentPlan {
            intent_id: Blake3Digest([0xCC; 32]),
            steps: vec![IntentStep {
                shard_id: ShardId(0),
                transaction: tx,
                depends_on: vec![],
            }],
            requires_htlc: false,
            estimated_gas: 10_000,
            expires_at: EpochNumber(100),
        };
        let backend = MockIntentBackend::new().with_submit_result(Ok(plan));
        let broadcaster = Arc::new(MockBroadcaster::new());
        let broadcaster_ref = Arc::clone(&broadcaster);
        let mut state = mock_state_with_intent(backend);
        Arc::get_mut(&mut state).unwrap().broadcaster =
            Some(broadcaster as Arc<dyn crate::rest::TransactionBroadcaster>);
        let app = router().with_state(state);

        let intent = sample_signed_intent();
        let body = serde_json::to_string(&intent).unwrap();
        let req = Request::builder()
            .method("POST")
            .uri("/v2/intent/submit")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let payloads = broadcaster_ref
            .payloads
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        assert_eq!(payloads.len(), 1);
        assert_eq!(payloads[0], expected);
    }

    #[tokio::test]
    async fn submit_intent_emits_failed_event() {
        let backend = MockIntentBackend::new().with_submit_result(Err(
            nexus_intent::IntentError::InsufficientBalance {
                account: AccountAddress([0xAA; 32]),
                token: TokenId::Native,
                available: 100,
                required: 1_000,
            }
            .into(),
        ));
        let (tx, mut rx) = crate::ws::event_channel();
        let mut state = mock_state_with_intent(backend);
        Arc::get_mut(&mut state).unwrap().events = Some(tx);
        let app = router().with_state(state);

        let intent = sample_signed_intent();
        let req = Request::builder()
            .method("POST")
            .uri("/v2/intent/submit")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&intent).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let event = rx.try_recv().unwrap();
        match event {
            NodeEvent::IntentStatusChanged(dto) => {
                assert_eq!(dto.intent_id, intent.digest);
                assert!(matches!(
                    dto.status,
                    nexus_intent::types::IntentStatus::Failed { .. }
                ));
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn estimate_gas_returns_200() {
        let estimate = GasEstimate {
            gas_units: 25_000,
            shards_touched: 1,
            requires_cross_shard: false,
        };
        let backend = MockIntentBackend::new().with_estimate_result(Ok(estimate));
        let state = mock_state_with_intent(backend);
        let app = router().with_state(state);

        let intent = sample_signed_intent();
        let body = serde_json::to_string(&intent).unwrap();
        let req = Request::builder()
            .method("POST")
            .uri("/v2/intent/estimate-gas")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let dto: GasEstimateDto = serde_json::from_slice(&body).unwrap();
        assert_eq!(dto.gas_units, 25_000);
        assert!(!dto.requires_cross_shard);
    }

    #[tokio::test]
    async fn submit_intent_returns_503_when_no_backend() {
        let state = Arc::new(AppState {
            query: Arc::new(crate::rest::test_helpers::MockQueryBackend::new()),
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
        });
        let app = router().with_state(state);

        let intent = sample_signed_intent();
        let body = serde_json::to_string(&intent).unwrap();
        let req = Request::builder()
            .method("POST")
            .uri("/v2/intent/submit")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
