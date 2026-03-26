//! REST API module — axum-based HTTP endpoints.
//!
//! # Endpoints
//!
//! | Method | Path | Description |
//! |--------|------|-------------|
//! | GET | `/health` | Liveness probe |
//! | GET | `/ready` | Readiness probe |
//! | GET | `/metrics` | Prometheus metrics scrape |
//! | GET | `/v2/account/:addr/balance` | Account balance query |
//! | GET | `/v2/tx/:hash/status` | Transaction receipt query |
//! | POST | `/v2/tx/submit` | Submit signed transaction for broadcast |
//! | POST | `/v2/intent/submit` | Submit signed intent |
//! | POST | `/v2/intent/estimate-gas` | Estimate gas for intent |
//! | GET  | `/v2/intent/:id/status` | Intent lifecycle status |
//! | POST | `/v2/contract/query` | Execute read-only view function |
//! | GET | `/v2/mcp/tools` | List exposed MCP tools |
//! | POST | `/v2/mcp/call` | Invoke an MCP tool |
//! | GET | `/v2/chain/head` | Latest committed block head |
//! | GET | `/v2/validators` | List active validators |
//! | GET | `/v2/validators/:index` | Single validator info |
//! | GET | `/v2/consensus/status` | Consensus engine status |
//! | GET | `/v2/consensus/epoch` | Current epoch information |
//! | POST | `/v2/faucet/mint` | Mint test tokens (devnet only) |
//! | GET | `/v2/admin/epoch/history` | Epoch transition history |
//! | POST | `/v2/admin/epoch/advance` | Manual epoch advance |
//! | POST | `/v2/admin/validator/slash` | Slash a validator |
//! | GET | `/v2/shards` | Current shard topology (W-5) |
//! | GET | `/v2/shards/:shard_id/head` | Per-shard chain head (W-5) |
//! | GET | `/v2/htlc/:lock_digest` | Query HTLC lock by digest (W-5) |
//! | GET | `/v2/htlc/pending` | List pending HTLC locks (W-5) |/// | GET | `/v2/sessions/:id` | Retrieve session by ID |
/// | GET | `/v2/sessions` | List sessions (optional `?active=true`) |
/// | GET | `/v2/provenance/:digest` | Retrieve provenance record by ID |
/// | GET | `/v2/provenance` | Query provenance feed (agent, session, or all) |
pub mod account;
pub mod chain;
pub mod consensus;
pub mod contract;
pub mod faucet;
pub mod health;
pub mod htlc;
pub mod intent;
pub mod mcp;
pub mod network;
pub mod prometheus;
pub mod proof;
pub mod provenance;
pub mod readiness;
pub mod session;
pub mod shard;
pub mod transaction;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use axum::Router;

use crate::error::{RpcError, RpcResult};
use nexus_primitives::AccountAddress;

// ── Shared helpers ──────────────────────────────────────────────────────

/// Parse a hex-encoded 32-byte address, returning `RpcError::BadRequest`
/// on invalid input.
pub fn parse_address(hex_str: &str) -> Result<AccountAddress, RpcError> {
    AccountAddress::from_hex(hex_str)
        .map_err(|e| RpcError::BadRequest(format!("invalid hex address: {e}")))
}

// ── Application state ───────────────────────────────────────────────────

