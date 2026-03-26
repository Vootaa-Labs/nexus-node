// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for the nexus-rpc HTTP API.
//!
//! These tests build the full router via `RpcService::builder()` and exercise
//! the public HTTP surface end-to-end — including middleware (CORS, request-id,
//! tracing) and JSON serialization.

use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use http::{Method, Request, StatusCode};
use tower::ServiceExt;

use nexus_primitives::*;
use nexus_rpc::dto::*;
use nexus_rpc::error::{RpcError, RpcResult};
use nexus_rpc::{
    ConsensusBackend, IntentBackend, QueryBackend, RpcService, TransactionBroadcaster,
};

// ═══════════════════════════════════════════════════════════════════════
// Mock backends for integration tests
// ═══════════════════════════════════════════════════════════════════════

struct TestQueryBackend {
    balances: Mutex<HashMap<(AccountAddress, TokenId), Amount>>,
    receipts: Mutex<HashMap<TxDigest, TransactionReceiptDto>>,
}

impl TestQueryBackend {
    fn new() -> Self {
        Self {
            balances: Mutex::new(HashMap::new()),
            receipts: Mutex::new(HashMap::new()),
        }
    }

    fn with_balance(self, addr: AccountAddress, token: TokenId, amount: Amount) -> Self {
        self.balances.lock().unwrap().insert((addr, token), amount);
        self
    }

    fn with_receipt(self, digest: TxDigest, receipt: TransactionReceiptDto) -> Self {
        self.receipts.lock().unwrap().insert(digest, receipt);
        self
    }
}

impl QueryBackend for TestQueryBackend {
    fn account_balance(
        &self,
        address: &AccountAddress,
        token: &TokenId,
    ) -> Result<Amount, RpcError> {
        self.balances
            .lock()
            .unwrap()
            .get(&(*address, *token))
            .copied()
            .ok_or_else(|| RpcError::NotFound(format!("account {address:?} not found")))
    }

    fn transaction_receipt(
        &self,
        digest: &TxDigest,
    ) -> Result<Option<TransactionReceiptDto>, RpcError> {
        Ok(self.receipts.lock().unwrap().get(digest).cloned())
    }

    fn health_status(&self) -> HealthResponse {
        HealthResponse {
            status: "healthy",
            version: env!("CARGO_PKG_VERSION"),
            peers: 3,
            epoch: EpochNumber(42),
            latest_commit: CommitSequence(1000),
            uptime_seconds: 0,
            subsystems: Vec::new(),
            reason: None,
        }
    }

    fn contract_query(
        &self,
        _request: &nexus_rpc::ContractQueryRequest,
    ) -> Result<nexus_rpc::ContractQueryResponse, RpcError> {
        Err(RpcError::Unavailable(
            "contract query not implemented in test backend".into(),
        ))
    }
}

struct TestConsensusBackend {
    validators: Mutex<Vec<ValidatorInfoDto>>,
    status: Mutex<Option<ConsensusStatusDto>>,
}

impl TestConsensusBackend {
    fn new() -> Self {
        Self {
            validators: Mutex::new(Vec::new()),
            status: Mutex::new(None),
        }
    }

    fn with_validators(self, validators: Vec<ValidatorInfoDto>) -> Self {
        *self.validators.lock().unwrap() = validators;
        self
    }

    fn with_status(self, status: ConsensusStatusDto) -> Self {
        *self.status.lock().unwrap() = Some(status);
        self
    }
}

impl ConsensusBackend for TestConsensusBackend {
    fn active_validators(&self) -> RpcResult<Vec<ValidatorInfoDto>> {
        Ok(self.validators.lock().unwrap().clone())
    }

    fn validator_info(&self, index: ValidatorIndex) -> RpcResult<ValidatorInfoDto> {
        self.validators
            .lock()
            .unwrap()
            .iter()
            .find(|v| v.index == index)
            .cloned()
            .ok_or_else(|| RpcError::NotFound(format!("validator {} not found", index.0)))
    }

