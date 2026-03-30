// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Provenance query REST endpoints.
//!
//! | Method | Path | Description |
//! |--------|------|-------------|
//! | GET | `/v2/provenance/:digest` | Retrieve provenance record by ID |
//! | GET | `/v2/provenance` | Query provenance feed (agent, session, tx_hash, status, or all) |
//! | GET | `/v2/provenance/anchor/:digest` | Retrieve anchor receipt by digest |
//! | GET | `/v2/provenance/anchors` | List anchor receipts |

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;

use crate::dto::{
    AnchorReceiptDto, AnchorReceiptListResponse, ProvenanceQueryResponse, ProvenanceRecordDto,
};
use crate::error::RpcError;
use crate::rest::AppState;
use nexus_intent::agent_core::provenance::{ProvenanceQueryParams, ProvenanceStatus};
use nexus_primitives::{AccountAddress, Blake3Digest, TimestampMs};

/// Query parameters for `GET /v2/provenance`.
#[derive(Debug, Deserialize)]
pub struct ProvenanceFeedParams {
    /// Filter by agent address (hex).
    pub agent: Option<String>,
    /// Filter by session ID (hex).
    pub session: Option<String>,
    /// Filter by transaction hash (hex).
    pub tx_hash: Option<String>,
    /// Filter by provenance status (`Pending`, `Committed`, `Failed`, `Aborted`, `Expired`).
    pub status: Option<String>,
    /// Max records to return (default 50).
    pub limit: Option<u32>,
    /// Pagination cursor (hex, from previous response).
    pub cursor: Option<String>,
    /// Only include records after this timestamp (ms).
    pub after_ms: Option<u64>,
    /// Only include records before this timestamp (ms).
    pub before_ms: Option<u64>,
}

/// `GET /v2/provenance/:digest`
async fn get_provenance(
    State(state): State<Arc<AppState>>,
    Path(id_hex): Path<String>,
) -> Result<Json<ProvenanceRecordDto>, RpcError> {
    let backend = state
        .session_provenance
        .as_ref()
        .ok_or_else(|| RpcError::Unavailable("provenance store not available".into()))?;

    let digest = parse_digest(&id_hex)?;

    let record = backend
        .get_provenance(&digest)
        .ok_or_else(|| RpcError::NotFound(format!("provenance {id_hex} not found")))?;

    Ok(Json(record_to_dto(&record)))
}

/// `GET /v2/provenance`
async fn query_provenance(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ProvenanceFeedParams>,
) -> Result<Json<ProvenanceQueryResponse>, RpcError> {
    let backend = state
        .session_provenance
        .as_ref()
        .ok_or_else(|| RpcError::Unavailable("provenance store not available".into()))?;

    let query_params = ProvenanceQueryParams {
        limit: params.limit.unwrap_or(50),
        cursor: params.cursor.as_deref().map(parse_digest).transpose()?,
        after_ms: params.after_ms.map(TimestampMs),
        before_ms: params.before_ms.map(TimestampMs),
    };

    let result = if let Some(ref agent_hex) = params.agent {
        let agent = parse_address(agent_hex)?;
        backend.query_provenance_by_agent(&agent, &query_params)
    } else if let Some(ref session_hex) = params.session {
        let digest = parse_digest(session_hex)?;
        backend.query_provenance_by_session(&digest, &query_params)
    } else if let Some(ref tx_hex) = params.tx_hash {
        let digest = parse_digest(tx_hex)?;
        backend.query_provenance_by_tx_hash(&digest, &query_params)
    } else if let Some(ref status_str) = params.status {
        let status = parse_provenance_status(status_str)?;
        backend.query_provenance_by_status(status, &query_params)
    } else {
        backend.provenance_activity_feed(&query_params)
    };

    Ok(Json(ProvenanceQueryResponse {
        records: result.records.iter().map(record_to_dto).collect(),
        total_count: result.total_count,
        cursor: result.cursor.map(|c| hex::encode(c.0)),
    }))
}

// ── Helpers ─────────────────────────────────────────────────────────────

