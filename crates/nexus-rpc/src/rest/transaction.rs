// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Transaction endpoints.
//!
//! `GET  /v2/tx/{hash}/status` — query transaction receipt by digest.
//! `POST /v2/tx/submit`        — submit a signed transaction for P2P broadcast.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};

use super::AppState;
use crate::dto::{TransactionReceiptDto, TxSubmitResponse};
use crate::error::{RpcError, RpcResult};
use nexus_execution::types::SignedTransaction;
use nexus_primitives::Blake3Digest;

/// Build the transaction router fragment.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v2/tx/:hash/status", get(tx_status))
        .route("/v2/tx/submit", post(submit_tx))
}

/// `GET /v2/tx/{hash}/status`
///
/// Returns the transaction receipt for the given digest (hex-encoded).
async fn tx_status(
    State(state): State<Arc<AppState>>,
    Path(hash_hex): Path<String>,
) -> RpcResult<Json<TransactionReceiptDto>> {
    let digest = parse_digest(&hash_hex)?;
    let receipt = state
        .query
        .transaction_receipt(&digest)?
        .ok_or_else(|| RpcError::NotFound(format!("transaction {hash_hex} not found")))?;
    Ok(Json(receipt))
}

/// `POST /v2/tx/submit`
///
/// Accepts a JSON-encoded [`SignedTransaction`], BCS-encodes it, and
/// broadcasts it to peers via the P2P gossip layer.
///
/// If the transaction's `target_shard` is `None`, the server auto-derives it
/// from the sender address using Jump Consistent Hash.
async fn submit_tx(
    State(state): State<Arc<AppState>>,
    Json(mut tx): Json<SignedTransaction>,
) -> RpcResult<Json<TxSubmitResponse>> {
    let broadcaster = state.broadcaster.as_ref().ok_or_else(|| {
        RpcError::Unavailable("transaction broadcast service not available".into())
    })?;

    // Auto-derive target_shard when the client omitted it.
    if tx.body.target_shard.is_none() && state.num_shards > 0 {
        let derived = nexus_intent::resolver::shard_lookup::jump_consistent_hash(
            &tx.body.sender,
            state.num_shards,
        );
        tx.body.target_shard = Some(derived);
    }

    let digest = tx.digest;
    let encoded = bcs::to_bytes(&tx)
        .map_err(|e| RpcError::Serialization(format!("BCS encode failed: {e}")))?;

    broadcaster.broadcast_tx(encoded).await?;

    Ok(Json(TxSubmitResponse {
        tx_digest: digest,
        accepted: true,
    }))
}

