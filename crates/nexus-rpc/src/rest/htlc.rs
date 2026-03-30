// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! HTLC lock state REST endpoints (W-5).
//!
//! | Method | Path | Description |
//! |--------|------|-------------|
//! | GET | `/v2/htlc/:lock_digest` | Query HTLC lock by digest |
//! | GET | `/v2/htlc/pending` | List pending HTLC locks |

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};

use super::AppState;
use crate::dto::{HtlcLockDto, HtlcPendingListDto};
use crate::error::{RpcError, RpcResult};

/// Default limit for pending HTLC list queries.
const DEFAULT_PENDING_LIMIT: u32 = 100;

/// Build the HTLC router fragment.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        // NOTE: `/v2/htlc/pending` must be registered BEFORE the wildcard
        // `/v2/htlc/:lock_digest` to avoid the static segment being captured.
        .route("/v2/htlc/pending", get(htlc_pending))
        .route("/v2/htlc/:lock_digest", get(htlc_by_digest))
}

/// `GET /v2/htlc/:lock_digest`
///
/// Returns the state of a single HTLC lock identified by its hex-encoded digest.
async fn htlc_by_digest(
    State(state): State<Arc<AppState>>,
    Path(lock_digest_hex): Path<String>,
) -> RpcResult<Json<HtlcLockDto>> {
    let htlc_backend = state
        .htlc
        .as_ref()
        .ok_or_else(|| RpcError::Unavailable("HTLC backend not available".into()))?;

    let digest = nexus_primitives::Blake3Digest::from_hex(&lock_digest_hex)
        .map_err(|e| RpcError::BadRequest(format!("invalid lock digest: {e}")))?;

    match htlc_backend.get_htlc_lock(&digest)? {
        Some(dto) => Ok(Json(dto)),
        None => Err(RpcError::NotFound(format!(
            "HTLC lock not found: {lock_digest_hex}"
        ))),
    }
}

/// `GET /v2/htlc/pending`
///
/// Returns a list of pending (unclaimed, unrefunded) HTLC locks.
async fn htlc_pending(State(state): State<Arc<AppState>>) -> RpcResult<Json<HtlcPendingListDto>> {
    let htlc_backend = state
        .htlc
        .as_ref()
        .ok_or_else(|| RpcError::Unavailable("HTLC backend not available".into()))?;

    let dto = htlc_backend.list_pending_htlc_locks(DEFAULT_PENDING_LIMIT)?;
    Ok(Json(dto))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::{HtlcPendingListDto, HtlcStatusDto};
    use crate::rest::test_helpers::{mock_state, mock_state_with_htlc, MockHtlcBackend};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn sample_lock(digest_hex: &str) -> HtlcLockDto {
        HtlcLockDto {
            lock_digest: digest_hex.to_string(),
            sender: "aa".repeat(32),
            recipient: "bb".repeat(32),
            amount: 1_000_000,
            target_shard: 0,
            timeout_epoch: 10,
            status: HtlcStatusDto::Pending,
        }
    }

    #[tokio::test]
    async fn htlc_by_digest_returns_503_when_no_backend() {
        let app = router().with_state(mock_state());
        let digest_hex = "aa".repeat(32);
        let req = Request::builder()
            .uri(format!("/v2/htlc/{digest_hex}"))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn htlc_pending_returns_503_when_no_backend() {
        let app = router().with_state(mock_state());
        let req = Request::builder()
            .uri("/v2/htlc/pending")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn htlc_by_digest_returns_200_when_found() {
        let digest_hex = "cc".repeat(32);
        let digest = nexus_primitives::Blake3Digest::from_hex(&digest_hex).unwrap();
        let lock = sample_lock(&digest_hex);
        let backend = MockHtlcBackend::new().with_lock(digest, lock.clone());
        let app = router().with_state(mock_state_with_htlc(backend));

        let req = Request::builder()
            .uri(format!("/v2/htlc/{digest_hex}"))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let dto: HtlcLockDto = serde_json::from_slice(&body).unwrap();
        assert_eq!(dto.lock_digest, digest_hex);
        assert_eq!(dto.amount, 1_000_000);
    }

    #[tokio::test]
    async fn htlc_by_digest_returns_404_when_not_found() {
        let backend = MockHtlcBackend::new();
        let app = router().with_state(mock_state_with_htlc(backend));
        let digest_hex = "dd".repeat(32);
        let req = Request::builder()
            .uri(format!("/v2/htlc/{digest_hex}"))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn htlc_by_digest_returns_400_for_invalid_hex() {
        let backend = MockHtlcBackend::new();
        let app = router().with_state(mock_state_with_htlc(backend));
        let req = Request::builder()
            .uri("/v2/htlc/not-valid-hex")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn htlc_pending_returns_200_with_locks() {
        let lock = sample_lock(&"ee".repeat(32));
        let backend = MockHtlcBackend::new().with_pending(vec![lock.clone()]);
        let app = router().with_state(mock_state_with_htlc(backend));

        let req = Request::builder()
            .uri("/v2/htlc/pending")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let dto: HtlcPendingListDto = serde_json::from_slice(&body).unwrap();
        assert_eq!(dto.total, 1);
        assert_eq!(dto.locks.len(), 1);
        assert_eq!(dto.locks[0].status, HtlcStatusDto::Pending);
    }

    #[tokio::test]
    async fn htlc_pending_returns_empty_list() {
        let backend = MockHtlcBackend::new().with_pending(vec![]);
        let app = router().with_state(mock_state_with_htlc(backend));

        let req = Request::builder()
            .uri("/v2/htlc/pending")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let dto: HtlcPendingListDto = serde_json::from_slice(&body).unwrap();
        assert_eq!(dto.total, 0);
        assert!(dto.locks.is_empty());
    }
}
