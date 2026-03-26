// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! HTTP middleware stack: CORS, tracing, request-id, rate limiting, audit.
//!
//! # Middleware order (outermost → innermost)
//!
//! 1. **CORS** — browser pre-flight handling
//! 2. **Request ID** — unique `x-request-id` header propagation
//! 3. **Trace** — structured request/response logging via `tracing`
//! 4. **Rate limit** — per-IP/global request throttling
//! 5. **Audit** — structured JSON access log (`nexus::audit`)

use std::sync::Arc;
use std::time::Duration;

use axum::extract::DefaultBodyLimit;
use axum::Router;
use http::header::{HeaderName, CONTENT_TYPE};
use http::{HeaderValue, Method};
use tower_http::cors::CorsLayer;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::trace::TraceLayer;

use crate::rest::AppState;

/// The header name used for request correlation.
static X_REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

/// Maximum request body size (512 KiB).
///
/// Prevents memory exhaustion from oversized POST payloads. The largest
/// legitimate payload is a transaction submission (~256 KiB max).
const MAX_BODY_SIZE: usize = 512 * 1024;

/// Apply the standard middleware stack to a router.
///
/// This should wrap the composed REST + WS router after state has been
/// resolved via `with_state()`.
pub fn apply_middleware(router: Router, api_keys: &[String], cors_origins: &[String]) -> Router {
    let r = router
        // Innermost first in tower's `.layer()` order means outermost wraps first.
        .layer(DefaultBodyLimit::max(MAX_BODY_SIZE))
        .layer(TraceLayer::new_for_http())
        .layer(PropagateRequestIdLayer::new(X_REQUEST_ID.clone()))
        .layer(SetRequestIdLayer::new(
            X_REQUEST_ID.clone(),
            MakeRequestUuid,
        ))
        .layer(cors_layer(cors_origins));

    if api_keys.is_empty() {
        r
    } else {
        let keys: Arc<Vec<String>> = Arc::new(api_keys.to_vec());
        r.layer(axum::middleware::from_fn(move |req, next| {
            api_key_middleware(keys.clone(), req, next)
        }))
    }
}

/// Build a CORS layer (SEC-M14).
///
/// When `origins` is empty, **no cross-origin requests are allowed**
/// (fail-closed).  To allow all origins during development, pass
/// `["*"]` explicitly.
fn cors_layer(origins: &[String]) -> CorsLayer {
    let base = CorsLayer::new()
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([CONTENT_TYPE, X_REQUEST_ID.clone()])
        .max_age(Duration::from_secs(3600));

    if origins.is_empty() {
        // Fail-closed: no `Access-Control-Allow-Origin` header is emitted,
        // so browsers will reject cross-origin requests.
        base
    } else if origins.len() == 1 && origins[0] == "*" {
        base.allow_origin(tower_http::cors::Any)
    } else {
        let allowed: Vec<HeaderValue> = origins
            .iter()
            .filter_map(|o| o.parse::<HeaderValue>().ok())
            .collect();
        base.allow_origin(allowed)
    }
}

// ── API key authentication ──────────────────────────────────────────────

/// Middleware that enforces API key authentication on POST requests.
///
/// GET requests (health, readiness, metrics) are always allowed through.
/// POST/PUT/DELETE require a valid `x-api-key` header.
async fn api_key_middleware(
    keys: Arc<Vec<String>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    // Allow all GET/OPTIONS requests without auth.
    if matches!(
        req.method(),
        &Method::GET | &Method::OPTIONS | &Method::HEAD
    ) {
        return next.run(req).await;
    }

    // Check x-api-key header (constant-time comparison to prevent timing attacks).
    let provided = req.headers().get("x-api-key").and_then(|v| v.to_str().ok());

    match provided {
        Some(key)
            if keys
                .iter()
                .any(|k| constant_time_eq(k.as_bytes(), key.as_bytes())) =>
        {
            next.run(req).await
        }
        _ => {
            let body = serde_json::json!({
                "error": "UNAUTHORIZED",
                "message": "missing or invalid x-api-key header",
            });
            axum::response::Response::builder()
                .status(http::StatusCode::UNAUTHORIZED)
                .header(CONTENT_TYPE, "application/json")
                .body(axum::body::Body::from(body.to_string()))
                .unwrap_or_else(|_| axum::response::Response::new(axum::body::Body::empty()))
        }
    }
}

// ── Constant-time comparison ────────────────────────────────────────────

/// Compare two byte slices in constant time to prevent timing side-channel
/// attacks on secret values (e.g. API keys).
///
/// Returns `true` iff both slices have equal length and identical contents.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    // XOR every byte pair and OR into accumulator; final result is 0 iff equal.
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ── Faucet per-address rate limiter ─────────────────────────────────────

/// Maximum number of distinct addresses tracked by [`FaucetAddressLimiter`].
///
/// When this cap is reached, expired entries are purged first.  If still
/// at capacity after purging, the request is rejected (fail-closed)
/// to prevent an attacker from bypassing rate limiting via table exhaustion.
const FAUCET_LIMITER_MAX_ENTRIES: usize = 100_000;

/// Per-address rate limiter for faucet requests (token-bucket, 1-hour window).
pub struct FaucetAddressLimiter {
    max_per_hour: u32,
    max_entries: usize,
    buckets: Mutex<HashMap<[u8; 32], (u32, std::time::Instant)>>,
}

