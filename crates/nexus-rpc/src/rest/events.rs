// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Contract event query endpoints.
//!
//! | Method | Path           | Description                          |
//! |--------|----------------|--------------------------------------|
//! | GET    | `/v2/events`   | Query contract events with filters   |

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;

use super::AppState;
use crate::dto::EventQueryResponse;
use crate::error::{RpcError, RpcResult};
use nexus_primitives::AccountAddress;

/// Query parameters for `GET /v2/events`.
#[derive(Debug, Deserialize)]
pub struct EventQueryParams {
    /// Filter by emitting contract address (hex).
    pub contract: Option<String>,
    /// Filter by event type tag (e.g. `"counter::IncrementEvent"`).
    pub event_type: Option<String>,
    /// Only return events after this block sequence (exclusive).
    pub after_seq: Option<u64>,
    /// Only return events before this block sequence (exclusive).
    pub before_seq: Option<u64>,
    /// Maximum number of events to return (default: 50, max: 200).
    pub limit: Option<u32>,
    /// Opaque pagination cursor from a previous response.
    pub cursor: Option<String>,
}

/// Build the events router fragment.
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/v2/events", get(query_events))
}

/// `GET /v2/events`
///
/// Returns contract events matching the given filters with pagination.
async fn query_events(
    State(state): State<Arc<AppState>>,
    Query(params): Query<EventQueryParams>,
) -> RpcResult<Json<EventQueryResponse>> {
    let backend = state
        .event_backend
        .as_ref()
        .ok_or_else(|| RpcError::Unavailable("event queries not available".into()))?;

    let contract = params
        .contract
        .as_deref()
        .map(AccountAddress::from_hex)
        .transpose()
        .map_err(|e| RpcError::BadRequest(format!("invalid contract address: {e}")))?;

    let limit = params.limit.unwrap_or(50).min(200);

    let result = backend.query_events(
        contract.as_ref(),
        params.event_type.as_deref(),
        params.after_seq,
        params.before_seq,
        limit,
        params.cursor.as_deref(),
    )?;

    Ok(Json(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::{ContractEventDto, EventQueryResponse};
    use crate::rest::test_helpers::{mock_state, mock_state_with_events, MockEventBackend};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn sample_event() -> ContractEventDto {
        ContractEventDto {
            emitter: "aa".repeat(32),
            event_type: "0x1::coin::DepositEvent".into(),
            sequence_number: 0,
            data_hex: "deadbeef".into(),
            data_json: None,
            tx_digest: "cc".repeat(32),
            block_seq: 1,
            timestamp_ms: 1_700_000_000_000,
        }
    }

    fn sample_response(events: Vec<ContractEventDto>) -> EventQueryResponse {
        EventQueryResponse {
            events,
            next_cursor: None,
            has_more: false,
        }
    }

    #[tokio::test]
    async fn events_returns_503_when_backend_absent() {
        let app = router().with_state(mock_state());
        let req = Request::builder()
            .uri("/v2/events")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn events_returns_200_with_results() {
        let backend = MockEventBackend::new().with_response(sample_response(vec![sample_event()]));
        let app = router().with_state(mock_state_with_events(backend));
        let req = Request::builder()
            .uri("/v2/events")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let result: EventQueryResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(result.events.len(), 1);
        assert_eq!(result.events[0].event_type, "0x1::coin::DepositEvent");
        assert!(!result.has_more);
    }

    #[tokio::test]
    async fn events_returns_empty_result() {
        let backend = MockEventBackend::new().with_response(sample_response(vec![]));
        let app = router().with_state(mock_state_with_events(backend));
        let req = Request::builder()
            .uri("/v2/events")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let result: EventQueryResponse = serde_json::from_slice(&body).unwrap();
        assert!(result.events.is_empty());
    }

    #[tokio::test]
    async fn events_invalid_contract_address_returns_400() {
        let backend = MockEventBackend::new().with_response(sample_response(vec![]));
        let app = router().with_state(mock_state_with_events(backend));
        let req = Request::builder()
            .uri("/v2/events?contract=not_hex")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn events_with_query_params() {
        let backend = MockEventBackend::new().with_response(sample_response(vec![sample_event()]));
        let app = router().with_state(mock_state_with_events(backend));
        let req = Request::builder()
            .uri("/v2/events?limit=10&after_seq=0&before_seq=100")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
