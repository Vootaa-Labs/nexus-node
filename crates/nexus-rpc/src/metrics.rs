// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! RPC metrics collection.
//!
//! Uses the [`metrics`] crate for counter/histogram/gauge registration.
//! The actual exporter (Prometheus, StatsD, etc.) is configured by the
//! binary that embeds `nexus-rpc`.
//!
//! # Metric names
//!
//! | Name | Type | Labels | Description |
//! |------|------|--------|-------------|
//! | `rpc_requests_total` | Counter | `method`, `path`, `status` | Total HTTP requests |
//! | `rpc_request_duration_seconds` | Histogram | `method`, `path` | Request latency |
//! | `rpc_active_connections` | Gauge | — | Current WebSocket connections |
//! | `rpc_rate_limited_total` | Counter | — | Requests rejected by rate limiter |
//! | `proof_requests_total` | Counter | `endpoint`, `status` | Proof endpoint requests (ok/error) |
//! | `proof_request_duration_seconds` | Histogram | `endpoint` | Proof endpoint latency |
//! | `proof_batch_keys` | Histogram | — | Number of keys per batch proof request |
//! | `proof_commitment_queries_total` | Counter | — | Commitment summary queries |

use std::sync::Arc;
use std::time::Instant;

use axum::extract::{MatchedPath, State};
use axum::middleware::Next;
use axum::response::Response;

use crate::rest::AppState;

/// Record an HTTP request's method, path, status code, and latency.
pub async fn metrics_middleware(
    State(_state): State<Arc<AppState>>,
    matched_path: Option<MatchedPath>,
    req: axum::extract::Request,
    next: Next,
) -> Response {
    let method = req.method().to_string();
    let path = matched_path
        .map(|p| p.as_str().to_owned())
        .unwrap_or_else(|| "unknown".to_owned());

    let start = Instant::now();
    let response = next.run(req).await;
    let duration = start.elapsed();

    let status = response.status().as_u16().to_string();

    metrics::counter!("rpc_requests_total", "method" => method.clone(), "path" => path.clone(), "status" => status)
        .increment(1);
    metrics::histogram!("rpc_request_duration_seconds", "method" => method, "path" => path)
        .record(duration.as_secs_f64());

    response
}

/// Increment the active WebSocket connection gauge.
pub fn ws_connection_opened() {
    metrics::gauge!("rpc_active_connections").increment(1.0);
}

/// Decrement the active WebSocket connection gauge.
pub fn ws_connection_closed() {
    metrics::gauge!("rpc_active_connections").decrement(1.0);
}

/// Increment the rate-limited request counter.
pub fn rate_limited() {
    metrics::counter!("rpc_rate_limited_total").increment(1);
}

// ── ACE metrics ─────────────────────────────────────────────────────────

/// Record a new agent session creation.
pub fn ace_session_created() {
    metrics::counter!("ace_sessions_created_total").increment(1);
}

/// Record a policy denial.
pub fn ace_capability_denied() {
    metrics::counter!("ace_capability_denied_total").increment(1);
}

/// Record an A2A negotiation initiation.
pub fn ace_a2a_negotiation_started() {
    metrics::counter!("ace_a2a_negotiations_started_total").increment(1);
}

/// Record an A2A negotiation settlement.
pub fn ace_a2a_negotiation_settled() {
    metrics::counter!("ace_a2a_negotiations_settled_total").increment(1);
}

/// Record a plan execution.
pub fn ace_plan_executed() {
    metrics::counter!("ace_plans_executed_total").increment(1);
}

/// Record a provenance record creation.
pub fn ace_provenance_recorded() {
    metrics::counter!("ace_provenance_records_total").increment(1);
}

// ── Proof surface metrics (C-3) ─────────────────────────────────────────

/// Record a successful proof request (single or batch).
pub fn proof_request_ok(endpoint: &str) {
    metrics::counter!("proof_requests_total", "endpoint" => endpoint.to_owned(), "status" => "ok")
        .increment(1);
}

/// Record a failed proof request.
pub fn proof_request_err(endpoint: &str) {
    metrics::counter!("proof_requests_total", "endpoint" => endpoint.to_owned(), "status" => "error")
        .increment(1);
}

/// Record proof request latency in seconds.
pub fn proof_request_duration(endpoint: &str, duration_secs: f64) {
    metrics::histogram!("proof_request_duration_seconds", "endpoint" => endpoint.to_owned())
        .record(duration_secs);
}

/// Record the number of keys in a batch proof request.
pub fn proof_batch_size(count: usize) {
    metrics::histogram!("proof_batch_keys").record(count as f64);
}

/// Record a commitment query.
pub fn proof_commitment_queried() {
    metrics::counter!("proof_commitment_queries_total").increment(1);
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_gauge_operations_do_not_panic() {
        // These should not panic even without a recorder installed.
        ws_connection_opened();
        ws_connection_opened();
        ws_connection_closed();
    }

    #[test]
    fn rate_limited_counter_does_not_panic() {
        rate_limited();
        rate_limited();
    }

    #[test]
    fn ace_counters_do_not_panic() {
        ace_session_created();
        ace_capability_denied();
        ace_a2a_negotiation_started();
        ace_a2a_negotiation_settled();
        ace_plan_executed();
        ace_provenance_recorded();
    }

    #[test]
    fn proof_counters_do_not_panic() {
        proof_request_ok("commitment");
        proof_request_ok("proof");
        proof_request_ok("proofs");
        proof_request_err("proof");
        proof_request_duration("proof", 0.042);
        proof_batch_size(5);
        proof_commitment_queried();
    }
}