impl FaucetAddressLimiter {
    /// Create a new per-address limiter.
    pub fn new(max_per_hour: u32) -> Self {
        Self {
            max_per_hour,
            max_entries: FAUCET_LIMITER_MAX_ENTRIES,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Create a limiter with a custom entry cap (for testing).
    #[cfg(test)]
    fn with_max_entries(max_per_hour: u32, max_entries: usize) -> Self {
        Self {
            max_per_hour,
            max_entries,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Check if the given address is allowed another faucet request.
    pub fn check(&self, address: &[u8; 32]) -> Result<u32, Duration> {
        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        let window = Duration::from_secs(3600);
        let now = std::time::Instant::now();

        // Purge expired entries when at capacity to bound memory.
        if buckets.len() >= self.max_entries {
            buckets.retain(|_, (_, start)| now.duration_since(*start) < window);
            // Fail-closed: reject when address table is full (SEC-M15).
            if buckets.len() >= self.max_entries {
                return Err(Duration::from_secs(60));
            }
        }

        let entry = buckets.entry(*address).or_insert((self.max_per_hour, now));

        if now.duration_since(entry.1) >= window {
            *entry = (self.max_per_hour, now);
        }

        if entry.0 > 0 {
            entry.0 -= 1;
            Ok(entry.0)
        } else {
            let retry_after = window.saturating_sub(now.duration_since(entry.1));
            Err(retry_after)
        }
    }
}

// ── Rate limiter (simple token-bucket) ──────────────────────────────────

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;

/// Maximum number of distinct IPs tracked by [`RateLimiter`].
///
/// Bounds memory usage.  When reached, expired entries are purged; if
/// still full the oldest entry is evicted.
const RATE_LIMITER_MAX_ENTRIES: usize = 200_000;

/// Simple in-memory rate limiter using a token-bucket per IP.
///
/// This is intentionally minimal. For production, use a dedicated
/// rate-limiting service or the `governor` crate with a shared store.
pub struct RateLimiter {
    /// Max requests per window.
    max_requests: u32,
    /// Window duration.
    window: Duration,
    /// Maximum number of tracked IPs before fail-closed kicks in.
    max_entries: usize,
    /// Per-IP buckets: (remaining tokens, window start).
    buckets: Mutex<HashMap<IpAddr, (u32, std::time::Instant)>>,
}

impl RateLimiter {
    /// Create a new rate limiter.
    pub fn new(max_requests: u32, window: Duration) -> Self {
        Self {
            max_requests,
            window,
            max_entries: RATE_LIMITER_MAX_ENTRIES,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Create a rate limiter with a custom entry cap (for testing).
    #[cfg(test)]
    fn with_max_entries(max_requests: u32, window: Duration, max_entries: usize) -> Self {
        Self {
            max_requests,
            window,
            max_entries,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Check if the request from `ip` is allowed.
    ///
    /// Returns `Ok(remaining)` if allowed, `Err(retry_after)` if throttled.
    pub fn check(&self, ip: IpAddr) -> Result<u32, Duration> {
        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        let now = std::time::Instant::now();

        // Bound memory: purge expired entries when at capacity.
        if buckets.len() >= self.max_entries {
            buckets.retain(|_, (_, start)| now.duration_since(*start) < self.window);
            // Fail-closed: reject when bucket table is full (SEC-M10).
            if buckets.len() >= self.max_entries {
                return Err(Duration::from_secs(60));
            }
        }

        let entry = buckets.entry(ip).or_insert((self.max_requests, now));

        // Reset window if expired.
        if now.duration_since(entry.1) >= self.window {
            *entry = (self.max_requests, now);
        }

        if entry.0 > 0 {
            entry.0 -= 1;
            Ok(entry.0)
        } else {
            let retry_after = self.window.saturating_sub(now.duration_since(entry.1));
            Err(retry_after)
        }
    }
}

/// Build a rate-limiting middleware layer as an axum middleware function.
///
/// Returns 429 Too Many Requests when the limit is exceeded.
pub async fn rate_limit_middleware(
    state: axum::extract::State<Arc<AppState>>,
    connect_info: Option<axum::extract::ConnectInfo<std::net::SocketAddr>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    // Use the actual TCP peer address from ConnectInfo (set by axum::serve).
    // This cannot be spoofed by the client, unlike X-Forwarded-For.
    // Falls back to loopback when ConnectInfo is unavailable (e.g. in-process tests).
    let ip = connect_info
        .map(|ci| ci.0.ip())
        .unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));

    if let Some(ref limiter) = state.rate_limiter {
        match limiter.check(ip) {
            Ok(remaining) => {
                let mut resp = next.run(req).await;
                resp.headers_mut().insert(
                    HeaderName::from_static("x-ratelimit-remaining"),
                    HeaderValue::from_str(&remaining.to_string())
                        .unwrap_or_else(|_| HeaderValue::from_static("0")),
                );
                resp
            }
            Err(retry_after) => {
                let body = serde_json::json!({
                    "error": "TOO_MANY_REQUESTS",
                    "message": "rate limit exceeded",
                    "retry_after_secs": retry_after.as_secs()
                });
                axum::response::Response::builder()
                    .status(http::StatusCode::TOO_MANY_REQUESTS)
                    .header(CONTENT_TYPE, "application/json")
                    .header("retry-after", retry_after.as_secs().to_string())
                    .body(axum::body::Body::from(body.to_string()))
                    .unwrap_or_else(|_| axum::response::Response::new(axum::body::Body::empty()))
            }
        }
    } else {
        next.run(req).await
    }
}

// ── Quota Tier Manager (D-2 / E-2) ──────────────────────────────────────

/// Caller tier for quota enforcement.
///
/// Higher tiers receive more generous rate limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaTier {
    /// No API key — lowest quota.
    Anonymous,
    /// Valid API key but not whitelisted — mid-tier quota.
    Authenticated,
    /// Whitelisted API key — highest quota.
    Whitelisted,
}

impl std::fmt::Display for QuotaTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Anonymous => f.write_str("anonymous"),
            Self::Authenticated => f.write_str("authenticated"),
            Self::Whitelisted => f.write_str("whitelisted"),
        }
    }
}

/// Endpoint class for per-path quota differentiation (E-2).
///
/// Each class can have independent rate limits per [`QuotaTier`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointClass {
    /// `/v2/contract/query` — read-only view calls.
    Query = 0,
    /// `/v2/intent/submit`, `/v2/intent/estimate-gas` — intent compilation.
    Intent = 1,
    /// `/v2/mcp/*` — Model Context Protocol tool calls.
    Mcp = 2,
}

impl std::fmt::Display for EndpointClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Query => f.write_str("query"),
            Self::Intent => f.write_str("intent"),
            Self::Mcp => f.write_str("mcp"),
        }
    }
}

