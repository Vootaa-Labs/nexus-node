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
    use crate::rest::test_helpers::mock_state;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

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
}