/// Shared application state accessible by all handlers.
///
/// Uses `Arc<dyn Trait>` to abstract backend services, enabling
/// dependency injection and mock-based testing.
pub struct AppState {
    /// Query backend for account and transaction data.
    pub query: Arc<dyn QueryBackend>,
    /// Intent compilation backend (optional — `None` if intent service is unavailable).
    pub intent: Option<Arc<dyn IntentBackend>>,
    /// Consensus query backend (optional — `None` if consensus service is unavailable).
    pub consensus: Option<Arc<dyn ConsensusBackend>>,
    /// Network query backend (optional — `None` if network layer is unavailable).
    pub network: Option<Arc<dyn NetworkBackend>>,
    /// Transaction broadcaster for P2P gossip (optional).
    pub broadcaster: Option<Arc<dyn TransactionBroadcaster>>,
    /// Event broadcast sender for WebSocket subscriptions (optional).
    pub events: Option<tokio::sync::broadcast::Sender<crate::ws::NodeEvent>>,
    /// Rate limiter (optional — `None` disables rate limiting).
    pub rate_limiter: Option<Arc<crate::middleware::RateLimiter>>,
    /// Per-address faucet rate limiter (optional).
    pub faucet_addr_limiter: Option<Arc<crate::middleware::FaucetAddressLimiter>>,
    /// Prometheus metrics handle for `/metrics` scrape endpoint.
    pub metrics_handle: Option<metrics_exporter_prometheus::PrometheusHandle>,
    /// Whether the faucet endpoint is enabled (dev/testnet only).
    pub faucet_enabled: bool,
    /// Amount dispensed per faucet request (smallest unit).
    pub faucet_amount: u64,
    /// Maximum concurrent WebSocket connections.
    pub max_ws_connections: usize,
    /// Active WebSocket connection counter.
    pub ws_connection_count: std::sync::atomic::AtomicUsize,
    /// Intent lifecycle tracker (optional — `None` if intent tracking is disabled).
    pub intent_tracker: Option<Arc<crate::intent_tracker::IntentTracker>>,
    /// Session and provenance query backend (optional — `None` if not configured).
    pub session_provenance: Option<Arc<dyn SessionProvenanceBackend>>,
    /// State proof and commitment query backend (optional — `None` if commitment tracking is disabled).
    pub state_proof: Option<Arc<dyn StateProofBackend>>,
    /// MCP dispatcher used by the online MCP adapter (optional).
    pub mcp_dispatcher: Option<Arc<dyn nexus_intent::agent_core::dispatcher::DispatchBackend>>,
    /// Monotonic call index used to derive unique MCP request IDs.
    pub mcp_call_index: std::sync::atomic::AtomicU64,
    /// Per-tier quota manager for query / intent / MCP endpoints (optional).
    pub quota_manager: Option<Arc<crate::middleware::QuotaManager>>,
    /// Maximum gas budget for a single read-only view query.
    pub query_gas_budget: u64,
    /// Timeout for a single read-only view query (milliseconds).
    pub query_timeout_ms: u64,
    /// Number of active shards for auto-deriving `target_shard` on tx submit.
    pub num_shards: u16,
    /// Transaction lifecycle tracker for benchmark tooling (optional).
    pub tx_lifecycle: Option<Arc<crate::tx_lifecycle::TxLifecycleRegistry>>,
    /// HTLC query backend (optional — `None` if HTLC tracking is unavailable).
    pub htlc: Option<Arc<dyn HtlcBackend>>,
}

/// Abstraction over account/transaction queries.
///
/// Implementations include the real node backend and a mock for testing.
pub trait QueryBackend: Send + Sync + 'static {
    /// Query account balance for the given address and token.
    fn account_balance(
        &self,
        address: &nexus_primitives::AccountAddress,
        token: &nexus_primitives::TokenId,
    ) -> Result<nexus_primitives::Amount, RpcError>;

    /// Query a transaction receipt by digest.
    fn transaction_receipt(
        &self,
        digest: &nexus_primitives::TxDigest,
    ) -> Result<Option<crate::dto::TransactionReceiptDto>, RpcError>;

    /// Return the current node health status.
    fn health_status(&self) -> crate::dto::HealthResponse;

    /// Execute a read-only view function on a deployed contract.
    fn contract_query(
        &self,
        request: &crate::dto::ContractQueryRequest,
    ) -> Result<crate::dto::ContractQueryResponse, RpcError>;

    /// Return the latest committed block (chain head).
    /// Returns `None` if no block has been committed yet.
    fn chain_head(&self) -> Result<Option<crate::dto::ChainHeadDto>, RpcError> {
        Ok(None)
    }

    /// Mint faucet tokens for the given address (dev/testnet only).
    /// The default implementation returns an error.
    fn faucet_mint(
        &self,
        _address: &nexus_primitives::AccountAddress,
        _amount: u64,
    ) -> Result<(), RpcError> {
        Err(RpcError::Unavailable("faucet not implemented".into()))
    }
}