    fn consensus_status(&self) -> RpcResult<ConsensusStatusDto> {
        self.status
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| RpcError::Internal("no status".into()))
    }
}

struct TestIntentBackend {
    submit_result: Mutex<Option<RpcResult<nexus_intent::types::CompiledIntentPlan>>>,
    estimate_result: Mutex<Option<RpcResult<nexus_intent::types::GasEstimate>>>,
}

impl IntentBackend for TestIntentBackend {
    fn submit_intent(
        &self,
        _intent: nexus_intent::types::SignedUserIntent,
    ) -> Pin<Box<dyn Future<Output = RpcResult<nexus_intent::types::CompiledIntentPlan>> + Send + '_>>
    {
        let result = self
            .submit_result
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| Err(RpcError::Internal("no mock result".into())));
        Box::pin(std::future::ready(result))
    }

    fn estimate_gas(
        &self,
        _intent: nexus_intent::types::SignedUserIntent,
    ) -> Pin<Box<dyn Future<Output = RpcResult<nexus_intent::types::GasEstimate>> + Send + '_>>
    {
        let result = self
            .estimate_result
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| Err(RpcError::Internal("no mock result".into())));
        Box::pin(std::future::ready(result))
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════

fn listen_addr() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

/// Build a router with only the query backend (no intent or consensus).
fn basic_router() -> axum::Router {
    RpcService::builder(listen_addr())
        .query_backend(Arc::new(TestQueryBackend::new()))
        .build()
        .into_router()
}

/// Build a router with all backends configured.
fn full_router(
    query: TestQueryBackend,
    intent: Option<TestIntentBackend>,
    consensus: Option<TestConsensusBackend>,
) -> axum::Router {
    let mut builder = RpcService::builder(listen_addr()).query_backend(Arc::new(query));

    if let Some(i) = intent {
        builder = builder.intent_backend(Arc::new(i));
    }
    if let Some(c) = consensus {
        builder = builder.consensus_backend(Arc::new(c));
    }

    builder.build().into_router()
}

async fn body_to_json(body: Body) -> serde_json::Value {
    let bytes = axum::body::to_bytes(body, 1_048_576).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

// ═══════════════════════════════════════════════════════════════════════
// Health endpoint
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn health_endpoint_returns_ok() {
    let app = basic_router();
    let req = Request::builder()
        .uri("/health")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_to_json(resp.into_body()).await;
    assert_eq!(json["status"], "healthy");
    assert_eq!(json["peers"], 3);
    assert_eq!(json["epoch"], 42);
}

// ═══════════════════════════════════════════════════════════════════════
// CORS middleware
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn cors_allows_any_origin() {
    // B-6: CORS is now fail-closed — empty origins blocks all cross-origin
    // requests.  Explicitly pass ["*"] to allow any origin.
    let app = RpcService::builder(listen_addr())
        .query_backend(Arc::new(TestQueryBackend::new()))
        .cors_allowed_origins(vec!["*".to_owned()])
        .build()
        .into_router();
    let req = Request::builder()
        .method(Method::OPTIONS)
        .uri("/health")
        .header("origin", "https://example.com")
        .header("access-control-request-method", "GET")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    // CORS preflight returns 200
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers().contains_key("access-control-allow-origin"));
}

// ═══════════════════════════════════════════════════════════════════════
// Request-ID propagation
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn request_id_header_is_set() {
    let app = basic_router();
    let req = Request::builder()
        .uri("/health")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert!(
        resp.headers().contains_key("x-request-id"),
        "response should contain x-request-id header"
    );
}

#[tokio::test]
async fn request_id_header_is_propagated() {
    let app = basic_router();
    let req = Request::builder()
        .uri("/health")
        .header("x-request-id", "test-correlation-123")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(
        resp.headers().get("x-request-id").unwrap(),
        "test-correlation-123"
    );
}