fn parse_digest(hex_str: &str) -> Result<Blake3Digest, RpcError> {
    let bytes =
        hex::decode(hex_str).map_err(|e| RpcError::BadRequest(format!("invalid hex: {e}")))?;
    if bytes.len() != 32 {
        return Err(RpcError::BadRequest("digest must be 32 bytes".into()));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(Blake3Digest(arr))
}

fn parse_address(hex_str: &str) -> Result<AccountAddress, RpcError> {
    let bytes =
        hex::decode(hex_str).map_err(|e| RpcError::BadRequest(format!("invalid hex: {e}")))?;
    if bytes.len() != 32 {
        return Err(RpcError::BadRequest("address must be 32 bytes".into()));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(AccountAddress(arr))
}

fn parse_provenance_status(s: &str) -> Result<ProvenanceStatus, RpcError> {
    match s {
        "Pending" => Ok(ProvenanceStatus::Pending),
        "Committed" => Ok(ProvenanceStatus::Committed),
        "Failed" => Ok(ProvenanceStatus::Failed),
        "Aborted" => Ok(ProvenanceStatus::Aborted),
        "Expired" => Ok(ProvenanceStatus::Expired),
        _ => Err(RpcError::BadRequest(format!(
            "unknown provenance status: {s}"
        ))),
    }
}

fn record_to_dto(r: &nexus_intent::ProvenanceRecord) -> ProvenanceRecordDto {
    ProvenanceRecordDto {
        provenance_id: hex::encode(r.provenance_id.0),
        session_id: hex::encode(r.session_id.0),
        agent_id: hex::encode(r.agent_id.0),
        parent_agent_id: r.parent_agent_id.map(|a| hex::encode(a.0)),
        intent_hash: hex::encode(r.intent_hash.0),
        plan_hash: hex::encode(r.plan_hash.0),
        tx_hash: r.tx_hash.map(|h| hex::encode(h.0)),
        status: format!("{:?}", r.status),
        created_at_ms: r.created_at_ms.0,
    }
}

fn receipt_to_dto(r: &nexus_intent::AnchorReceipt) -> AnchorReceiptDto {
    AnchorReceiptDto {
        batch_seq: r.batch_seq,
        anchor_digest: hex::encode(r.anchor_digest.0),
        tx_hash: hex::encode(r.tx_hash.0),
        block_height: r.block_height,
        anchored_at_ms: r.anchored_at_ms.0,
    }
}

// ── Anchor receipt endpoints ────────────────────────────────────────────

/// Query parameters for `GET /v2/provenance/anchors`.
#[derive(Debug, Deserialize)]
pub struct AnchorListParams {
    /// Max receipts to return (default 50).
    pub limit: Option<u32>,
}

/// `GET /v2/provenance/anchor/:digest`
async fn get_anchor_receipt(
    State(state): State<Arc<AppState>>,
    Path(digest_hex): Path<String>,
) -> Result<Json<AnchorReceiptDto>, RpcError> {
    let backend = state
        .session_provenance
        .as_ref()
        .ok_or_else(|| RpcError::Unavailable("provenance store not available".into()))?;

    let digest = parse_digest(&digest_hex)?;

    let receipt = backend
        .get_anchor_receipt(&digest)
        .ok_or_else(|| RpcError::NotFound(format!("anchor receipt {digest_hex} not found")))?;

    Ok(Json(receipt_to_dto(&receipt)))
}

/// `GET /v2/provenance/anchors`
async fn list_anchor_receipts(
    State(state): State<Arc<AppState>>,
    Query(params): Query<AnchorListParams>,
) -> Result<Json<AnchorReceiptListResponse>, RpcError> {
    let backend = state
        .session_provenance
        .as_ref()
        .ok_or_else(|| RpcError::Unavailable("provenance store not available".into()))?;

    let limit = params.limit.unwrap_or(50);
    let receipts = backend
        .list_anchor_receipts(limit)
        .iter()
        .map(receipt_to_dto)
        .collect();

    Ok(Json(AnchorReceiptListResponse { receipts }))
}

/// Build the provenance router.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v2/provenance", get(query_provenance))
        .route("/v2/provenance/anchors", get(list_anchor_receipts))
        .route("/v2/provenance/anchor/:digest", get(get_anchor_receipt))
        .route("/v2/provenance/:digest", get(get_provenance))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rest::test_helpers::{
        mock_state, mock_state_with_session_provenance, MockSessionProvenanceBackend,
    };
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use nexus_intent::agent_core::provenance::ProvenanceStatus;
    use nexus_intent::ProvenanceRecord;
    use nexus_primitives::{AccountAddress, Blake3Digest, TimestampMs};
    use tower::ServiceExt;

    fn sample_record(id_byte: u8, status: ProvenanceStatus) -> ProvenanceRecord {
        ProvenanceRecord {
            provenance_id: Blake3Digest([id_byte; 32]),
            session_id: Blake3Digest([0x01; 32]),
            request_id: Blake3Digest([0x02; 32]),
            agent_id: AccountAddress([0x10; 32]),
            parent_agent_id: None,
            capability_token_id: None,
            intent_hash: Blake3Digest([0x03; 32]),
            plan_hash: Blake3Digest([0x04; 32]),
            confirmation_ref: None,
            tx_hash: Some(Blake3Digest([0x05; 32])),
            status,
            created_at_ms: TimestampMs(1_700_000_000_000),
        }
    }

    #[tokio::test]
    async fn provenance_returns_503_when_backend_absent() {
        let app = router().with_state(mock_state());
        let id_hex = "aa".repeat(32);
        let req = Request::builder()
            .uri(format!("/v2/provenance/{id_hex}"))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn get_provenance_returns_200() {
        let record = sample_record(0xaa, ProvenanceStatus::Committed);
        let backend = MockSessionProvenanceBackend::new().with_provenance(record);
        let app = router().with_state(mock_state_with_session_provenance(backend));
        let id_hex = "aa".repeat(32);
        let req = Request::builder()
            .uri(format!("/v2/provenance/{id_hex}"))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let dto: ProvenanceRecordDto = serde_json::from_slice(&body).unwrap();
        assert_eq!(dto.provenance_id, id_hex);
        assert_eq!(dto.status, "Committed");
    }

    #[tokio::test]
    async fn provenance_not_found_returns_404() {
        let backend = MockSessionProvenanceBackend::new();
        let app = router().with_state(mock_state_with_session_provenance(backend));
        let id_hex = "ff".repeat(32);
        let req = Request::builder()
            .uri(format!("/v2/provenance/{id_hex}"))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn provenance_invalid_hex_returns_400() {
        let backend = MockSessionProvenanceBackend::new();
        let app = router().with_state(mock_state_with_session_provenance(backend));
        let req = Request::builder()
            .uri("/v2/provenance/not_hex")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn query_by_agent_returns_results() {
        let record = sample_record(0xaa, ProvenanceStatus::Committed);
        let backend = MockSessionProvenanceBackend::new().with_provenance(record);
        let app = router().with_state(mock_state_with_session_provenance(backend));
        let agent_hex = "10".repeat(32);
        let req = Request::builder()
            .uri(format!("/v2/provenance?agent={agent_hex}"))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let result: ProvenanceQueryResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(result.records.len(), 1);
    }

    #[tokio::test]
    async fn query_by_status_filters() {
        let committed = sample_record(0xaa, ProvenanceStatus::Committed);
        let pending = sample_record(0xbb, ProvenanceStatus::Pending);
        let backend = MockSessionProvenanceBackend::new()
            .with_provenance(committed)
            .with_provenance(pending);
        let app = router().with_state(mock_state_with_session_provenance(backend));
        let req = Request::builder()
            .uri("/v2/provenance?status=Committed")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let result: ProvenanceQueryResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(result.records.len(), 1);
        assert_eq!(result.records[0].status, "Committed");
    }

    #[tokio::test]
    async fn query_activity_feed_returns_all() {
        let r1 = sample_record(0xaa, ProvenanceStatus::Committed);
        let r2 = sample_record(0xbb, ProvenanceStatus::Pending);
        let backend = MockSessionProvenanceBackend::new()
            .with_provenance(r1)
            .with_provenance(r2);
        let app = router().with_state(mock_state_with_session_provenance(backend));
        let req = Request::builder()
            .uri("/v2/provenance")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let result: ProvenanceQueryResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(result.records.len(), 2);
    }

    #[tokio::test]
    async fn query_invalid_status_returns_400() {
        let backend = MockSessionProvenanceBackend::new();
        let app = router().with_state(mock_state_with_session_provenance(backend));
        let req = Request::builder()
            .uri("/v2/provenance?status=InvalidStatus")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