/// Abstraction over intent compilation and gas estimation.
pub trait IntentBackend: Send + Sync + 'static {
    /// Submit a signed intent for compilation, returning a plan.
    fn submit_intent(
        &self,
        intent: nexus_intent::types::SignedUserIntent,
    ) -> Pin<Box<dyn Future<Output = RpcResult<nexus_intent::types::CompiledIntentPlan>> + Send + '_>>;

    /// Estimate gas for an intent without executing.
    fn estimate_gas(
        &self,
        intent: nexus_intent::types::SignedUserIntent,
    ) -> Pin<Box<dyn Future<Output = RpcResult<nexus_intent::types::GasEstimate>> + Send + '_>>;
}

/// Abstraction over consensus and validator queries.
pub trait ConsensusBackend: Send + Sync + 'static {
    /// Return all active (non-slashed) validators.
    fn active_validators(&self) -> RpcResult<Vec<crate::dto::ValidatorInfoDto>>;

    /// Return a single validator by committee index.
    fn validator_info(
        &self,
        index: nexus_primitives::ValidatorIndex,
    ) -> RpcResult<crate::dto::ValidatorInfoDto>;

    /// Return current consensus engine status.
    fn consensus_status(&self) -> RpcResult<crate::dto::ConsensusStatusDto>;

    /// Return current epoch information.
    fn epoch_info(&self) -> RpcResult<crate::dto::EpochInfoDto> {
        Err(RpcError::Unavailable("epoch info not available".into()))
    }

    /// Return epoch transition history.
    fn epoch_history(&self) -> RpcResult<crate::dto::EpochHistoryResponse> {
        Err(RpcError::Unavailable("epoch history not available".into()))
    }

    /// Manually advance the epoch (governance action).
    fn advance_epoch(&self, _reason: &str) -> RpcResult<crate::dto::EpochAdvanceResponse> {
        Err(RpcError::Unavailable("epoch advance not available".into()))
    }

    /// Slash a validator by index (governance action).
    fn slash_validator(
        &self,
        _index: nexus_primitives::ValidatorIndex,
        _reason: &str,
    ) -> RpcResult<crate::dto::SlashValidatorResponse> {
        Err(RpcError::Unavailable("slashing not available".into()))
    }

    /// Return the latest election result (if any).
    fn election_result(&self) -> RpcResult<crate::dto::ElectionResultDto> {
        Err(RpcError::Unavailable(
            "election result not available".into(),
        ))
    }

    /// Return the current committee rotation policy.
    fn rotation_policy(&self) -> RpcResult<crate::dto::RotationPolicyDto> {
        Err(RpcError::Unavailable(
            "rotation policy not available".into(),
        ))
    }

    /// Return current staking validators snapshot.
    fn staking_validators(&self) -> RpcResult<crate::dto::StakingValidatorsResponse> {
        Err(RpcError::Unavailable(
            "staking validators not available".into(),
        ))
    }

    /// Return the current shard topology (W-5).
    fn shard_topology(&self) -> RpcResult<crate::dto::ShardTopologyDto> {
        Err(RpcError::Unavailable("shard topology not available".into()))
    }

    /// Return the chain head for a specific shard (W-5).
    fn shard_chain_head(&self, _shard_id: u16) -> RpcResult<crate::dto::ShardChainHeadDto> {
        Err(RpcError::Unavailable(
            "shard chain head not available".into(),
        ))
    }
}

/// Abstraction over transaction broadcasting to the P2P network.
pub trait TransactionBroadcaster: Send + Sync + 'static {
    /// Broadcast a BCS-encoded signed transaction to peers.
    fn broadcast_tx(
        &self,
        data: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = RpcResult<()>> + Send + '_>>;
}

/// Abstraction over P2P network status queries.
pub trait NetworkBackend: Send + Sync + 'static {
    /// List known peers.
    fn network_peers(&self) -> RpcResult<crate::dto::NetworkPeersResponse>;

    /// Return current network status (routing table health).
    fn network_status(&self) -> RpcResult<crate::dto::NetworkStatusResponse>;

    /// Return P2P network health summary.
    fn network_health(&self) -> RpcResult<crate::dto::NetworkHealthResponse>;
}

