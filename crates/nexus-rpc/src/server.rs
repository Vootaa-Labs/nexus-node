// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! `RpcService` — top-level HTTP server lifecycle manager.
//!
//! Wires together the REST router, WebSocket handler, middleware stack,
//! and optional backends into a single `tokio::net::TcpListener`-bound
//! service.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::sync::broadcast;

use crate::middleware::{self, RateLimiter};
use crate::rest::{
    self, AppState, ConsensusBackend, IntentBackend, NetworkBackend, QueryBackend,
    SessionProvenanceBackend, TransactionBroadcaster,
};
use crate::ws::NodeEvent;

/// Builder for constructing an [`RpcService`].
pub struct RpcServiceBuilder {
    listen_addr: SocketAddr,
    query: Option<Arc<dyn QueryBackend>>,
    intent: Option<Arc<dyn IntentBackend>>,
    consensus: Option<Arc<dyn ConsensusBackend>>,
    network: Option<Arc<dyn NetworkBackend>>,
    broadcaster: Option<Arc<dyn TransactionBroadcaster>>,
    events_tx: Option<broadcast::Sender<NodeEvent>>,
    rate_limit_per_ip: Option<(u32, Duration)>,
    metrics_handle: Option<metrics_exporter_prometheus::PrometheusHandle>,
    faucet_enabled: bool,
    faucet_amount: u64,
    api_keys: Vec<String>,
    faucet_per_addr_limit_per_hour: u32,
    cors_allowed_origins: Vec<String>,
    max_ws_connections: usize,
    intent_tracker: Option<Arc<crate::intent_tracker::IntentTracker>>,
    session_provenance: Option<Arc<dyn SessionProvenanceBackend>>,
    state_proof: Option<Arc<dyn crate::rest::StateProofBackend>>,
    mcp_dispatcher: Option<Arc<dyn nexus_intent::agent_core::dispatcher::DispatchBackend>>,
    quota_manager: Option<Arc<crate::middleware::QuotaManager>>,
    query_gas_budget: u64,
    query_timeout_ms: u64,
    num_shards: u16,
    tx_lifecycle: Option<Arc<crate::tx_lifecycle::TxLifecycleRegistry>>,
    htlc: Option<Arc<dyn crate::rest::HtlcBackend>>,
    block: Option<Arc<dyn crate::rest::BlockBackend>>,
    event_backend: Option<Arc<dyn crate::rest::EventBackend>>,
}

impl RpcServiceBuilder {
    /// Start building a new RPC service.
    pub fn new(listen_addr: SocketAddr) -> Self {
        Self {
            listen_addr,
            query: None,
            intent: None,
            consensus: None,
            network: None,
            broadcaster: None,
            events_tx: None,
            rate_limit_per_ip: None,
            metrics_handle: None,
            faucet_enabled: false,
            faucet_amount: 0,
            api_keys: vec![],
            faucet_per_addr_limit_per_hour: 10,
            cors_allowed_origins: vec![],
            max_ws_connections: 10_000,
            intent_tracker: None,
            session_provenance: None,
            state_proof: None,
            mcp_dispatcher: None,
            quota_manager: None,
            query_gas_budget: 10_000_000,
            query_timeout_ms: 5_000,
            num_shards: 1,
            tx_lifecycle: None,
            htlc: None,
            block: None,
            event_backend: None,
        }
    }

    /// Set the query backend (required).
    pub fn query_backend(mut self, backend: Arc<dyn QueryBackend>) -> Self {
        self.query = Some(backend);
        self
    }

    /// Set the intent compilation backend (optional).
    pub fn intent_backend(mut self, backend: Arc<dyn IntentBackend>) -> Self {
        self.intent = Some(backend);
        self
    }

    /// Set the consensus query backend (optional).
    pub fn consensus_backend(mut self, backend: Arc<dyn ConsensusBackend>) -> Self {
        self.consensus = Some(backend);
        self
    }

    /// Set the network query backend (optional).
    pub fn network_backend(mut self, backend: Arc<dyn NetworkBackend>) -> Self {
        self.network = Some(backend);
        self
    }

    /// Set the transaction broadcaster for P2P gossip (optional).
    pub fn tx_broadcaster(mut self, broadcaster: Arc<dyn TransactionBroadcaster>) -> Self {
        self.broadcaster = Some(broadcaster);
        self
    }

    /// Set the event broadcast sender for WebSocket subscriptions.
    pub fn event_sender(mut self, tx: broadcast::Sender<NodeEvent>) -> Self {
        self.events_tx = Some(tx);
        self
    }

