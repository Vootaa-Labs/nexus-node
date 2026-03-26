//! Prometheus metrics scrape endpoint.
//!
//! `GET /metrics` — returns all registered metrics in Prometheus text
//! exposition format, suitable for scraping by Prometheus, Grafana Agent,
//! or OpenTelemetry Collector.

use std::sync::Arc;

use axum::extract::State;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;

use super::AppState;

/// Build the metrics scrape router fragment.
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/metrics", get(metrics_scrape))
}

/// `GET /metrics`
///
/// Renders all metrics collected by the `metrics` crate global recorder
/// into Prometheus text exposition format.  Returns an empty body with
/// correct content-type when no recorder handle is available.
async fn metrics_scrape(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let body = match state.metrics_handle {
        Some(ref handle) => handle.render(),
        None => String::new(),
    };
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rest::test_helpers::mock_state;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[tokio::test]
    async fn metrics_returns_200() {
        let app = router().with_state(mock_state());
        let req = Request::builder()
            .uri("/metrics")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let ct = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(ct.contains("text/plain"));
    }
}
