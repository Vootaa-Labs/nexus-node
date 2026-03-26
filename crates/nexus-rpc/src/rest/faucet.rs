// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Faucet endpoint — dispense test tokens on devnet.
//!
//! `POST /v2/faucet/mint` — mints a fixed amount of native tokens to the
//! given recipient address. Only available when `faucet_enabled` is true
//! in the node configuration.

use std::sync::Arc;

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::AppState;
use crate::error::{RpcError, RpcResult};

/// Build the faucet router fragment.
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/v2/faucet/mint", post(faucet_mint))
}

/// Body for `POST /v2/faucet/mint`.
#[derive(Debug, Deserialize)]
pub struct FaucetRequest {
    /// Hex-encoded 32-byte recipient address.
    pub recipient: String,
}

/// Response from `POST /v2/faucet/mint`.
#[derive(Debug, Serialize, Deserialize)]
pub struct FaucetResponse {
    /// Hex-encoded transaction digest (synthetic for faucet).
    pub tx_digest: String,
    /// Amount minted (in voo, the smallest unit; 1 NXS = 10^9 voo).
    pub amount: u64,
}

/// `POST /v2/faucet/mint`
///
/// Mints test tokens to the given address. Only available on devnet.
async fn faucet_mint(
    State(state): State<Arc<AppState>>,
    Json(req): Json<FaucetRequest>,
) -> RpcResult<Json<FaucetResponse>> {
    // Guard: faucet must be enabled.
    if !state.faucet_enabled {
        return Err(RpcError::BadRequest(
            "faucet is disabled on this node".to_string(),
        ));
    }

    let address = super::parse_address(&req.recipient)?;

    // Per-address rate limit check.
    if let Some(ref limiter) = state.faucet_addr_limiter {
        if limiter.check(&address.0).is_err() {
            return Err(RpcError::BadRequest(
                "faucet rate limit exceeded for this address".to_string(),
            ));
        }
    }

    // Delegate to the query backend's faucet_mint method.
    let amount = state.faucet_amount;
    state.query.faucet_mint(&address, amount)?;

    // Compute a synthetic digest so the response schema matches normal tx flow.
    // Domain-separated to avoid collision with real transaction hashes.
    // Include wall-clock time for per-request uniqueness (L-002).
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"nexus::faucet::synthetic_digest::v2");
    hasher.update(&address.0);
    hasher.update(&amount.to_le_bytes());
    hasher.update(
        &std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .to_le_bytes(),
    );
    let digest = hasher.finalize();

    tracing::info!(
        recipient = %address.to_hex(),
        amount = amount,
        tx_digest = %hex::encode(digest.as_bytes()),
        "faucet mint completed"
    );

    Ok(Json(FaucetResponse {
        tx_digest: hex::encode(digest.as_bytes()),
        amount,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rest::test_helpers::mock_state;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn faucet_state(enabled: bool) -> Arc<AppState> {
        let mut state = mock_state();
        let inner = Arc::get_mut(&mut state).unwrap();
        inner.faucet_enabled = enabled;
        inner.faucet_amount = 1_000_000;
        state
    }

    #[tokio::test]
    async fn faucet_returns_200_when_enabled() {
        let state = faucet_state(true);
        let app = router().with_state(state);

        let body = serde_json::json!({ "recipient": hex::encode([0xAA; 32]) });
        let req = Request::builder()
            .method("POST")
            .uri("/v2/faucet/mint")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let resp: FaucetResponse = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(resp.amount, 1_000_000);
        assert!(!resp.tx_digest.is_empty());
    }

    #[tokio::test]
    async fn faucet_returns_400_when_disabled() {
        let state = faucet_state(false);
        let app = router().with_state(state);

        let body = serde_json::json!({ "recipient": hex::encode([0xBB; 32]) });
        let req = Request::builder()
            .method("POST")
            .uri("/v2/faucet/mint")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