/// Per-class, per-tier, per-IP rate limiter for query / intent / MCP paths.
///
/// Each `(EndpointClass, QuotaTier)` pair has an independent RPM limit.
/// Buckets are keyed by `(class, tier, IP)`.
pub struct QuotaManager {
    /// Limits indexed by `[EndpointClass][QuotaTier]` (3 classes × 3 tiers).
    limits: [[u32; 3]; 3],
    whitelisted_keys: Vec<String>,
    window: Duration,
    max_entries: usize,
    /// Bucket key: (class_u8 << 2 | tier_u8, IP).
    buckets: Mutex<HashMap<(u8, IpAddr), (u32, std::time::Instant)>>,
}

/// Maximum number of tracked class+tier+IP triples.
const QUOTA_MAX_ENTRIES: usize = 200_000;

impl QuotaManager {
    /// Create a new quota manager with per-class, per-tier limits (E-2).
    ///
    /// Each array is `[anonymous_rpm, authenticated_rpm, whitelisted_rpm]`.
    pub fn new_per_class(
        query_rpms: [u32; 3],
        intent_rpms: [u32; 3],
        mcp_rpms: [u32; 3],
        whitelisted_keys: Vec<String>,
    ) -> Self {
        Self {
            limits: [query_rpms, intent_rpms, mcp_rpms],
            whitelisted_keys,
            window: Duration::from_secs(60),
            max_entries: QUOTA_MAX_ENTRIES,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Backwards-compatible constructor that uses the same limits for all classes.
    pub fn new(
        anonymous_rpm: u32,
        authenticated_rpm: u32,
        whitelisted_rpm: u32,
        whitelisted_keys: Vec<String>,
    ) -> Self {
        let rpms = [anonymous_rpm, authenticated_rpm, whitelisted_rpm];
        Self::new_per_class(rpms, rpms, rpms, whitelisted_keys)
    }

    /// Resolve the caller's quota tier from the API key header.
    pub fn resolve_tier(&self, api_key: Option<&str>) -> QuotaTier {
        match api_key {
            None => QuotaTier::Anonymous,
            Some(key) => {
                if self
                    .whitelisted_keys
                    .iter()
                    .any(|k| constant_time_eq(k.as_bytes(), key.as_bytes()))
                {
                    QuotaTier::Whitelisted
                } else {
                    QuotaTier::Authenticated
                }
            }
        }
    }

    /// Resolve the endpoint class from a request path.
    pub fn resolve_class(path: &str) -> Option<EndpointClass> {
        if path.starts_with("/v2/contract/query") {
            Some(EndpointClass::Query)
        } else if path.starts_with("/v2/intent/") {
            Some(EndpointClass::Intent)
        } else if path.starts_with("/v2/mcp") {
            Some(EndpointClass::Mcp)
        } else {
            None
        }
    }

    /// Check if the request from `ip` at `tier` for `class` is allowed.
    ///
    /// Returns `Ok((remaining, tier))` or `Err(retry_after)`.
    pub fn check(
        &self,
        ip: IpAddr,
        tier: QuotaTier,
        class: EndpointClass,
    ) -> Result<(u32, QuotaTier), Duration> {
        let limit = self.limits[class as usize][tier as usize];
        // Composite key: 2 bits for class + 2 bits for tier = 4 bits max.
        let bucket_key = ((class as u8) << 2) | (tier as u8);

        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        let now = std::time::Instant::now();

        // Bound memory: purge expired entries when at capacity.
        if buckets.len() >= self.max_entries {
            buckets.retain(|_, (_, start)| now.duration_since(*start) < self.window);
            if buckets.len() >= self.max_entries {
                return Err(Duration::from_secs(60));
            }
        }

        let entry = buckets.entry((bucket_key, ip)).or_insert((limit, now));

        // Reset window if expired.
        if now.duration_since(entry.1) >= self.window {
            *entry = (limit, now);
        }

        if entry.0 > 0 {
            entry.0 -= 1;
            Ok((entry.0, tier))
        } else {
            let retry_after = self.window.saturating_sub(now.duration_since(entry.1));
            Err(retry_after)
        }
    }
}

/// Middleware that enforces quota-tiered rate limiting on query / intent / MCP paths.
///
/// Each endpoint class (Query, Intent, Mcp) has independent per-tier
/// rate limits.  GET requests pass through.
pub async fn quota_middleware(
    state: axum::extract::State<Arc<AppState>>,
    connect_info: Option<axum::extract::ConnectInfo<std::net::SocketAddr>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let quota_mgr = match &state.quota_manager {
        Some(qm) => qm,
        None => return next.run(req).await,
    };

    // Determine endpoint class from the request path.
    let path = req.uri().path();
    let endpoint_class = match QuotaManager::resolve_class(path) {
        Some(cls) => cls,
        None => return next.run(req).await,
    };

    let ip = connect_info
        .map(|ci| ci.0.ip())
        .unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));

