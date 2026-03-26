//! Chain head REST endpoint.
//!
//! `GET /v2/chain/head` — returns the latest committed block summary.

use std::sync::Arc;

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};

use super::AppState;
use crate::dto::ChainHeadDto;
use crate::error::{RpcError, RpcResult};

/// Build the chain head router fragment.
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/v2/chain/head", get(chain_head))
}

/// `GET /v2/chain/head`
///
/// Returns the latest committed block summary (sequence, state root,
/// anchor digest, tx/gas counts).
async fn chain_head(State(state): State<Arc<AppState>>) -> RpcResult<Json<ChainHeadDto>> {
    let head = state
        .query
        .chain_head()
        .map_err(|e| RpcError::Internal(e.to_string()))?;

    match head {
        Some(dto) => Ok(Json(dto)),
        None => Err(RpcError::NotFound("no committed block yet".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rest::test_helpers::mock_state;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[tokio::test]
    async fn chain_head_returns_404_when_no_commits() {
        let app = router().with_state(mock_state());
        let req = Request::builder()
            .uri("/v2/chain/head")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
