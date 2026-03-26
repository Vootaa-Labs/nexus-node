// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Session query REST endpoints.
//!
//! | Method | Path | Description |
//! |--------|------|-------------|
//! | GET | `/v2/sessions/:id` | Retrieve session by ID |
//! | GET | `/v2/sessions` | List sessions (optional `?active=true` filter) |

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;

use crate::dto::{SessionDto, SessionListResponse};
use crate::error::RpcError;
use crate::rest::AppState;
use nexus_primitives::Blake3Digest;

/// Query parameters for session listing.
#[derive(Debug, Deserialize)]
pub struct SessionListParams {
    /// If `true`, only return non-terminal (active) sessions.
    #[serde(default)]
    pub active: bool,
}

/// `GET /v2/sessions/:id`
async fn get_session(
    State(state): State<Arc<AppState>>,
    Path(id_hex): Path<String>,
) -> Result<Json<SessionDto>, RpcError> {
    let backend = state
        .session_provenance
        .as_ref()
        .ok_or_else(|| RpcError::Unavailable("session store not available".into()))?;

    let id_bytes =
        hex::decode(&id_hex).map_err(|e| RpcError::BadRequest(format!("invalid hex: {e}")))?;
    if id_bytes.len() != 32 {
        return Err(RpcError::BadRequest("session id must be 32 bytes".into()));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&id_bytes);
    let session_id = Blake3Digest(arr);

    let session = backend
        .get_session(&session_id)
        .ok_or_else(|| RpcError::NotFound(format!("session {id_hex} not found")))?;

    Ok(Json(session_to_dto(&session)))
}

/// `GET /v2/sessions`
async fn list_sessions(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SessionListParams>,
) -> Result<Json<SessionListResponse>, RpcError> {
    let backend = state
        .session_provenance
        .as_ref()
        .ok_or_else(|| RpcError::Unavailable("session store not available".into()))?;

    let sessions: Vec<SessionDto> = if params.active {
        backend
            .active_sessions()
            .iter()
            .map(session_to_dto)
            .collect()
    } else {
        backend.all_sessions().iter().map(session_to_dto).collect()
    };

    let total = sessions.len();
    Ok(Json(SessionListResponse { sessions, total }))
}

fn session_to_dto(s: &nexus_intent::AgentSession) -> SessionDto {
    SessionDto {
        session_id: hex::encode(s.session_id.0),
        state: format!("{:?}", s.current_state),
        created_at_ms: s.created_at_ms.0,
        plan_hash: s.plan_hash.map(|h| hex::encode(h.0)),
        confirmation_ref: s.confirmation_ref.map(|h| hex::encode(h.0)),
    }
}

/// Build the session router.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v2/sessions", get(list_sessions))
        .route("/v2/sessions/{id}", get(get_session))
}