    /// Enable per-IP rate limiting.
    pub fn rate_limit(mut self, max_requests: u32, window: Duration) -> Self {
        self.rate_limit_per_ip = Some((max_requests, window));
        self
    }

    /// Set the Prometheus metrics handle for the `/metrics` scrape endpoint.
    pub fn metrics_handle(mut self, handle: metrics_exporter_prometheus::PrometheusHandle) -> Self {
        self.metrics_handle = Some(handle);
        self
    }

    /// Enable the faucet endpoint with the given mint amount per request.
    pub fn faucet(mut self, enabled: bool, amount: u64) -> Self {
        self.faucet_enabled = enabled;
        self.faucet_amount = amount;
        self
    }

    /// Set API keys for authenticated access.
    pub fn api_keys(mut self, keys: Vec<String>) -> Self {
        self.api_keys = keys;
        self
    }

    /// Set per-address faucet rate limit.
    pub fn faucet_per_addr_limit(mut self, limit: u32) -> Self {
        self.faucet_per_addr_limit_per_hour = limit;
        self
    }

    /// Set allowed CORS origins.  Empty = allow all (`*`).
    pub fn cors_allowed_origins(mut self, origins: Vec<String>) -> Self {
        self.cors_allowed_origins = origins;
        self
    }

    /// Set maximum concurrent WebSocket connections.
    pub fn max_ws_connections(mut self, max: usize) -> Self {
        self.max_ws_connections = max;
        self
    }

    /// Set the intent lifecycle tracker.
    pub fn intent_tracker(mut self, tracker: Arc<crate::intent_tracker::IntentTracker>) -> Self {
        self.intent_tracker = Some(tracker);
        self
    }

    /// Set the session and provenance query backend.
    pub fn session_provenance(mut self, backend: Arc<dyn SessionProvenanceBackend>) -> Self {
        self.session_provenance = Some(backend);
        self
    }

    /// Set the state proof backend.
    pub fn state_proof(mut self, backend: Arc<dyn crate::rest::StateProofBackend>) -> Self {
        self.state_proof = Some(backend);
        self
    }

    /// Set the live MCP dispatcher.
    pub fn mcp_dispatcher(
        mut self,
        dispatcher: Arc<dyn nexus_intent::agent_core::dispatcher::DispatchBackend>,
    ) -> Self {
        self.mcp_dispatcher = Some(dispatcher);
        self
    }

    /// Set the per-tier quota manager for query / intent / MCP endpoints.
    pub fn quota_manager(mut self, mgr: Arc<crate::middleware::QuotaManager>) -> Self {
        self.quota_manager = Some(mgr);
        self
    }

    /// Set the maximum gas budget for a single read-only view query.
    pub fn query_gas_budget(mut self, budget: u64) -> Self {
        self.query_gas_budget = budget;
        self
    }

    /// Set the timeout for a single read-only view query (milliseconds).
    pub fn query_timeout_ms(mut self, ms: u64) -> Self {
        self.query_timeout_ms = ms;
        self
    }

    /// Set the number of active shards for auto-deriving `target_shard`.
    pub fn num_shards(mut self, n: u16) -> Self {
        self.num_shards = n;
        self
    }

    /// Set the HTLC query backend (optional).
    pub fn htlc_backend(mut self, backend: Arc<dyn crate::rest::HtlcBackend>) -> Self {
        self.htlc = Some(backend);
        self
    }

    /// Set the block query backend (optional).
    pub fn block_backend(mut self, backend: Arc<dyn crate::rest::BlockBackend>) -> Self {
        self.block = Some(backend);
        self
    }

    /// Set the event query backend (optional).
    pub fn event_backend(mut self, backend: Arc<dyn crate::rest::EventBackend>) -> Self {
        self.event_backend = Some(backend);
        self
    }

    /// Set the in-memory transaction lifecycle tracker used by benchmark tooling.
    pub fn tx_lifecycle(mut self, tracker: Arc<crate::tx_lifecycle::TxLifecycleRegistry>) -> Self {
        self.tx_lifecycle = Some(tracker);
        self
    }

