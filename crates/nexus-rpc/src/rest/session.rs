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
        .route("/v2/sessions/:id", get(get_session))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rest::test_helpers::{
        mock_state, mock_state_with_session_provenance, MockSessionProvenanceBackend,
    };
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use nexus_intent::agent_core::session::SessionState;
    use nexus_intent::AgentSession;
    use nexus_primitives::{Blake3Digest, TimestampMs};
    use tower::ServiceExt;

    fn sample_session(id_byte: u8, state: SessionState) -> AgentSession {
        AgentSession {
            session_id: Blake3Digest([id_byte; 32]),
            created_at_ms: TimestampMs(1_700_000_000_000),
            replay_window_ms: 60_000,
            current_state: state,
            plan_hash: None,
            confirmation_ref: None,
        }
    }

    #[tokio::test]
    async fn session_returns_503_when_backend_absent() {
        let app = router().with_state(mock_state());
        let id_hex = "aa".repeat(32);
        let req = Request::builder()
            .uri(format!("/v2/sessions/{id_hex}"))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn get_session_returns_200() {
        let session = sample_session(0xaa, SessionState::Received);
        let backend = MockSessionProvenanceBackend::new().with_session(session);
        let app = router().with_state(mock_state_with_session_provenance(backend));
        let id_hex = "aa".repeat(32);
        let req = Request::builder()
            .uri(format!("/v2/sessions/{id_hex}"))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let dto: SessionDto = serde_json::from_slice(&body).unwrap();
        assert_eq!(dto.session_id, id_hex);
        assert_eq!(dto.state, "Received");
    }

    #[tokio::test]
    async fn session_not_found_returns_404() {
        let backend = MockSessionProvenanceBackend::new();
        let app = router().with_state(mock_state_with_session_provenance(backend));
        let id_hex = "ff".repeat(32);
        let req = Request::builder()
            .uri(format!("/v2/sessions/{id_hex}"))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn session_invalid_hex_returns_400() {
        let backend = MockSessionProvenanceBackend::new();
        let app = router().with_state(mock_state_with_session_provenance(backend));
        let req = Request::builder()
            .uri("/v2/sessions/not_valid_hex")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn list_sessions_returns_all() {
        let backend = MockSessionProvenanceBackend::new()
            .with_session(sample_session(0xaa, SessionState::Received))
            .with_session(sample_session(0xbb, SessionState::Finalized));
        let app = router().with_state(mock_state_with_session_provenance(backend));
        let req = Request::builder()
            .uri("/v2/sessions")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let list: SessionListResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(list.total, 2);
        assert_eq!(list.sessions.len(), 2);
    }

    #[tokio::test]
    async fn list_active_sessions_filters_terminal() {
        let backend = MockSessionProvenanceBackend::new()
            .with_session(sample_session(0xaa, SessionState::Received))
            .with_session(sample_session(0xbb, SessionState::Finalized))
            .with_session(sample_session(0xcc, SessionState::Aborted));
        let app = router().with_state(mock_state_with_session_provenance(backend));
        let req = Request::builder()
            .uri("/v2/sessions?active=true")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let list: SessionListResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(list.total, 1);
        assert_eq!(list.sessions[0].state, "Received");
    }
}