/// Abstraction over session and provenance queries.
pub trait SessionProvenanceBackend: Send + Sync + 'static {
    /// Retrieve a session by ID.
    fn get_session(
        &self,
        session_id: &nexus_primitives::Blake3Digest,
    ) -> Option<nexus_intent::AgentSession>;

    /// List all active (non-terminal) sessions.
    fn active_sessions(&self) -> Vec<nexus_intent::AgentSession>;

    /// List all sessions (both active and terminal).
    fn all_sessions(&self) -> Vec<nexus_intent::AgentSession>;

    /// Retrieve a provenance record by ID.
    fn get_provenance(
        &self,
        provenance_id: &nexus_primitives::Blake3Digest,
    ) -> Option<nexus_intent::ProvenanceRecord>;

    /// Query provenance records by agent ID.
    fn query_provenance_by_agent(
        &self,
        agent: &nexus_primitives::AccountAddress,
        params: &nexus_intent::agent_core::provenance::ProvenanceQueryParams,
    ) -> nexus_intent::agent_core::provenance::ProvenanceQueryResult;

    /// Query provenance records by session ID.
    fn query_provenance_by_session(
        &self,
        session_id: &nexus_primitives::Blake3Digest,
        params: &nexus_intent::agent_core::provenance::ProvenanceQueryParams,
    ) -> nexus_intent::agent_core::provenance::ProvenanceQueryResult;

    /// Get chronological provenance activity feed.
    fn provenance_activity_feed(
        &self,
        params: &nexus_intent::agent_core::provenance::ProvenanceQueryParams,
    ) -> nexus_intent::agent_core::provenance::ProvenanceQueryResult;

    /// Retrieve an anchor receipt by anchor digest.
    fn get_anchor_receipt(
        &self,
        anchor_digest: &nexus_primitives::Blake3Digest,
    ) -> Option<nexus_intent::AnchorReceipt>;

    /// List anchor receipts in batch-sequence order.
    fn list_anchor_receipts(&self, limit: u32) -> Vec<nexus_intent::AnchorReceipt>;
}

/// Abstraction over state proof and commitment queries.
pub trait StateProofBackend: Send + Sync + 'static {
    /// Return the current state commitment summary.
    fn commitment_info(&self) -> RpcResult<crate::dto::StateCommitmentDto>;

    /// Generate an inclusion/exclusion proof for a single key.
    fn prove_key(&self, key: &[u8]) -> RpcResult<(Option<Vec<u8>>, nexus_storage::MerkleProof)>;

    /// Generate proofs for multiple keys.
    fn prove_keys(
        &self,
        keys: &[Vec<u8>],
    ) -> RpcResult<Vec<(Option<Vec<u8>>, nexus_storage::MerkleProof)>>;

    /// Return the current commitment root.
    fn commitment_root(&self) -> RpcResult<nexus_primitives::Blake3Digest>;
}

/// Abstraction over HTLC lock queries (W-5).
pub trait HtlcBackend: Send + Sync + 'static {
    /// Retrieve an HTLC lock by its digest (hex-encoded).
    fn get_htlc_lock(
        &self,
        digest: &nexus_primitives::Blake3Digest,
    ) -> RpcResult<Option<crate::dto::HtlcLockDto>>;

    /// List pending HTLC locks, up to `limit` entries.
    fn list_pending_htlc_locks(&self, limit: u32) -> RpcResult<crate::dto::HtlcPendingListDto>;
}

/// Build the REST API router.
///
/// The returned [`Router`] should be composed into the top-level server.
pub fn rest_router(state: Arc<AppState>) -> Router {
    Router::new()
        .merge(health::router())
        .merge(readiness::router())
        .merge(prometheus::router())
        .merge(account::router())
        .merge(transaction::router())
        .merge(intent::router())
        .merge(mcp::router())
        .merge(consensus::router())
        .merge(network::router())
        .merge(contract::router())
        .merge(chain::router())
        .merge(faucet::router())
        .merge(session::router())
        .merge(provenance::router())
        .merge(proof::router())
        .merge(shard::router())
        .merge(htlc::router())
        .merge(crate::ws::router())
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::quota_middleware,
        ))
        .layer(axum::middleware::from_fn(
            crate::middleware::audit_middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::rate_limit_middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::metrics::metrics_middleware,
        ))
        .with_state(state)
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
pub(crate) mod test_helpers {
    use super::*;
    use crate::dto::{HealthResponse, TransactionReceiptDto};
    use nexus_primitives::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Mock query backend for testing.
    pub struct MockQueryBackend {
        pub balances: Mutex<HashMap<(AccountAddress, TokenId), Amount>>,
        pub receipts: Mutex<HashMap<TxDigest, TransactionReceiptDto>>,
    }

