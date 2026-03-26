//! Health check endpoint.
//!
//! `GET /health` — returns node status, version, and connectivity info.

use std::sync::Arc;

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};

use super::AppState;
use crate::dto::HealthResponse;

/// Build the health check router fragment.
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/health", get(health_check))
}

/// `GET /health`
async fn health_check(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    Json(state.query.health_status())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rest::test_helpers::mock_state;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[tokio::test]
    async fn health_returns_200() {
        let app = router().with_state(mock_state());
        let req = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn health_returns_json() {
        let app = router().with_state(mock_state());
        let req = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let health: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(health["status"], "healthy");
        assert_eq!(health["version"], env!("CARGO_PKG_VERSION"));
    }
}
