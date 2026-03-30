// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Block query REST endpoints.
//!
//! | Method | Path | Description |
//! |--------|------|-------------|
//! | GET | `/v2/block/:seq/header`   | Block header summary |
//! | GET | `/v2/block/:seq`          | Full block content |
//! | GET | `/v2/block/:seq/receipts` | Batch receipts |
//! | GET | `/v2/block/:seq/zk-proof` | ZK proof (stub) |

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};

use super::AppState;
use crate::dto::{BlockDto, BlockHeaderDto, BlockReceiptsDto, ZkProofDto};
use crate::error::{RpcError, RpcResult};

/// Build the block query router fragment.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v2/block/:seq/header", get(block_header))
        .route("/v2/block/:seq", get(block_full))
        .route("/v2/block/:seq/receipts", get(block_receipts))
        .route("/v2/block/:seq/zk-proof", get(block_zk_proof))
}

/// `GET /v2/block/:seq/header`
///
/// Returns block header metadata for the given commit sequence.
async fn block_header(
    State(state): State<Arc<AppState>>,
    Path(seq): Path<u64>,
) -> RpcResult<Json<BlockHeaderDto>> {
    let backend = state
        .block
        .as_ref()
        .ok_or_else(|| RpcError::Unavailable("block queries not available".into()))?;
    let header = backend.block_header(seq)?;
    Ok(Json(header))
}

/// `GET /v2/block/:seq`
///
/// Returns the full block content (header + transactions) for the given
/// commit sequence.
async fn block_full(
    State(state): State<Arc<AppState>>,
    Path(seq): Path<u64>,
) -> RpcResult<Json<BlockDto>> {
    let backend = state
        .block
        .as_ref()
        .ok_or_else(|| RpcError::Unavailable("block queries not available".into()))?;
    let block = backend.block_full(seq)?;
    Ok(Json(block))
}

/// `GET /v2/block/:seq/receipts`
///
/// Returns batch receipts for all transactions in the given block.
async fn block_receipts(
    State(state): State<Arc<AppState>>,
    Path(seq): Path<u64>,
) -> RpcResult<Json<BlockReceiptsDto>> {
    let backend = state
        .block
        .as_ref()
        .ok_or_else(|| RpcError::Unavailable("block queries not available".into()))?;
    let receipts = backend.block_receipts(seq)?;
    Ok(Json(receipts))
}

/// `GET /v2/block/:seq/zk-proof`
///
/// Returns the ZK proof for the given block (currently returns 503).
async fn block_zk_proof(
    State(state): State<Arc<AppState>>,
    Path(seq): Path<u64>,
) -> RpcResult<Json<ZkProofDto>> {
    let backend = state
        .block
        .as_ref()
        .ok_or_else(|| RpcError::Unavailable("block queries not available".into()))?;
    let proof = backend.block_zk_proof(seq)?;
    Ok(Json(proof))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::{ExecutionStatusDto, TxSummaryDto};
    use crate::rest::test_helpers::{mock_state, mock_state_with_block, MockBlockBackend};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn sample_header(seq: u64) -> BlockHeaderDto {
        BlockHeaderDto {
            sequence: seq,
            anchor_digest: "aa".repeat(32),
            state_root: "bb".repeat(32),
            epoch: 1,
            cert_count: 3,
            tx_count: 2,
            gas_total: 5000,
            committed_at_ms: 1_700_000_000_000,
        }
    }

    fn sample_block(seq: u64) -> BlockDto {
        BlockDto {
            header: sample_header(seq),
            transactions: vec![TxSummaryDto {
                tx_digest: "cc".repeat(32),
                gas_used: 2500,
                status: ExecutionStatusDto::Success,
            }],
        }
    }

    fn sample_receipts(seq: u64) -> BlockReceiptsDto {
        BlockReceiptsDto {
            block_seq: seq,
            receipts: Vec::new(),
            total_gas: 5000,
        }
    }

    #[tokio::test]
    async fn block_header_returns_503_when_backend_absent() {
        let app = router().with_state(mock_state());
        let req = Request::builder()
            .uri("/v2/block/1/header")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn block_full_returns_503_when_backend_absent() {
        let app = router().with_state(mock_state());
        let req = Request::builder()
            .uri("/v2/block/1")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn block_header_returns_200_with_mock() {
        let backend = MockBlockBackend::new().with_header(1, sample_header(1));
        let app = router().with_state(mock_state_with_block(backend));
        let req = Request::builder()
            .uri("/v2/block/1/header")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let header: BlockHeaderDto = serde_json::from_slice(&body).unwrap();
        assert_eq!(header.sequence, 1);
        assert_eq!(header.epoch, 1);
        assert_eq!(header.tx_count, 2);
    }

    #[tokio::test]
    async fn block_full_returns_200_with_mock() {
        let backend = MockBlockBackend::new().with_block(1, sample_block(1));
        let app = router().with_state(mock_state_with_block(backend));
        let req = Request::builder()
            .uri("/v2/block/1")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let block: BlockDto = serde_json::from_slice(&body).unwrap();
        assert_eq!(block.header.sequence, 1);
        assert_eq!(block.transactions.len(), 1);
    }

    #[tokio::test]
    async fn block_receipts_returns_200_with_mock() {
        let backend = MockBlockBackend::new().with_receipts(1, sample_receipts(1));
        let app = router().with_state(mock_state_with_block(backend));
        let req = Request::builder()
            .uri("/v2/block/1/receipts")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let receipts: BlockReceiptsDto = serde_json::from_slice(&body).unwrap();
        assert_eq!(receipts.block_seq, 1);
        assert_eq!(receipts.total_gas, 5000);
    }

    #[tokio::test]
    async fn block_not_found_returns_404() {
        let backend = MockBlockBackend::new();
        let app = router().with_state(mock_state_with_block(backend));
        let req = Request::builder()
            .uri("/v2/block/999/header")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn block_zk_proof_returns_503_stub() {
        let backend = MockBlockBackend::new();
        let app = router().with_state(mock_state_with_block(backend));
        let req = Request::builder()
            .uri("/v2/block/1/zk-proof")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