    impl MockQueryBackend {
        pub fn new() -> Self {
            Self {
                balances: Mutex::new(HashMap::new()),
                receipts: Mutex::new(HashMap::new()),
            }
        }

        pub fn with_balance(self, addr: AccountAddress, token: TokenId, amount: Amount) -> Self {
            self.balances
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert((addr, token), amount);
            self
        }

        pub fn with_receipt(self, digest: TxDigest, receipt: TransactionReceiptDto) -> Self {
            self.receipts
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(digest, receipt);
            self
        }
    }

    impl QueryBackend for MockQueryBackend {
        fn account_balance(
            &self,
            address: &AccountAddress,
            token: &TokenId,
        ) -> Result<Amount, RpcError> {
            let balances = self.balances.lock().unwrap_or_else(|e| e.into_inner());
            balances
                .get(&(*address, *token))
                .copied()
                .ok_or_else(|| RpcError::NotFound(format!("account {address:?} not found")))
        }

        fn transaction_receipt(
            &self,
            digest: &TxDigest,
        ) -> Result<Option<TransactionReceiptDto>, RpcError> {
            let receipts = self.receipts.lock().unwrap_or_else(|e| e.into_inner());
            Ok(receipts.get(digest).cloned())
        }

        fn health_status(&self) -> HealthResponse {
            HealthResponse {
                status: "healthy",
                version: env!("CARGO_PKG_VERSION"),
                peers: 0,
                epoch: EpochNumber(0),
                latest_commit: CommitSequence(0),
                uptime_seconds: 0,
                subsystems: Vec::new(),
                reason: None,
            }
        }

        fn contract_query(
            &self,
            _request: &crate::dto::ContractQueryRequest,
        ) -> Result<crate::dto::ContractQueryResponse, RpcError> {
            Err(RpcError::Unavailable(
                "contract query not yet implemented in mock backend".into(),
            ))
        }

        fn faucet_mint(&self, _address: &AccountAddress, _amount: u64) -> Result<(), RpcError> {
            Ok(())
        }
    }

    /// Create test app state with a mock backend (no intent or consensus backend).
    pub fn mock_state() -> Arc<AppState> {
        Arc::new(AppState {
            query: Arc::new(MockQueryBackend::new()),
            intent: None,
            consensus: None,
            network: None,
            broadcaster: None,
            events: None,
            rate_limiter: None,
            faucet_addr_limiter: None,
            metrics_handle: None,
            faucet_enabled: false,
            faucet_amount: 0,
            max_ws_connections: 100,
            ws_connection_count: std::sync::atomic::AtomicUsize::new(0),
            intent_tracker: None,
            session_provenance: None,
            state_proof: None,
            mcp_dispatcher: None,
            mcp_call_index: std::sync::atomic::AtomicU64::new(0),
            quota_manager: None,
            query_gas_budget: 10_000_000,
            query_timeout_ms: 5_000,
            num_shards: 1,
            tx_lifecycle: None,
            htlc: None,
        })
    }

    /// Create test app state with a custom mock backend (no intent or consensus backend).
    pub fn state_with_backend(backend: MockQueryBackend) -> Arc<AppState> {
        Arc::new(AppState {
            query: Arc::new(backend),
            intent: None,
            consensus: None,
            network: None,
            broadcaster: None,
            events: None,
            rate_limiter: None,
            faucet_addr_limiter: None,
            metrics_handle: None,
            faucet_enabled: false,
            faucet_amount: 0,
            max_ws_connections: 100,
            ws_connection_count: std::sync::atomic::AtomicUsize::new(0),
            intent_tracker: None,
            session_provenance: None,
            state_proof: None,
            mcp_dispatcher: None,
            mcp_call_index: std::sync::atomic::AtomicU64::new(0),
            quota_manager: None,
            query_gas_budget: 10_000_000,
            query_timeout_ms: 5_000,
            num_shards: 1,
            tx_lifecycle: None,
            htlc: None,
        })
    }

    /// Mock intent backend for testing.
    pub struct MockIntentBackend {
        pub submit_result: Mutex<Option<RpcResult<nexus_intent::types::CompiledIntentPlan>>>,
        pub estimate_result: Mutex<Option<RpcResult<nexus_intent::types::GasEstimate>>>,
    }