    /// Build the [`RpcService`].
    ///
    /// # Panics
    /// Panics if no query backend was set.
    pub fn build(self) -> RpcService {
        let query = self
            .query
            .expect("RpcServiceBuilder: query backend is required");

        let rate_limiter = self
            .rate_limit_per_ip
            .map(|(max, window)| Arc::new(RateLimiter::new(max, window)));

        let state = Arc::new(AppState {
            query,
            intent: self.intent,
            consensus: self.consensus,
            network: self.network,
            broadcaster: self.broadcaster,
            events: self.events_tx,
            rate_limiter,
            faucet_addr_limiter: if self.faucet_per_addr_limit_per_hour > 0 {
                Some(Arc::new(crate::middleware::FaucetAddressLimiter::new(
                    self.faucet_per_addr_limit_per_hour,
                )))
            } else {
                None
            },
            metrics_handle: self.metrics_handle,
            faucet_enabled: self.faucet_enabled,
            faucet_amount: self.faucet_amount,
            max_ws_connections: self.max_ws_connections,
            ws_connection_count: std::sync::atomic::AtomicUsize::new(0),
            intent_tracker: self.intent_tracker,
            session_provenance: self.session_provenance,
            state_proof: self.state_proof,
            mcp_dispatcher: self.mcp_dispatcher,
            mcp_call_index: std::sync::atomic::AtomicU64::new(0),
            quota_manager: self.quota_manager,
            query_gas_budget: self.query_gas_budget,
            query_timeout_ms: self.query_timeout_ms,
            num_shards: self.num_shards,
            tx_lifecycle: self.tx_lifecycle,
            htlc: self.htlc,
            block: self.block,
            event_backend: self.event_backend,
        });

        RpcService {
            listen_addr: self.listen_addr,
            state,
            api_keys: self.api_keys,
            cors_allowed_origins: self.cors_allowed_origins,
        }
    }
}

/// A configured RPC service ready to serve.
pub struct RpcService {
    listen_addr: SocketAddr,
    state: Arc<AppState>,
    api_keys: Vec<String>,
    cors_allowed_origins: Vec<String>,
}

impl RpcService {
    /// Create a builder for the service.
    pub fn builder(listen_addr: SocketAddr) -> RpcServiceBuilder {
        RpcServiceBuilder::new(listen_addr)
    }

    /// Return a reference to the shared state.
    pub fn state(&self) -> &Arc<AppState> {
        &self.state
    }

    /// Build the composed axum [`Router`] with all middleware applied.
    pub fn into_router(self) -> axum::Router {
        let api_keys = self.api_keys.clone();
        let cors_origins = self.cors_allowed_origins.clone();
        let router = rest::rest_router(self.state);
        middleware::apply_middleware(router, &api_keys, &cors_origins)
    }

    /// Bind to the configured address and serve until the `shutdown` signal
    /// fires.
    ///
    /// Returns the actual bound address (useful when binding to port 0).
    pub async fn serve(
        self,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> std::io::Result<SocketAddr> {
        let listener = TcpListener::bind(self.listen_addr).await?;
        let local_addr = listener.local_addr()?;

        tracing::info!(%local_addr, "RPC server listening");

        let router = self.into_router();
        // Provide ConnectInfo<SocketAddr> so rate-limit middleware can
        // extract the peer IP without relying on spoofable headers.
        let service = router.into_make_service_with_connect_info::<SocketAddr>();
        axum::serve(listener, service)
            .with_graceful_shutdown(shutdown)
            .await?;

        Ok(local_addr)
    }

    /// Bind to the configured address and serve until Ctrl-C.
    pub async fn serve_until_ctrl_c(self) -> std::io::Result<SocketAddr> {
        self.serve(async {
            tokio::signal::ctrl_c()
                .await
                .expect("failed to listen for ctrl-c");
        })
        .await
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rest::test_helpers::MockQueryBackend;

    #[test]
    fn builder_constructs_service() {
        let svc = RpcService::builder("127.0.0.1:0".parse().unwrap())
            .query_backend(Arc::new(MockQueryBackend::new()))
            .build();

        assert_eq!(svc.listen_addr.ip().to_string(), "127.0.0.1");
    }

    #[test]
    fn builder_with_all_options() {
        let (tx, _rx) = crate::ws::event_channel();
        let svc = RpcService::builder("0.0.0.0:8080".parse().unwrap())
            .query_backend(Arc::new(MockQueryBackend::new()))
            .event_sender(tx)
            .rate_limit(100, Duration::from_secs(60))
            .build();

        assert!(svc.state.events.is_some());
        assert!(svc.state.rate_limiter.is_some());
    }

    #[test]
    #[should_panic(expected = "query backend is required")]
    fn builder_panics_without_query() {
        let _ = RpcService::builder("127.0.0.1:0".parse().unwrap()).build();
    }

    #[test]
    fn into_router_succeeds() {
        let svc = RpcService::builder("127.0.0.1:0".parse().unwrap())
            .query_backend(Arc::new(MockQueryBackend::new()))
            .build();

        // Should not panic.
        let _router = svc.into_router();
    }
}
