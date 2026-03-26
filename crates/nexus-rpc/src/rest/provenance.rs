// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Provenance query REST endpoints.
//!
//! | Method | Path | Description |
//! |--------|------|-------------|
//! | GET | `/v2/provenance/:digest` | Retrieve provenance record by ID |
//! | GET | `/v2/provenance` | Query provenance feed (agent, session, or all) |
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
use nexus_intent::agent_core::provenance::ProvenanceQueryParams;
use nexus_primitives::{AccountAddress, Blake3Digest, TimestampMs};

/// Query parameters for `GET /v2/provenance`.
#[derive(Debug, Deserialize)]
pub struct ProvenanceFeedParams {
    /// Filter by agent address (hex).
    pub agent: Option<String>,
    /// Filter by session ID (hex).
    pub session: Option<String>,
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
        .route("/v2/provenance/anchor/{digest}", get(get_anchor_receipt))
        .route("/v2/provenance/{digest}", get(get_provenance))
}