    impl MockIntentBackend {
        pub fn new() -> Self {
            Self {
                submit_result: Mutex::new(None),
                estimate_result: Mutex::new(None),
            }
        }

        pub fn with_submit_result(
            self,
            result: RpcResult<nexus_intent::types::CompiledIntentPlan>,
        ) -> Self {
            *self.submit_result.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
            self
        }

        pub fn with_estimate_result(
            self,
            result: RpcResult<nexus_intent::types::GasEstimate>,
        ) -> Self {
            *self
                .estimate_result
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(result);
            self
        }
    }

    impl IntentBackend for MockIntentBackend {
        fn submit_intent(
            &self,
            _intent: nexus_intent::types::SignedUserIntent,
        ) -> Pin<
            Box<
                dyn Future<Output = RpcResult<nexus_intent::types::CompiledIntentPlan>> + Send + '_,
            >,
        > {
            let result = self
                .submit_result
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
                .unwrap_or_else(|| Err(RpcError::Internal("no mock result configured".into())));
            Box::pin(std::future::ready(result))
        }

        fn estimate_gas(
            &self,
            _intent: nexus_intent::types::SignedUserIntent,
        ) -> Pin<Box<dyn Future<Output = RpcResult<nexus_intent::types::GasEstimate>> + Send + '_>>
        {
            let result = self
                .estimate_result
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
                .unwrap_or_else(|| Err(RpcError::Internal("no mock result configured".into())));
            Box::pin(std::future::ready(result))
        }
    }

    /// Create test app state with *both* query and intent backends.
    pub fn mock_state_with_intent(intent_backend: MockIntentBackend) -> Arc<AppState> {
        Arc::new(AppState {
            query: Arc::new(MockQueryBackend::new()),
            intent: Some(Arc::new(intent_backend)),
            consensus: None,
            network: None,
            broadcaster: None,
            events: None,
            rate_limiter: None,
            faucet_addr_limiter: None,
            metrics_handle: None,
            faucet_enabled: false,
            faucet_amount: 0,
            max_ws_connections: 100,
            ws_connection_count: std::sync::atomic::AtomicUsize::new(0),
            intent_tracker: None,
            session_provenance: None,
            state_proof: None,
            mcp_dispatcher: None,
            mcp_call_index: std::sync::atomic::AtomicU64::new(0),
            quota_manager: None,
            query_gas_budget: 10_000_000,
            query_timeout_ms: 5_000,
            num_shards: 1,
            tx_lifecycle: None,
            htlc: None,
        })
    }

    /// Mock consensus backend for testing.
    pub struct MockConsensusBackend {
        pub validators: Mutex<Vec<crate::dto::ValidatorInfoDto>>,
        pub status: Mutex<Option<crate::dto::ConsensusStatusDto>>,
        pub epoch_info: Mutex<Option<crate::dto::EpochInfoDto>>,
        pub epoch_history: Mutex<Option<crate::dto::EpochHistoryResponse>>,
    }

    impl MockConsensusBackend {
        pub fn new() -> Self {
            Self {
                validators: Mutex::new(Vec::new()),
                status: Mutex::new(None),
                epoch_info: Mutex::new(None),
                epoch_history: Mutex::new(None),
            }
        }

        pub fn with_validators(self, validators: Vec<crate::dto::ValidatorInfoDto>) -> Self {
            *self.validators.lock().unwrap_or_else(|e| e.into_inner()) = validators;
            self
        }

        pub fn with_status(self, status: crate::dto::ConsensusStatusDto) -> Self {
            *self.status.lock().unwrap_or_else(|e| e.into_inner()) = Some(status);
            self
        }

        pub fn with_epoch_info(self, info: crate::dto::EpochInfoDto) -> Self {
            *self.epoch_info.lock().unwrap_or_else(|e| e.into_inner()) = Some(info);
            self
        }

        pub fn with_epoch_history(self, history: crate::dto::EpochHistoryResponse) -> Self {
            *self.epoch_history.lock().unwrap_or_else(|e| e.into_inner()) = Some(history);
            self
        }
    }

