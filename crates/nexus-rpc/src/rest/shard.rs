//! Shard topology REST endpoints (W-5).
//!
//! | Method | Path | Description |
//! |--------|------|-------------|
//! | GET | `/v2/shards` | Current shard topology |
//! | GET | `/v2/shards/:shard_id/head` | Per-shard chain head |

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};

use super::AppState;
use crate::dto::{ShardChainHeadDto, ShardTopologyDto};
use crate::error::{RpcError, RpcResult};

/// Build the shard router fragment.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v2/shards", get(shard_topology))
        .route("/v2/shards/:shard_id/head", get(shard_chain_head))
}

/// `GET /v2/shards`
///
/// Returns the current shard topology: total shard count and per-shard
/// validator allocations.
async fn shard_topology(State(state): State<Arc<AppState>>) -> RpcResult<Json<ShardTopologyDto>> {
    let consensus = state
        .consensus
        .as_ref()
        .ok_or_else(|| RpcError::Unavailable("consensus backend not available".into()))?;
    let dto = consensus.shard_topology()?;
    Ok(Json(dto))
}

/// `GET /v2/shards/:shard_id/head`
///
/// Returns the chain head for a specific shard.
async fn shard_chain_head(
    State(state): State<Arc<AppState>>,
    Path(shard_id): Path<u16>,
) -> RpcResult<Json<ShardChainHeadDto>> {
    if shard_id >= state.num_shards && state.num_shards > 0 {
        return Err(RpcError::NotFound(format!(
            "shard {shard_id} does not exist (num_shards={})",
            state.num_shards
        )));
    }

    let consensus = state
        .consensus
        .as_ref()
        .ok_or_else(|| RpcError::Unavailable("consensus backend not available".into()))?;
    let dto = consensus.shard_chain_head(shard_id)?;
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
    async fn shard_topology_returns_503_when_no_backend() {
        let app = router().with_state(mock_state());
        let req = Request::builder()
            .uri("/v2/shards")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn shard_chain_head_returns_503_when_no_backend() {
        let app = router().with_state(mock_state());
        let req = Request::builder()
            .uri("/v2/shards/0/head")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