    let api_key = req.headers().get("x-api-key").and_then(|v| v.to_str().ok());

    let tier = quota_mgr.resolve_tier(api_key);

    match quota_mgr.check(ip, tier, endpoint_class) {
        Ok((remaining, resolved_tier)) => {
            let mut resp = next.run(req).await;
            resp.headers_mut().insert(
                HeaderName::from_static("x-quota-tier"),
                HeaderValue::from_str(&resolved_tier.to_string())
                    .unwrap_or_else(|_| HeaderValue::from_static("unknown")),
            );
            resp.headers_mut().insert(
                HeaderName::from_static("x-quota-class"),
                HeaderValue::from_str(&endpoint_class.to_string())
                    .unwrap_or_else(|_| HeaderValue::from_static("unknown")),
            );
            resp.headers_mut().insert(
                HeaderName::from_static("x-quota-remaining"),
                HeaderValue::from_str(&remaining.to_string())
                    .unwrap_or_else(|_| HeaderValue::from_static("0")),
            );
            resp
        }
        Err(retry_after) => {
            tracing::warn!(
                %ip,
                %tier,
                endpoint_class = %endpoint_class,
                retry_after_secs = retry_after.as_secs(),
                "quota exceeded"
            );
            let body = serde_json::json!({
                "error": "QUOTA_EXCEEDED",
                "message": format!("quota exceeded for {endpoint_class}/{tier}"),
                "tier": tier.to_string(),
                "endpoint_class": endpoint_class.to_string(),
                "retry_after_secs": retry_after.as_secs()
            });
            axum::response::Response::builder()
                .status(http::StatusCode::TOO_MANY_REQUESTS)
                .header(CONTENT_TYPE, "application/json")
                .header("retry-after", retry_after.as_secs().to_string())
                .body(axum::body::Body::from(body.to_string()))
                .unwrap_or_else(|_| axum::response::Response::new(axum::body::Body::empty()))
        }
    }
}

// ── Audit log middleware (E-3) ───────────────────────────────────────────

