// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

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
    use crate::rest::test_helpers::{mock_state, mock_state_with_chain_head};
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

    #[tokio::test]
    async fn chain_head_returns_200_with_data() {
        let head = ChainHeadDto {
            sequence: 42,
            anchor_digest: "ab".repeat(32),
            state_root: "cd".repeat(32),
            epoch: 3,
            round: 100,
            cert_count: 4,
            tx_count: 10,
            gas_total: 50_000,
            committed_at_ms: 1_700_000_000_000,
        };
        let app = router().with_state(mock_state_with_chain_head(head));
        let req = Request::builder()
            .uri("/v2/chain/head")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let dto: ChainHeadDto = serde_json::from_slice(&body).unwrap();
        assert_eq!(dto.sequence, 42);
        assert_eq!(dto.epoch, 3);
        assert_eq!(dto.tx_count, 10);
    }
}