// ═══════════════════════════════════════════════════════════════════════
// Account balance endpoint
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn account_balance_returns_balance() {
    let addr = AccountAddress([0xAA; 32]);
    let token = TokenId::Native;
    let amount = Amount(1_000_000);

    let query = TestQueryBackend::new().with_balance(addr, token, amount);
    let app = full_router(query, None, None);

    let hex_addr = hex::encode(addr.0);
    let req = Request::builder()
        .uri(format!("/v2/account/{hex_addr}/balance"))
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_to_json(resp.into_body()).await;
    assert!(json["balances"].is_array());
}

#[tokio::test]
async fn account_balance_not_found() {
    let app = basic_router();
    let fake_addr = hex::encode([0xBB; 32]);
    let req = Request::builder()
        .uri(format!("/v2/account/{fake_addr}/balance"))
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    let json = body_to_json(resp.into_body()).await;
    assert!(json["error"].as_str().unwrap().contains("NOT_FOUND"));
}

#[tokio::test]
async fn account_balance_bad_hex_address() {
    let app = basic_router();
    let req = Request::builder()
        .uri("/v2/account/not-valid-hex/balance")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ═══════════════════════════════════════════════════════════════════════
// Transaction receipt endpoint
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn transaction_receipt_found() {
    let digest = Blake3Digest([0xCC; 32]);
    let receipt = TransactionReceiptDto {
        tx_digest: digest,
        commit_seq: CommitSequence(50),
        shard_id: ShardId(1),
        status: ExecutionStatusDto::Success,
        gas_used: 42_000,
        timestamp: TimestampMs(1_700_000_000_000),
    };

    let query = TestQueryBackend::new().with_receipt(digest, receipt);
    let app = full_router(query, None, None);

    let hex_hash = hex::encode(digest.0);
    let req = Request::builder()
        .uri(format!("/v2/tx/{hex_hash}/status"))
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_to_json(resp.into_body()).await;
    assert_eq!(json["status"], "Success");
    assert_eq!(json["gas_used"], 42_000);
}

#[tokio::test]
async fn transaction_receipt_not_found() {
    let app = basic_router();
    let fake_hash = hex::encode([0xDD; 32]);
    let req = Request::builder()
        .uri(format!("/v2/tx/{fake_hash}/status"))
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ═══════════════════════════════════════════════════════════════════════
// Validator / consensus endpoints
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn validators_list() {
    let validators = vec![
        ValidatorInfoDto {
            index: ValidatorIndex(0),
            public_key_hex: hex::encode([0x01; 32]),
            stake: Amount(1_000_000),
            reputation: 9_500,
            is_slashed: false,
            shard_id: Some(ShardId(0)),
        },
        ValidatorInfoDto {
            index: ValidatorIndex(1),
            public_key_hex: hex::encode([0x02; 32]),
            stake: Amount(500_000),
            reputation: 8_000,
            is_slashed: false,
            shard_id: None,
        },
    ];

    let consensus = TestConsensusBackend::new().with_validators(validators);
    let app = full_router(TestQueryBackend::new(), None, Some(consensus));

    let req = Request::builder()
        .uri("/v2/validators")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_to_json(resp.into_body()).await;
    let arr = json.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["index"], 0);
    assert_eq!(arr[1]["index"], 1);
}

#[tokio::test]
async fn validator_by_index() {
    let validators = vec![ValidatorInfoDto {
        index: ValidatorIndex(7),
        public_key_hex: hex::encode([0xAA; 32]),
        stake: Amount(2_000_000),
        reputation: 9_000,
        is_slashed: false,
        shard_id: Some(ShardId(3)),
    }];

    let consensus = TestConsensusBackend::new().with_validators(validators);
    let app = full_router(TestQueryBackend::new(), None, Some(consensus));

    let req = Request::builder()
        .uri("/v2/validators/7")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_to_json(resp.into_body()).await;
    assert_eq!(json["index"], 7);
    assert_eq!(json["stake"], 2_000_000);
}

#[tokio::test]
async fn validator_not_found() {
    let consensus = TestConsensusBackend::new().with_validators(vec![]);
    let app = full_router(TestQueryBackend::new(), None, Some(consensus));

    let req = Request::builder()
        .uri("/v2/validators/999")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn consensus_status_endpoint() {
    let status = ConsensusStatusDto {
        epoch: EpochNumber(10),
        dag_size: 5_000,
        total_commits: 12_345,
        pending_commits: 3,
    };

    let consensus = TestConsensusBackend::new().with_status(status);
    let app = full_router(TestQueryBackend::new(), None, Some(consensus));

    let req = Request::builder()
        .uri("/v2/consensus/status")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_to_json(resp.into_body()).await;
    assert_eq!(json["epoch"], 10);
    assert_eq!(json["total_commits"], 12_345);
}

// ═══════════════════════════════════════════════════════════════════════
// Optional backend → 503 Unavailable
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn validators_503_when_no_consensus_backend() {
    let app = basic_router(); // no consensus backend
    let req = Request::builder()
        .uri("/v2/validators")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn consensus_status_503_when_unavailable() {
    let app = basic_router();
    let req = Request::builder()
        .uri("/v2/consensus/status")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

// ═══════════════════════════════════════════════════════════════════════
// Gas estimation endpoint
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn estimate_gas_rejects_invalid_body() {
    // Axum's Json extractor rejects the body before the handler can check
    // backend availability, so we get 422 (Unprocessable Entity).
    let app = basic_router();
    let req = Request::builder()
        .method(Method::POST)
        .uri("/v2/intent/estimate-gas")
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn submit_intent_rejects_invalid_body() {
    let app = basic_router();
    let req = Request::builder()
        .method(Method::POST)
        .uri("/v2/intent/submit")
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

// ═══════════════════════════════════════════════════════════════════════
// 404 for unknown routes
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn unknown_route_returns_404() {
    let app = basic_router();
    let req = Request::builder()
        .uri("/v2/nonexistent")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ═══════════════════════════════════════════════════════════════════════
// Full TCP server lifecycle
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn tcp_server_serves_and_shuts_down() {
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    let svc = RpcService::builder("127.0.0.1:0".parse().unwrap())
        .query_backend(Arc::new(TestQueryBackend::new()))
        .build();

    let handle = tokio::spawn(async move {
        svc.serve(async {
            shutdown_rx.await.ok();
        })
        .await
    });

    // Give server a moment to bind.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Signal shutdown.
    let _ = shutdown_tx.send(());
    let result = handle.await.unwrap();
    assert!(result.is_ok());
}

// ═══════════════════════════════════════════════════════════════════════
// Rate limiting integration
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn rate_limiter_integration() {
    // Build a service with very tight rate limit (2 requests per 10s window).
    let svc = RpcService::builder(listen_addr())
        .query_backend(Arc::new(TestQueryBackend::new()))
        .rate_limit(2, Duration::from_secs(10))
        .build();

    assert!(svc.state().rate_limiter.is_some());
}

// ═══════════════════════════════════════════════════════════════════════
// Intent lifecycle E2E — submit → track → confirm → query
// ═══════════════════════════════════════════════════════════════════════

/// Build a signed test transaction with a deterministic digest.
fn build_test_signed_tx(seq: u64) -> nexus_execution::types::SignedTransaction {
    use nexus_crypto::{DilithiumSigner, Signer};
    use nexus_execution::types::{
        compute_tx_digest, SignedTransaction, TransactionBody, TransactionPayload, TX_DOMAIN,
    };

    let (sk, pk) = DilithiumSigner::generate_keypair();
    let body = TransactionBody {
        sender: AccountAddress([0xAA; 32]),
        sequence_number: seq,
        expiry_epoch: nexus_primitives::EpochNumber(100),
        gas_limit: 50_000,
        gas_price: 1,
        target_shard: Some(nexus_primitives::ShardId(0)),
        payload: TransactionPayload::Transfer {
            recipient: AccountAddress([0xBB; 32]),
            amount: nexus_primitives::Amount(1_000),
            token: nexus_primitives::TokenId::Native,
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

/// Build a mock `CompiledIntentPlan` with the given transactions.
fn build_test_plan(
    intent_id: Blake3Digest,
    txs: Vec<nexus_execution::types::SignedTransaction>,
) -> nexus_intent::types::CompiledIntentPlan {
    use nexus_intent::types::{CompiledIntentPlan, IntentStep};

    let steps = txs
        .into_iter()
        .map(|tx| IntentStep {
            shard_id: nexus_primitives::ShardId(0),
            transaction: tx,
            depends_on: vec![],
        })
        .collect();

    CompiledIntentPlan {
        intent_id,
        steps,
        requires_htlc: false,
        estimated_gas: 20_000,
        expires_at: nexus_primitives::EpochNumber(200),
    }
}

/// Build the intent router with tracker and mock intent backend wired in.
fn intent_lifecycle_router(
    plan: nexus_intent::types::CompiledIntentPlan,
    tracker: Arc<nexus_rpc::IntentTracker>,
) -> axum::Router {
    let intent_backend = TestIntentBackend {
        submit_result: Mutex::new(Some(Ok(plan))),
        estimate_result: Mutex::new(None),
    };
    let broadcaster = TestBroadcaster;

    RpcService::builder(listen_addr())
        .query_backend(Arc::new(TestQueryBackend::new()))
        .intent_backend(Arc::new(intent_backend))
        .tx_broadcaster(Arc::new(broadcaster))
        .intent_tracker(tracker)
        .build()
        .into_router()
}

/// No-op broadcaster for lifecycle tests (we only care about tracking).
struct TestBroadcaster;

impl TransactionBroadcaster for TestBroadcaster {
    fn broadcast_tx(
        &self,
        _data: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = nexus_rpc::error::RpcResult<()>> + Send + '_>> {
        Box::pin(std::future::ready(Ok(())))
    }
}

/// Helper: build a JSON-serialisable `SignedUserIntent` with a known digest.
fn build_test_signed_intent(digest: Blake3Digest) -> nexus_intent::types::SignedUserIntent {
    use nexus_crypto::{DilithiumSigner, Signer};
    use nexus_intent::types::{SignedUserIntent, UserIntent};

    let (sk, vk) = DilithiumSigner::generate_keypair();
    let intent = UserIntent::Transfer {
        to: AccountAddress([0xBB; 32]),
        token: nexus_primitives::TokenId::Native,
        amount: nexus_primitives::Amount(1_000),
    };
    let intent_bytes = serde_json::to_vec(&intent).unwrap();
    let sig = DilithiumSigner::sign(&sk, b"nexus-intent", &intent_bytes);
    SignedUserIntent {
        intent,
        sender: AccountAddress([0xAA; 32]),
        signature: sig,
        sender_pk: vk,
        nonce: 1,
        created_at: nexus_primitives::TimestampMs(1_700_000_000_000),
        digest,
    }
}

/// E2E: Submit intent → response contains tx_hashes → status is Submitted.
#[tokio::test]
async fn intent_lifecycle_submit_and_query_status() {
    let intent_id = Blake3Digest([0xE1; 32]);
    let tx1 = build_test_signed_tx(1);
    let tx2 = build_test_signed_tx(2);
    let tx1_digest = tx1.digest;
    let tx2_digest = tx2.digest;
    let plan = build_test_plan(intent_id, vec![tx1, tx2]);

    let tracker = Arc::new(nexus_rpc::IntentTracker::new());
    let app = intent_lifecycle_router(plan, Arc::clone(&tracker));

    // ── Step 1: Submit the intent ──────────────────────────────────
    let signed = build_test_signed_intent(intent_id);
    let req = Request::builder()
        .method(Method::POST)
        .uri("/v2/intent/submit")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&signed).unwrap()))
        .unwrap();

    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_to_json(resp.into_body()).await;
    let resp_tx_hashes = json["tx_hashes"].as_array().unwrap();
    assert_eq!(
        resp_tx_hashes.len(),
        2,
        "response should include 2 tx_hashes"
    );

    // ── Step 2: Query intent status — should be Submitted ──────────
    let hex_id = hex::encode(intent_id.0);
    let req = Request::builder()
        .uri(format!("/v2/intent/{hex_id}/status"))
        .body(Body::empty())
        .unwrap();

    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_to_json(resp.into_body()).await;
    let status_obj = &json["status"];
    assert_eq!(
        status_obj["Submitted"]["steps"], 2,
        "intent should be in Submitted state with 2 steps"
    );

    // ── Step 3: Simulate first tx execution via tracker ────────────
    let result = tracker.on_tx_executed(&tx1_digest, 8_000);
    assert!(result.is_some());
    // Still Submitted (1 of 2 confirmed)
    let record = tracker.status(&intent_id).unwrap();
    assert!(
        matches!(
            record.status,
            nexus_intent::types::IntentStatus::Submitted { .. }
        ),
        "should still be Submitted after 1/2 confirmations"
    );

    // ── Step 4: Simulate second tx execution → Completed ───────────
    let result = tracker.on_tx_executed(&tx2_digest, 12_000);
    assert!(result.is_some());
    let (_intent_id, status) = result.unwrap();
    assert!(
        matches!(
            status,
            nexus_intent::types::IntentStatus::Completed { gas_used: 20_000 }
        ),
        "should be Completed with total gas 20_000, got: {status:?}"
    );

    // ── Step 5: Query intent status via HTTP — should be Completed ─
    let req = Request::builder()
        .uri(format!("/v2/intent/{hex_id}/status"))
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_to_json(resp.into_body()).await;
    let status_obj = &json["status"];
    assert_eq!(
        status_obj["Completed"]["gas_used"], 20_000,
        "HTTP status endpoint should show Completed with gas_used=20_000"
    );
}

/// E2E: Submit intent → one tx fails → status is Failed.
#[tokio::test]
async fn intent_lifecycle_failure_propagation() {
    let intent_id = Blake3Digest([0xE2; 32]);
    let tx1 = build_test_signed_tx(10);
    let tx2 = build_test_signed_tx(11);
    let tx1_digest = tx1.digest;
    let plan = build_test_plan(intent_id, vec![tx1, tx2]);

    let tracker = Arc::new(nexus_rpc::IntentTracker::new());
    let app = intent_lifecycle_router(plan, Arc::clone(&tracker));

    // Submit the intent
    let signed = build_test_signed_intent(intent_id);
    let req = Request::builder()
        .method(Method::POST)
        .uri("/v2/intent/submit")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&signed).unwrap()))
        .unwrap();

    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // First tx fails
    let result = tracker.on_tx_failed(&tx1_digest, "MoveAbort: out of gas".into());
    assert!(result.is_some());

    // Query status via HTTP — should be Failed
    let hex_id = hex::encode(intent_id.0);
    let req = Request::builder()
        .uri(format!("/v2/intent/{hex_id}/status"))
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_to_json(resp.into_body()).await;
    let reason = json["status"]["Failed"]["reason"]
        .as_str()
        .expect("should have a failure reason");
    assert!(
        reason.contains("out of gas"),
        "failure reason should mention the cause: {reason}"
    );
}

/// E2E: Query status for unknown intent → 404.
#[tokio::test]
async fn intent_status_not_found() {
    let tracker = Arc::new(nexus_rpc::IntentTracker::new());

    // Build router with tracker but no intents registered
    let app = RpcService::builder(listen_addr())
        .query_backend(Arc::new(TestQueryBackend::new()))
        .intent_tracker(tracker)
        .build()
        .into_router();

    let fake_id = hex::encode([0xFF; 32]);
    let req = Request::builder()
        .uri(format!("/v2/intent/{fake_id}/status"))
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// E2E: Query intent status when tracker is not configured → 503.
#[tokio::test]
async fn intent_status_503_when_no_tracker() {
    let app = basic_router(); // no intent_tracker
    let fake_id = hex::encode([0xFF; 32]);
    let req = Request::builder()
        .uri(format!("/v2/intent/{fake_id}/status"))
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}
