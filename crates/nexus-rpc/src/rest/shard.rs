// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

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
/// validator allocations.  When the consensus backend provides full
/// topology the response includes per-shard validator assignments;
/// otherwise a minimal topology derived from `AppState::num_shards` is
/// returned so that clients can still resolve `target_shard` during early
/// devnet stages.
async fn shard_topology(State(state): State<Arc<AppState>>) -> RpcResult<Json<ShardTopologyDto>> {
    // Try the consensus backend first; fall back to the static shard count
    // when it is absent or hasn't implemented `shard_topology` yet.
    if let Some(consensus) = state.consensus.as_ref() {
        if let Ok(dto) = consensus.shard_topology() {
            return Ok(Json(dto));
        }
    }

    // Fallback: return a minimal topology using the statically-configured
    // shard count.
    let n = state.num_shards;
    let shards = (0..n)
        .map(|id| crate::dto::ShardInfoDto {
            shard_id: id,
            validators: Vec::new(),
        })
        .collect();
    Ok(Json(ShardTopologyDto {
        num_shards: n,
        shards,
    }))
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
    use crate::rest::test_helpers::{mock_state, mock_state_with_consensus, MockConsensusBackend};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[tokio::test]
    async fn shard_topology_returns_minimal_when_no_backend() {
        let app = router().with_state(mock_state());
        let req = Request::builder()
            .uri("/v2/shards")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let dto: ShardTopologyDto = serde_json::from_slice(&body).unwrap();
        // mock_state() sets num_shards = 1
        assert_eq!(dto.num_shards, 1);
        assert_eq!(dto.shards.len(), 1);
        assert!(dto.shards[0].validators.is_empty());
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

    #[tokio::test]
    async fn shard_chain_head_returns_404_for_invalid_shard_id() {
        // num_shards = 1, so shard_id=5 is out of range.
        let backend = MockConsensusBackend::new();
        let app = router().with_state(mock_state_with_consensus(backend));
        let req = Request::builder()
            .uri("/v2/shards/5/head")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn shard_topology_returns_200_with_consensus_backend() {
        // ConsensusBackend::shard_topology has default impl that returns
        // Unavailable, so the handler falls through to the static fallback.
        let backend = MockConsensusBackend::new();
        let app = router().with_state(mock_state_with_consensus(backend));
        let req = Request::builder()
            .uri("/v2/shards")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let dto: ShardTopologyDto = serde_json::from_slice(&body).unwrap();
        assert_eq!(dto.num_shards, 1);
    }
}