/// Parse a hex-encoded 32-byte digest.
fn parse_digest(hex_str: &str) -> Result<Blake3Digest, RpcError> {
    let bytes = hex::decode(hex_str)
        .map_err(|e| RpcError::BadRequest(format!("invalid hex digest: {e}")))?;
    if bytes.len() != 32 {
        return Err(RpcError::BadRequest(format!(
            "digest must be 32 bytes, got {}",
            bytes.len()
        )));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(Blake3Digest(arr))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::ExecutionStatusDto;
    use crate::rest::test_helpers::{
        mock_state, mock_state_with_broadcaster, state_with_backend, MockBroadcaster,
        MockQueryBackend,
    };
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use nexus_crypto::Signer;
    use nexus_execution::types::{
        compute_tx_digest, TransactionBody, TransactionPayload, TX_DOMAIN,
    };
    use nexus_primitives::{
        AccountAddress, Amount, CommitSequence, EpochNumber, ShardId, TimestampMs, TokenId,
    };
    use tower::ServiceExt;

    /// Build a minimal valid `SignedTransaction` for testing.
    fn sample_signed_tx() -> SignedTransaction {
        let sender = AccountAddress([0x01; 32]);
        let body = TransactionBody {
            sender,
            sequence_number: 1,
            expiry_epoch: EpochNumber(1000),
            gas_limit: 50_000,
            gas_price: 1,
            target_shard: None,
            payload: TransactionPayload::Transfer {
                recipient: AccountAddress([0x02; 32]),
                amount: Amount(100),
                token: TokenId::Native,
            },
            chain_id: 1,
        };
        let digest = compute_tx_digest(&body).unwrap();
        let (sk, pk) = nexus_crypto::DilithiumSigner::generate_keypair();
        let sig = nexus_crypto::DilithiumSigner::sign(&sk, TX_DOMAIN, digest.as_bytes());
        SignedTransaction {
            body,
            signature: sig,
            sender_pk: pk,
            digest,
        }
    }

    fn sample_receipt() -> TransactionReceiptDto {
        TransactionReceiptDto {
            tx_digest: Blake3Digest([0xAA; 32]),
            commit_seq: CommitSequence(42),
            shard_id: ShardId(0),
            status: ExecutionStatusDto::Success,
            gas_used: 5_000,
            timestamp: TimestampMs(1_700_000_000_000),
        }
    }

    #[tokio::test]
    async fn tx_status_returns_200_for_known_tx() {
        let receipt = sample_receipt();
        let backend =
            MockQueryBackend::new().with_receipt(Blake3Digest([0xAA; 32]), receipt.clone());
        let state = state_with_backend(backend);
        let app = router().with_state(state);

        let hash_hex = hex::encode([0xAA; 32]);
        let req = Request::builder()
            .uri(format!("/v2/tx/{hash_hex}/status"))
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let dto: TransactionReceiptDto = serde_json::from_slice(&body).unwrap();
        assert_eq!(dto.tx_digest, Blake3Digest([0xAA; 32]));
        assert_eq!(dto.gas_used, 5_000);
    }

    #[tokio::test]
    async fn tx_status_returns_404_for_unknown_tx() {
        let state = state_with_backend(MockQueryBackend::new());
        let app = router().with_state(state);

        let hash_hex = hex::encode([0xBB; 32]);
        let req = Request::builder()
            .uri(format!("/v2/tx/{hash_hex}/status"))
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn tx_status_returns_400_for_invalid_hex() {
        let state = state_with_backend(MockQueryBackend::new());
        let app = router().with_state(state);

        let req = Request::builder()
            .uri("/v2/tx/not-valid-hex/status")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn parse_digest_valid() {
        let hex_str = hex::encode([0xDD; 32]);
        let result = parse_digest(&hex_str);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Blake3Digest([0xDD; 32]));
    }

    #[test]
    fn parse_digest_wrong_length() {
        let hex_str = hex::encode([0xEE; 16]);
        let result = parse_digest(&hex_str);
        assert!(result.is_err());
    }

    // ── submit_tx tests ─────────────────────────────────────────────────

    #[tokio::test]
    async fn submit_tx_returns_200_on_success() {
        let broadcaster = MockBroadcaster::new();
        let state = mock_state_with_broadcaster(broadcaster);
        let app = router().with_state(state);

        let tx = sample_signed_tx();
        let body = serde_json::to_vec(&tx).unwrap();

        let req = Request::builder()
            .method("POST")
            .uri("/v2/tx/submit")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let dto: TxSubmitResponse = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(dto.tx_digest, tx.digest);
        assert!(dto.accepted);
    }

    #[tokio::test]
    async fn submit_tx_returns_503_without_broadcaster() {
        let state = mock_state(); // no broadcaster
        let app = router().with_state(state);

        let tx = sample_signed_tx();
        let body = serde_json::to_vec(&tx).unwrap();

        let req = Request::builder()
            .method("POST")
            .uri("/v2/tx/submit")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn submit_tx_forwards_bcs_payload_to_broadcaster() {
        let broadcaster = Arc::new(MockBroadcaster::new());
        let broadcaster_ref = Arc::clone(&broadcaster);

        let state = Arc::new(AppState {
            query: Arc::new(MockQueryBackend::new()),
            intent: None,
            consensus: None,
            network: None,
            broadcaster: Some(broadcaster as Arc<dyn crate::rest::TransactionBroadcaster>),
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

        let tx = sample_signed_tx();
        let body = serde_json::to_vec(&tx).unwrap();

        // The handler auto-derives target_shard when None; mirror that for
        // computing the expected BCS payload.
        let mut expected_tx = tx.clone();
        expected_tx.body.target_shard =
            Some(nexus_intent::resolver::shard_lookup::jump_consistent_hash(
                &expected_tx.body.sender,
                1, // matches num_shards in the AppState above
            ));
        let expected_bcs = bcs::to_bytes(&expected_tx).unwrap();

        let req = Request::builder()
            .method("POST")
            .uri("/v2/tx/submit")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let payloads = broadcaster_ref.payloads.lock().unwrap();
        assert_eq!(payloads.len(), 1);
        assert_eq!(payloads[0], expected_bcs);
    }

    #[tokio::test]
    async fn submit_tx_returns_500_on_broadcast_failure() {
        let broadcaster = MockBroadcaster::failing(RpcError::Internal("network down".into()));
        let state = mock_state_with_broadcaster(broadcaster);
        let app = router().with_state(state);

        let tx = sample_signed_tx();
        let body = serde_json::to_vec(&tx).unwrap();

        let req = Request::builder()
            .method("POST")
            .uri("/v2/tx/submit")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
