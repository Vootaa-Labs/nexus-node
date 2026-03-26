//! Readiness probe endpoint.
//!
//! `GET /ready` — returns 200 when the node is ready to serve traffic,
//! 503 (Service Unavailable) when it is still initialising or degraded.
//!
//! Unlike `/health` (liveness probe), `/ready` is a binary gate used by
//! container orchestrators (Docker Compose healthcheck, k8s readinessProbe)
//! to decide whether the node should receive traffic.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use super::AppState;

/// Build the readiness router fragment.
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/ready", get(readiness_check))
}

/// Readiness response body.
#[derive(Debug, Clone, Serialize)]
struct ReadinessResponse {
    ready: bool,
    status: &'static str,
    reason: Option<&'static str>,
}

/// `GET /ready`
///
/// Ready when health status is `"healthy"` or `"degraded"`.
/// Returns 503 for `"bootstrapping"`, `"syncing"`, and `"halted"`.
async fn readiness_check(
    State(state): State<Arc<AppState>>,
) -> (StatusCode, Json<ReadinessResponse>) {
    let health = state.query.health_status();

    let ready = matches!(health.status, "healthy" | "degraded");

    if ready {
        (
            StatusCode::OK,
            Json(ReadinessResponse {
                ready: true,
                status: health.status,
                reason: None,
            }),
        )
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ReadinessResponse {
                ready: false,
                status: health.status,
                reason: Some(health.status),
            }),
        )
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
    async fn ready_returns_200_when_healthy() {
        let app = router().with_state(mock_state());
        let req = Request::builder()
            .uri("/ready")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["ready"], true);
        assert!(json["reason"].is_null());
    }
}