    impl ConsensusBackend for MockConsensusBackend {
        fn active_validators(&self) -> RpcResult<Vec<crate::dto::ValidatorInfoDto>> {
            Ok(self
                .validators
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone())
        }

        fn validator_info(&self, index: ValidatorIndex) -> RpcResult<crate::dto::ValidatorInfoDto> {
            self.validators
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .find(|v| v.index == index)
                .cloned()
                .ok_or_else(|| RpcError::NotFound(format!("validator {} not found", index.0)))
        }

        fn consensus_status(&self) -> RpcResult<crate::dto::ConsensusStatusDto> {
            self.status
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
                .ok_or_else(|| RpcError::Internal("no mock status configured".into()))
        }

        fn epoch_info(&self) -> RpcResult<crate::dto::EpochInfoDto> {
            self.epoch_info
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
                .ok_or_else(|| RpcError::Unavailable("epoch info not available".into()))
        }

        fn epoch_history(&self) -> RpcResult<crate::dto::EpochHistoryResponse> {
            self.epoch_history
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
                .ok_or_else(|| RpcError::Unavailable("epoch history not available".into()))
        }
    }

    /// Create test app state with *both* query and consensus backends.
    pub fn mock_state_with_consensus(consensus_backend: MockConsensusBackend) -> Arc<AppState> {
        Arc::new(AppState {
            query: Arc::new(MockQueryBackend::new()),
            intent: None,
            consensus: Some(Arc::new(consensus_backend)),
            network: None,
            broadcaster: None,
            events: None,
            rate_limiter: None,
            faucet_addr_limiter: None,
            metrics_handle: None,
            faucet_enabled: false,
            faucet_amount: 0,
            max_ws_connections: 100,
            ws_connection_count: std::sync::atomic::AtomicUsize::new(0),
            intent_tracker: None,
            session_provenance: None,
            state_proof: None,
            mcp_dispatcher: None,
            mcp_call_index: std::sync::atomic::AtomicU64::new(0),
            quota_manager: None,
            query_gas_budget: 10_000_000,
            query_timeout_ms: 5_000,
            num_shards: 1,
            tx_lifecycle: None,
            htlc: None,
        })
    }

    /// Mock transaction broadcaster for testing.
    pub struct MockBroadcaster {
        /// Recorded payloads from broadcast calls.
        pub payloads: Mutex<Vec<Vec<u8>>>,
        /// If set, broadcast will return this error.
        pub fail_with: Mutex<Option<RpcError>>,
    }

    impl MockBroadcaster {
        pub fn new() -> Self {
            Self {
                payloads: Mutex::new(Vec::new()),
                fail_with: Mutex::new(None),
            }
        }

        pub fn failing(err: RpcError) -> Self {
            Self {
                payloads: Mutex::new(Vec::new()),
                fail_with: Mutex::new(Some(err)),
            }
        }
    }

    impl TransactionBroadcaster for MockBroadcaster {
        fn broadcast_tx(
            &self,
            data: Vec<u8>,
        ) -> Pin<Box<dyn Future<Output = RpcResult<()>> + Send + '_>> {
            self.payloads
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(data);
            let result = self
                .fail_with
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
                .map(Err)
                .unwrap_or(Ok(()));
            Box::pin(std::future::ready(result))
        }
    }

    /// Create test app state with a mock broadcaster.
    pub fn mock_state_with_broadcaster(broadcaster: MockBroadcaster) -> Arc<AppState> {
        Arc::new(AppState {
            query: Arc::new(MockQueryBackend::new()),
            intent: None,
            consensus: None,
            network: None,
            broadcaster: Some(Arc::new(broadcaster)),
            events: None,
            rate_limiter: None,
            faucet_addr_limiter: None,
            metrics_handle: None,
            faucet_enabled: false,
            faucet_amount: 0,
            max_ws_connections: 100,
            ws_connection_count: std::sync::atomic::AtomicUsize::new(0),
            intent_tracker: None,
            session_provenance: None,
            state_proof: None,
            mcp_dispatcher: None,
            mcp_call_index: std::sync::atomic::AtomicU64::new(0),
            quota_manager: None,
            query_gas_budget: 10_000_000,
            query_timeout_ms: 5_000,
            num_shards: 1,
            tx_lifecycle: None,
            htlc: None,
        })
    }
}