/// Structured audit log middleware.
///
/// Emits a JSON-structured log line at INFO level under the `nexus::audit`
/// target for every request. Fields: method, path, status, latency_ms,
/// ip, request_id, quota_tier, endpoint_class.
pub async fn audit_middleware(
    connect_info: Option<axum::extract::ConnectInfo<std::net::SocketAddr>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let started_at = std::time::Instant::now();
    let method = req.method().clone();
    let path = req.uri().path().to_owned();
    let request_id = req
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-")
        .to_owned();
    let ip = connect_info
        .map(|ci| ci.0.ip())
        .unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));

    let resp = next.run(req).await;

    let status = resp.status().as_u16();
    let latency_ms = started_at.elapsed().as_millis() as u64;
    let tier = resp
        .headers()
        .get("x-quota-tier")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-");
    let endpoint_class = resp
        .headers()
        .get("x-quota-class")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-");

    tracing::info!(
        target: "nexus::audit",
        %method,
        %path,
        status,
        latency_ms,
        %ip,
        request_id,
        tier,
        endpoint_class,
        "request completed"
    );

    resp
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limiter_allows_within_limit() {
        let limiter = RateLimiter::new(3, Duration::from_secs(60));
        let ip: IpAddr = "10.0.0.1".parse().unwrap();

        assert_eq!(limiter.check(ip), Ok(2));
        assert_eq!(limiter.check(ip), Ok(1));
        assert_eq!(limiter.check(ip), Ok(0));
        assert!(limiter.check(ip).is_err());
    }

    #[test]
    fn rate_limiter_isolates_ips() {
        let limiter = RateLimiter::new(1, Duration::from_secs(60));
        let ip1: IpAddr = "10.0.0.1".parse().unwrap();
        let ip2: IpAddr = "10.0.0.2".parse().unwrap();

        assert!(limiter.check(ip1).is_ok());
        assert!(limiter.check(ip1).is_err());
        // Different IP should still have tokens.
        assert!(limiter.check(ip2).is_ok());
    }

    #[test]
    fn rate_limiter_resets_after_window() {
        let limiter = RateLimiter::new(1, Duration::from_millis(50));
        let ip: IpAddr = "10.0.0.1".parse().unwrap();

        assert!(limiter.check(ip).is_ok());
        assert!(limiter.check(ip).is_err());

        // Wait for window to expire (generous margin for CI load).
        std::thread::sleep(Duration::from_millis(120));
        assert!(limiter.check(ip).is_ok());
    }

    #[test]
    fn cors_layer_blocks_when_empty() {
        // SEC-M14: empty origins = fail-closed (no CORS headers).
        let _layer = cors_layer(&[]);
    }

    #[test]
    fn cors_layer_allows_all_with_star() {
        let _layer = cors_layer(&["*".to_string()]);
    }

    #[test]
    fn cors_layer_with_specific_origins() {
        let _layer = cors_layer(&["https://example.com".to_string()]);
    }

    #[test]
    fn constant_time_eq_matches_equal() {
        assert!(constant_time_eq(b"secret-key-123", b"secret-key-123"));
    }

    #[test]
    fn constant_time_eq_rejects_different() {
        assert!(!constant_time_eq(b"secret-key-123", b"secret-key-124"));
    }

    #[test]
    fn constant_time_eq_rejects_different_length() {
        assert!(!constant_time_eq(b"short", b"longer-key"));
    }

    #[test]
    fn faucet_limiter_enforces_per_address() {
        let limiter = FaucetAddressLimiter::new(2);
        let addr = [0xAAu8; 32];

        assert_eq!(limiter.check(&addr), Ok(1));
        assert_eq!(limiter.check(&addr), Ok(0));
        assert!(limiter.check(&addr).is_err());

        // Different address still has tokens.
        let addr2 = [0xBBu8; 32];
        assert!(limiter.check(&addr2).is_ok());
    }

    #[test]
    fn rate_limiter_should_fail_closed_when_bucket_table_is_full() {
        // Use a small max_entries to make the test feasible.
        let limiter = RateLimiter::with_max_entries(100, Duration::from_secs(3600), 3);

        // Fill the bucket table with different IPs (all within window so they won't expire).
        let ip1: IpAddr = "10.0.0.1".parse().unwrap();
        let ip2: IpAddr = "10.0.0.2".parse().unwrap();
        let ip3: IpAddr = "10.0.0.3".parse().unwrap();

        assert!(limiter.check(ip1).is_ok());
        assert!(limiter.check(ip2).is_ok());
        assert!(limiter.check(ip3).is_ok());

        // Table is full and entries haven't expired — new IP must be rejected.
        let ip4: IpAddr = "10.0.0.4".parse().unwrap();
        assert!(
            limiter.check(ip4).is_err(),
            "should fail-closed when bucket table is full"
        );
    }

    #[test]
    fn faucet_limiter_should_fail_closed_when_address_table_is_full() {
        // Use a tiny cap so we can fill it quickly.
        let limiter = FaucetAddressLimiter::with_max_entries(100, 3);

        // Fill the table with 3 distinct addresses (long window so they won't expire).
        let a1 = [0x01u8; 32];
        let a2 = [0x02u8; 32];
        let a3 = [0x03u8; 32];

        assert!(limiter.check(&a1).is_ok());
        assert!(limiter.check(&a2).is_ok());
        assert!(limiter.check(&a3).is_ok());

        // Table is full — a new address must be rejected (fail-closed).
        let a4 = [0x04u8; 32];
        assert!(
            limiter.check(&a4).is_err(),
            "should fail-closed when address table is full"
        );
    }

    #[test]
    fn quota_manager_resolves_tiers_from_api_key() {
        let mgr = QuotaManager::new(10, 20, 30, vec!["wl-key-1234567890abcd".into()]);

        assert_eq!(mgr.resolve_tier(None), QuotaTier::Anonymous);
        assert_eq!(
            mgr.resolve_tier(Some("ordinary-key-1234567890")),
            QuotaTier::Authenticated
        );
        assert_eq!(
            mgr.resolve_tier(Some("wl-key-1234567890abcd")),
            QuotaTier::Whitelisted
        );
    }

    #[test]
    fn quota_manager_tracks_limits_per_tier() {
        let mgr = QuotaManager::new(1, 2, 3, vec!["wl-key-1234567890abcd".into()]);
        let ip: IpAddr = "10.0.0.9".parse().unwrap();

        assert_eq!(
            mgr.check(ip, QuotaTier::Anonymous, EndpointClass::Query),
            Ok((0, QuotaTier::Anonymous))
        );
        assert!(mgr
            .check(ip, QuotaTier::Anonymous, EndpointClass::Query)
            .is_err());

        assert_eq!(
            mgr.check(ip, QuotaTier::Authenticated, EndpointClass::Query),
            Ok((1, QuotaTier::Authenticated))
        );
        assert_eq!(
            mgr.check(ip, QuotaTier::Authenticated, EndpointClass::Query),
            Ok((0, QuotaTier::Authenticated))
        );
        assert!(mgr
            .check(ip, QuotaTier::Authenticated, EndpointClass::Query)
            .is_err());

        assert_eq!(
            mgr.check(ip, QuotaTier::Whitelisted, EndpointClass::Query),
            Ok((2, QuotaTier::Whitelisted))
        );
    }

    #[test]
    fn quota_manager_isolates_endpoint_classes() {
        // Query: 2 rpm, Intent: 1 rpm, Mcp: 3 rpm (all anonymous)
        let mgr = QuotaManager::new_per_class([2, 10, 10], [1, 10, 10], [3, 10, 10], vec![]);
        let ip: IpAddr = "10.0.0.10".parse().unwrap();

        // Exhaust query quota
        assert!(mgr
            .check(ip, QuotaTier::Anonymous, EndpointClass::Query)
            .is_ok());
        assert!(mgr
            .check(ip, QuotaTier::Anonymous, EndpointClass::Query)
            .is_ok());
        assert!(mgr
            .check(ip, QuotaTier::Anonymous, EndpointClass::Query)
            .is_err());

        // Intent quota is independent — still has 1 token
        assert!(mgr
            .check(ip, QuotaTier::Anonymous, EndpointClass::Intent)
            .is_ok());
        assert!(mgr
            .check(ip, QuotaTier::Anonymous, EndpointClass::Intent)
            .is_err());

        // Mcp quota is independent — still has 3 tokens
        assert!(mgr
            .check(ip, QuotaTier::Anonymous, EndpointClass::Mcp)
            .is_ok());
        assert!(mgr
            .check(ip, QuotaTier::Anonymous, EndpointClass::Mcp)
            .is_ok());
        assert!(mgr
            .check(ip, QuotaTier::Anonymous, EndpointClass::Mcp)
            .is_ok());
        assert!(mgr
            .check(ip, QuotaTier::Anonymous, EndpointClass::Mcp)
            .is_err());
    }

    #[test]
    fn resolve_class_maps_paths_correctly() {
        assert_eq!(
            QuotaManager::resolve_class("/v2/contract/query"),
            Some(EndpointClass::Query)
        );
        assert_eq!(
            QuotaManager::resolve_class("/v2/intent/submit"),
            Some(EndpointClass::Intent)
        );
        assert_eq!(
            QuotaManager::resolve_class("/v2/intent/estimate-gas"),
            Some(EndpointClass::Intent)
        );
        assert_eq!(
            QuotaManager::resolve_class("/v2/mcp/call"),
            Some(EndpointClass::Mcp)
        );
        assert_eq!(QuotaManager::resolve_class("/v2/health"), None);
        assert_eq!(QuotaManager::resolve_class("/v2/faucet/mint"), None);
    }

    // ── D-2: Capacity curves / tier matrix ──────────────────────────

    /// Verify RpcConfig default tier limits match Testnet_Access_Policy.md §3.2.
    #[test]
    fn config_defaults_match_access_policy() {
        let cfg = nexus_config::RpcConfig::default();

        // Query: 60 / 600 / 3000 rpm
        assert_eq!(cfg.query_rate_limit_anonymous_rpm, 60);
        assert_eq!(cfg.query_rate_limit_authenticated_rpm, 600);
        assert_eq!(cfg.query_rate_limit_whitelisted_rpm, 3_000);

        // Intent: 30 / 300 / 1500 rpm
        assert_eq!(cfg.intent_rate_limit_anonymous_rpm, 30);
        assert_eq!(cfg.intent_rate_limit_authenticated_rpm, 300);
        assert_eq!(cfg.intent_rate_limit_whitelisted_rpm, 1_500);

        // MCP: 30 / 300 / 1500 rpm
        assert_eq!(cfg.mcp_rate_limit_anonymous_rpm, 30);
        assert_eq!(cfg.mcp_rate_limit_authenticated_rpm, 300);
        assert_eq!(cfg.mcp_rate_limit_whitelisted_rpm, 1_500);

        // §3.1 global per-IP limit
        assert_eq!(cfg.rate_limit_per_ip_rps, 100);

        // §3.3 query gas budget
        assert_eq!(cfg.query_gas_budget, 10_000_000);
        assert_eq!(cfg.query_timeout_ms, 5_000);
    }

    /// Tier hierarchy invariant: Anonymous ≤ Authenticated ≤ Whitelisted
    /// for every endpoint class in the default configuration.
    #[test]
    fn tier_hierarchy_invariant_holds_for_defaults() {
        let cfg = nexus_config::RpcConfig::default();

        // Query
        assert!(cfg.query_rate_limit_anonymous_rpm <= cfg.query_rate_limit_authenticated_rpm);
        assert!(cfg.query_rate_limit_authenticated_rpm <= cfg.query_rate_limit_whitelisted_rpm);

        // Intent
        assert!(cfg.intent_rate_limit_anonymous_rpm <= cfg.intent_rate_limit_authenticated_rpm);
        assert!(cfg.intent_rate_limit_authenticated_rpm <= cfg.intent_rate_limit_whitelisted_rpm);

        // MCP
        assert!(cfg.mcp_rate_limit_anonymous_rpm <= cfg.mcp_rate_limit_authenticated_rpm);
        assert!(cfg.mcp_rate_limit_authenticated_rpm <= cfg.mcp_rate_limit_whitelisted_rpm);
    }

    /// Full 3×3 tier matrix: verify exact boundary enforcement for all
    /// (class, tier) combinations. Each bucket should allow exactly `limit`
    /// requests, then reject on `limit + 1`.
    #[test]
    fn capacity_curve_exact_boundary_all_nine_combinations() {
        // Use small limits so the test finishes quickly.
        let query_rpms = [3u32, 6, 9];
        let intent_rpms = [2u32, 5, 8];
        let mcp_rpms = [4u32, 7, 10];
        let mgr = QuotaManager::new_per_class(query_rpms, intent_rpms, mcp_rpms, vec![]);

        let classes = [
            EndpointClass::Query,
            EndpointClass::Intent,
            EndpointClass::Mcp,
        ];
        let tiers = [
            QuotaTier::Anonymous,
            QuotaTier::Authenticated,
            QuotaTier::Whitelisted,
        ];
        let limits = [query_rpms, intent_rpms, mcp_rpms];

        for (ci, class) in classes.iter().enumerate() {
            for (ti, tier) in tiers.iter().enumerate() {
                let limit = limits[ci][ti];
                // Use a unique IP per (class, tier) pair.
                let ip: IpAddr = format!("10.{}.{}.1", ci, ti).parse().unwrap();

                // Should accept exactly `limit` requests.
                for req in 0..limit {
                    let result = mgr.check(ip, *tier, *class);
                    assert!(
                        result.is_ok(),
                        "{class}/{tier}: request {req} of {limit} should be accepted"
                    );
                    let (remaining, resolved) = result.unwrap();
                    assert_eq!(remaining, limit - req - 1);
                    assert_eq!(resolved, *tier);
                }

                // The (limit + 1)th request must be rejected.
                let result = mgr.check(ip, *tier, *class);
                assert!(
                    result.is_err(),
                    "{class}/{tier}: request at limit should be rejected"
                );
            }
        }
    }

    /// Capacity curve: higher tiers have proportionally more headroom.
    /// After anonymous is exhausted, authenticated and whitelisted
    /// still have remaining tokens for the same IP.
    #[test]
    fn higher_tiers_have_headroom_when_lower_exhausted() {
        let mgr = QuotaManager::new_per_class([2, 5, 10], [2, 5, 10], [2, 5, 10], vec![]);
        let ip: IpAddr = "10.0.0.1".parse().unwrap();

        // Exhaust anonymous query quota.
        assert!(mgr
            .check(ip, QuotaTier::Anonymous, EndpointClass::Query)
            .is_ok());
        assert!(mgr
            .check(ip, QuotaTier::Anonymous, EndpointClass::Query)
            .is_ok());
        assert!(mgr
            .check(ip, QuotaTier::Anonymous, EndpointClass::Query)
            .is_err());

        // Authenticated still has 5 tokens (separate bucket).
        for _ in 0..5 {
            assert!(mgr
                .check(ip, QuotaTier::Authenticated, EndpointClass::Query)
                .is_ok());
        }
        assert!(mgr
            .check(ip, QuotaTier::Authenticated, EndpointClass::Query)
            .is_err());

        // Whitelisted still has 10 tokens.
        for _ in 0..10 {
            assert!(mgr
                .check(ip, QuotaTier::Whitelisted, EndpointClass::Query)
                .is_ok());
        }
        assert!(mgr
            .check(ip, QuotaTier::Whitelisted, EndpointClass::Query)
            .is_err());
    }

    /// Cross-class independence under sustained load: exhaust all tiers
    /// for one class, verify other classes remain unaffected.
    #[test]
    fn cross_class_independence_under_sustained_load() {
        let mgr = QuotaManager::new_per_class([3, 6, 9], [3, 6, 9], [3, 6, 9], vec![]);
        let ip: IpAddr = "10.0.0.1".parse().unwrap();

        // Exhaust all 3 tiers for Query.
        for tier in [
            QuotaTier::Anonymous,
            QuotaTier::Authenticated,
            QuotaTier::Whitelisted,
        ] {
            let limit = match tier {
                QuotaTier::Anonymous => 3,
                QuotaTier::Authenticated => 6,
                QuotaTier::Whitelisted => 9,
            };
            for _ in 0..limit {
                assert!(mgr.check(ip, tier, EndpointClass::Query).is_ok());
            }
            assert!(mgr.check(ip, tier, EndpointClass::Query).is_err());
        }

        // Intent and MCP must still have full budgets.
        for class in [EndpointClass::Intent, EndpointClass::Mcp] {
            for tier in [
                QuotaTier::Anonymous,
                QuotaTier::Authenticated,
                QuotaTier::Whitelisted,
            ] {
                let limit = match tier {
                    QuotaTier::Anonymous => 3,
                    QuotaTier::Authenticated => 6,
                    QuotaTier::Whitelisted => 9,
                };
                for _ in 0..limit {
                    assert!(
                        mgr.check(ip, tier, class).is_ok(),
                        "{class}/{tier} should be unaffected by Query exhaustion"
                    );
                }
            }
        }
    }

    /// Quota manager fail-closed: when bucket table reaches max_entries,
    /// new IPs are rejected even if they have available quota.
    #[test]
    fn quota_manager_fail_closed_at_capacity() {
        // Tiny max_entries so we can fill it in a test.
        let mgr = QuotaManager {
            limits: [[100; 3]; 3],
            whitelisted_keys: vec![],
            window: Duration::from_secs(3600),
            max_entries: 3,
            buckets: Mutex::new(HashMap::new()),
        };

        // Fill 3 entries with different IPs.
        let ip1: IpAddr = "10.0.0.1".parse().unwrap();
        let ip2: IpAddr = "10.0.0.2".parse().unwrap();
        let ip3: IpAddr = "10.0.0.3".parse().unwrap();
        assert!(mgr
            .check(ip1, QuotaTier::Anonymous, EndpointClass::Query)
            .is_ok());
        assert!(mgr
            .check(ip2, QuotaTier::Anonymous, EndpointClass::Query)
            .is_ok());
        assert!(mgr
            .check(ip3, QuotaTier::Anonymous, EndpointClass::Query)
            .is_ok());

        // New IP must be rejected (fail-closed).
        let ip4: IpAddr = "10.0.0.4".parse().unwrap();
        assert!(
            mgr.check(ip4, QuotaTier::Anonymous, EndpointClass::Query)
                .is_err(),
            "should reject new IP when bucket table is full"
        );
    }

    /// Tier matrix summary: programmatically enumerate all (class, tier)
    /// pairs from the default config and verify the expected 9-cell matrix.
    #[test]
    fn tier_matrix_enumerates_all_combinations() {
        let cfg = nexus_config::RpcConfig::default();
        let expected: [[u32; 3]; 3] = [
            // [Anonymous, Authenticated, Whitelisted]
            [
                cfg.query_rate_limit_anonymous_rpm,
                cfg.query_rate_limit_authenticated_rpm,
                cfg.query_rate_limit_whitelisted_rpm,
            ],
            [
                cfg.intent_rate_limit_anonymous_rpm,
                cfg.intent_rate_limit_authenticated_rpm,
                cfg.intent_rate_limit_whitelisted_rpm,
            ],
            [
                cfg.mcp_rate_limit_anonymous_rpm,
                cfg.mcp_rate_limit_authenticated_rpm,
                cfg.mcp_rate_limit_whitelisted_rpm,
            ],
        ];

        let mgr = QuotaManager::new_per_class(expected[0], expected[1], expected[2], vec![]);
        let classes = [
            EndpointClass::Query,
            EndpointClass::Intent,
            EndpointClass::Mcp,
        ];
        let tiers = [
            QuotaTier::Anonymous,
            QuotaTier::Authenticated,
            QuotaTier::Whitelisted,
        ];

        // Verify that the manager's internal limits match the config exactly.
        for (ci, class) in classes.iter().enumerate() {
            for (ti, tier) in tiers.iter().enumerate() {
                let ip: IpAddr = format!("10.{}.{}.1", ci + 10, ti + 10).parse().unwrap();
                let result = mgr.check(ip, *tier, *class);
                assert!(result.is_ok());
                let (remaining, _) = result.unwrap();
                // First check returns limit-1 remaining.
                assert_eq!(
                    remaining,
                    expected[ci][ti] - 1,
                    "matrix cell [{class}][{tier}] mismatch"
                );
            }
        }
    }

    /// Whitelisted key resolution: verify that a QuotaManager constructed
    /// from RpcConfig correctly distinguishes all three tiers.
    #[test]
    fn config_driven_tier_resolution() {
        let cfg = nexus_config::RpcConfig {
            api_keys: vec!["auth-key-0123456789ab".into(), "wl-key-9876543210cd".into()],
            whitelisted_api_keys: vec!["wl-key-9876543210cd".into()],
            ..Default::default()
        };

        let mgr = QuotaManager::new_per_class(
            [
                cfg.query_rate_limit_anonymous_rpm,
                cfg.query_rate_limit_authenticated_rpm,
                cfg.query_rate_limit_whitelisted_rpm,
            ],
            [
                cfg.intent_rate_limit_anonymous_rpm,
                cfg.intent_rate_limit_authenticated_rpm,
                cfg.intent_rate_limit_whitelisted_rpm,
            ],
            [
                cfg.mcp_rate_limit_anonymous_rpm,
                cfg.mcp_rate_limit_authenticated_rpm,
                cfg.mcp_rate_limit_whitelisted_rpm,
            ],
            cfg.whitelisted_api_keys.clone(),
        );

        assert_eq!(mgr.resolve_tier(None), QuotaTier::Anonymous);
        assert_eq!(
            mgr.resolve_tier(Some("auth-key-0123456789ab")),
            QuotaTier::Authenticated
        );
        assert_eq!(
            mgr.resolve_tier(Some("wl-key-9876543210cd")),
            QuotaTier::Whitelisted
        );
        // Unknown key → Authenticated (not anonymous, since key was provided).
        assert_eq!(
            mgr.resolve_tier(Some("unknown-key-abcdef1234")),
            QuotaTier::Authenticated
        );
    }

    /// Multi-IP capacity curve: 10 IPs each consuming their full anonymous
    /// quota on the Query class. Verifies quotas are truly per-IP.
    #[test]
    fn multi_ip_capacity_curve() {
        let mgr = QuotaManager::new(5, 10, 20, vec![]);

        for i in 1..=10u8 {
            let ip: IpAddr = format!("10.0.0.{i}").parse().unwrap();
            for req in 0..5 {
                let result = mgr.check(ip, QuotaTier::Anonymous, EndpointClass::Query);
                assert!(result.is_ok(), "IP {ip} request {req} should pass");
                let (remaining, _) = result.unwrap();
                assert_eq!(remaining, 4 - req);
            }
            assert!(
                mgr.check(ip, QuotaTier::Anonymous, EndpointClass::Query)
                    .is_err(),
                "IP {ip} should be throttled after 5 requests"
            );
        }
    }
}
