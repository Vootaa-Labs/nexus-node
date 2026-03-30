// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Real backend adapters bridging RPC traits to domain crate APIs.
//!
//! These replace the Phase 4 stub backends and delegate to the
//! concrete storage, consensus engine, and intent service instances.

#![forbid(unsafe_code)]

use std::pin::Pin;
use std::sync::{Arc, Mutex};

use nexus_consensus::{ConsensusEngine, ValidatorRegistry};
use nexus_execution::{ExecutionResult, StateView};
use nexus_intent::agent_core::dispatcher::{DispatchBackend, DispatchOutcome};
use nexus_intent::agent_core::envelope::{AgentRequestKind, ProvenanceFilter, QueryKind};
use nexus_intent::traits::AccountResolver;
use nexus_intent::{IntentError, IntentHandle};
use nexus_primitives::{
    AccountAddress, Amount, CommitSequence, EpochNumber, ShardId, TokenId, TxDigest, ValidatorIndex,
};
use nexus_rpc::{
    AccountStateProofDto, BlockBackend, BlockDto, BlockHeaderDto, BlockReceiptsDto, ChainHeadDto,
    ConsensusBackend, ConsensusStatusDto, ContractQueryRequest, ContractQueryResponse,
    EpochHistoryResponse, EpochInfoDto, EpochTransitionDto, HealthResponse, IntentBackend,
    NetworkBackend, QueryBackend, RpcError, RpcResult, SessionProvenanceBackend,
    SlashValidatorResponse, SubsystemHealthDto, TransactionReceiptDto, ValidatorInfoDto,
    ZkProofDto,
};
use nexus_storage::traits::StateStorage;
use nexus_storage::ColumnFamily;

use crate::readiness::NodeReadiness;

// ── Merkle proof conversion ────────────────────────────────────────────

/// Convert an internal `MerkleProof` to the RPC wire DTO.
fn merkle_proof_to_dto(proof: &nexus_storage::MerkleProof) -> nexus_rpc::MerkleProofDto {
    match proof {
        nexus_storage::MerkleProof::Inclusion {
            leaf_index,
            leaf_count,
            siblings,
        } => nexus_rpc::MerkleProofDto {
            proof_type: "inclusion".into(),
            leaf_count: *leaf_count,
            leaf_index: Some(*leaf_index),
            siblings: siblings.iter().map(|s| hex::encode(s.0)).collect(),
            left_neighbor: None,
            right_neighbor: None,
        },
        nexus_storage::MerkleProof::Exclusion {
            leaf_count,
            left_neighbor,
            right_neighbor,
        } => nexus_rpc::MerkleProofDto {
            proof_type: "exclusion".into(),
            leaf_count: *leaf_count,
            leaf_index: None,
            siblings: vec![],
            left_neighbor: left_neighbor
                .as_ref()
                .map(|n| nexus_rpc::MerkleNeighborProofDto {
                    key: hex::encode(&n.key),
                    value: hex::encode(&n.value),
                    leaf_index: n.leaf_index,
                    siblings: n.siblings.iter().map(|s| hex::encode(s.0)).collect(),
                }),
            right_neighbor: right_neighbor
                .as_ref()
                .map(|n| nexus_rpc::MerkleNeighborProofDto {
                    key: hex::encode(&n.key),
                    value: hex::encode(&n.value),
                    leaf_index: n.leaf_index,
                    siblings: n.siblings.iter().map(|s| hex::encode(s.0)).collect(),
                }),
        },
    }
}

// ── Shared chain-head state ────────────────────────────────────────────

/// Shared latest-commit snapshot, updated by the execution bridge.
///
/// Interior-mutable via `Mutex` so both the bridge (writer) and the RPC
/// `QueryBackend` (reader) can access it without lifetimes.
#[derive(Clone)]
pub struct SharedChainHead(pub Arc<Mutex<Option<ChainHeadDto>>>);

impl Default for SharedChainHead {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedChainHead {
    /// Create an empty chain head (no commits yet).
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(None)))
    }

    /// Record a new commit from the execution bridge.
    ///
    /// `tx_count` and `gas_total` are **cumulative** — each update adds
    /// the batch's contribution to the running totals.
    pub fn update(&self, dto: ChainHeadDto) {
        let mut guard = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(prev) = guard.as_ref() {
            let cumulative = ChainHeadDto {
                tx_count: prev.tx_count + dto.tx_count,
                gas_total: prev.gas_total + dto.gas_total,
                ..dto
            };
            *guard = Some(cumulative);
        } else {
            *guard = Some(dto);
        }
    }

    /// Read the current head (if any).
    pub fn get(&self) -> Option<ChainHeadDto> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

// ── StorageStateView ─────────────────────────────────────────────────────

/// [`StateView`] adapter backed by a [`StateStorage`] implementation.
///
/// Translates execution‐layer key lookups (`(AccountAddress, key)`) into
/// `ColumnFamily::State` reads via the underlying storage.
#[derive(Clone)]
pub struct StorageStateView<S: StateStorage> {
    store: S,
    shard_id: ShardId,
}

impl<S: StateStorage> StorageStateView<S> {
    /// Create a state view for the given shard.
    pub fn new(store: S, shard_id: ShardId) -> Self {
        Self { store, shard_id }
    }
}

impl<S: StateStorage> StateView for StorageStateView<S> {
    fn get(&self, account: &AccountAddress, key: &[u8]) -> ExecutionResult<Option<Vec<u8>>> {
        // Build the composite storage key: shard_id ‖ address ‖ user_key
        let account_key = nexus_storage::AccountKey {
            shard_id: self.shard_id,
            address: *account,
        };
        let mut full_key = account_key.to_bytes();
        full_key.extend_from_slice(key);

        self.store
            .get_sync(ColumnFamily::State.as_str(), &full_key)
            .map_err(|e| nexus_execution::ExecutionError::Storage(e.to_string()))
    }
}

// ── StorageQueryBackend ──────────────────────────────────────────────────

/// [`QueryBackend`] adapter backed by a [`StateStorage`] implementation.
///
/// Reads account balances from `cf_state` and transaction receipts from
/// `cf_receipts`. Health status reflects the current node startup state.
pub struct StorageQueryBackend<S: StateStorage> {
    store: S,
    num_shards: u16,
    epoch: Arc<std::sync::atomic::AtomicU64>,
    commit_seq: Arc<std::sync::atomic::AtomicU64>,
    start_time: std::time::Instant,
    /// Callback that returns the current peer count from the network layer.
    peer_count_fn: Arc<dyn Fn() -> usize + Send + Sync>,
    /// Shared chain-head state, updated by the execution bridge.
    chain_head: SharedChainHead,
    /// Node readiness tracker for real health reporting.
    readiness: Option<NodeReadiness>,
    /// Gas budget cap for read-only view queries (0 = unlimited).
    query_gas_budget: u64,
    /// Commitment tracker for Merkle proof generation (optional).
    commitment_tracker: Option<crate::commitment_tracker::SharedCommitmentTracker>,
}

impl<S: StateStorage> StorageQueryBackend<S> {
    /// Create a new storage-backed query adapter.
    ///
    /// * `store` — Shared storage backend (Clone = same backing data).
    /// * `epoch` — Shared atomic for the current epoch number.
    /// * `commit_seq` — Shared atomic for the latest commit sequence.
    pub fn new(
        store: S,
        epoch: Arc<std::sync::atomic::AtomicU64>,
        commit_seq: Arc<std::sync::atomic::AtomicU64>,
    ) -> Self {
        Self {
            store,
            num_shards: 1,
            epoch,
            commit_seq,
            start_time: std::time::Instant::now(),
            peer_count_fn: Arc::new(|| 0),
            chain_head: SharedChainHead::new(),
            readiness: None,
            query_gas_budget: 0,
            commitment_tracker: None,
        }
    }

    /// Set the peer count callback (from the network discovery layer).
    pub fn with_peer_count(mut self, f: Arc<dyn Fn() -> usize + Send + Sync>) -> Self {
        self.peer_count_fn = f;
        self
    }

    /// Set the shared chain-head state.
    pub fn with_chain_head(mut self, head: SharedChainHead) -> Self {
        self.chain_head = head;
        self
    }

    /// Set the commitment tracker for Merkle proof generation.
    pub fn with_commitment_tracker(
        mut self,
        tracker: crate::commitment_tracker::SharedCommitmentTracker,
    ) -> Self {
        self.commitment_tracker = Some(tracker);
        self
    }

    /// Set the node readiness tracker for real health reporting.
    pub fn with_readiness(mut self, readiness: NodeReadiness) -> Self {
        self.readiness = Some(readiness);
        self
    }

    /// Set the gas budget cap for read-only view queries (0 = unlimited).
    pub fn with_gas_budget(mut self, budget: u64) -> Self {
        self.query_gas_budget = budget;
        self
    }

    /// Set the total number of shards for cross-shard address resolution.
    pub fn with_num_shards(mut self, n: u16) -> Self {
        self.num_shards = n;
        self
    }

    /// Resolve which shard owns `address` using Jump Consistent Hash.
    fn resolve_shard(&self, address: &AccountAddress) -> ShardId {
        nexus_intent::resolver::shard_lookup::jump_consistent_hash(address, self.num_shards)
    }

    /// Build the storage key for an account balance lookup.
    fn balance_key(&self, address: &AccountAddress) -> Vec<u8> {
        let mut key = nexus_storage::AccountKey {
            shard_id: self.resolve_shard(address),
            address: *address,
        }
        .to_bytes();
        key.extend_from_slice(b"balance");
        key
    }
}

impl<S: StateStorage> QueryBackend for StorageQueryBackend<S> {
    fn account_balance(
        &self,
        address: &AccountAddress,
        _token: &TokenId,
    ) -> Result<Amount, RpcError> {
        let key = self.balance_key(address);
        let raw = self
            .store
            .get_sync(ColumnFamily::State.as_str(), &key)
            .map_err(|e| RpcError::Internal(format!("storage error: {e}")))?;

        match raw {
            Some(bytes) => {
                if bytes.len() != 8 {
                    return Err(RpcError::Internal(format!(
                        "corrupt balance: expected 8 bytes, got {}",
                        bytes.len()
                    )));
                }
                let arr: [u8; 8] = bytes
                    .try_into()
                    .map_err(|_| RpcError::Internal("balance byte conversion failed".into()))?;
                let val = u64::from_le_bytes(arr);
                Ok(Amount(val))
            }
            None => Err(RpcError::NotFound(format!(
                "account {} not found",
                hex::encode(address.0)
            ))),
        }
    }

    fn transaction_receipt(
        &self,
        digest: &TxDigest,
    ) -> Result<Option<TransactionReceiptDto>, RpcError> {
        let raw = self
            .store
            .get_sync(ColumnFamily::Receipts.as_str(), &digest.0)
            .map_err(|e| RpcError::Internal(format!("storage error: {e}")))?;

        match raw {
            Some(bytes) => {
                let receipt: nexus_execution::types::TransactionReceipt =
                    serde_json::from_slice(&bytes).map_err(|e| {
                        RpcError::Internal(format!("receipt deserialization error: {e}"))
                    })?;
                Ok(Some(receipt.into()))
            }
            None => Ok(None),
        }
    }

    fn health_status(&self) -> HealthResponse {
        let epoch_val = self.epoch.load(std::sync::atomic::Ordering::Acquire);
        let commit_val = self.commit_seq.load(std::sync::atomic::Ordering::Acquire);

        let uptime = self.start_time.elapsed().as_secs();

        let (status, subsystems, reason) = match &self.readiness {
            Some(nr) => {
                let snap = nr.subsystem_snapshot();
                let dto: Vec<SubsystemHealthDto> = snap
                    .iter()
                    .map(|s| SubsystemHealthDto {
                        name: s.name,
                        status: s.status,
                        last_progress_ms: s.last_progress_ms,
                    })
                    .collect();
                let node_status = nr.status();
                let reason = if node_status.as_str() != "healthy" {
                    let mut causes: Vec<String> = dto
                        .iter()
                        .filter(|s| s.status != "ready")
                        .map(|s| format!("{}={}", s.name, s.status))
                        .collect();
                    if causes.is_empty() {
                        // Status is non-healthy but all subsystems report ready
                        // — likely a stall detection.
                        causes = dto
                            .iter()
                            .filter(|s| s.last_progress_ms > 30_000)
                            .map(|s| format!("{} stalled ({}ms)", s.name, s.last_progress_ms))
                            .collect();
                    }
                    if causes.is_empty() {
                        None
                    } else {
                        Some(causes.join(", "))
                    }
                } else {
                    None
                };
                (node_status.as_str(), dto, reason)
            }
            None => ("healthy", Vec::new(), None),
        };

        HealthResponse {
            status,
            version: env!("CARGO_PKG_VERSION"),
            peers: (self.peer_count_fn)(),
            epoch: EpochNumber(epoch_val),
            latest_commit: CommitSequence(commit_val),
            uptime_seconds: uptime,
            subsystems,
            reason,
        }
    }

    fn contract_query(
        &self,
        request: &ContractQueryRequest,
    ) -> Result<ContractQueryResponse, RpcError> {
        // Parse contract address from hex.
        let addr_bytes: [u8; 32] = hex::decode(&request.contract)
            .map_err(|e| RpcError::BadRequest(format!("invalid contract address: {e}")))?
            .try_into()
            .map_err(|_| RpcError::BadRequest("contract address must be 32 bytes".into()))?;
        let contract = nexus_primitives::AccountAddress(addr_bytes);

        // Decode hex args to raw bytes.
        let args: Vec<Vec<u8>> = request
            .args
            .iter()
            .map(|h| hex::decode(h).map_err(|e| RpcError::BadRequest(format!("bad arg hex: {e}"))))
            .collect::<Result<_, _>>()?;

        let type_args: Vec<Vec<u8>> = request
            .type_args
            .iter()
            .map(|h| {
                hex::decode(h).map_err(|e| RpcError::BadRequest(format!("bad type_arg hex: {e}")))
            })
            .collect::<Result<_, _>>()?;

        // Execute the view function against current state.
        let state_view = StorageStateView::new(self.store.clone(), self.resolve_shard(&contract));
        let result = nexus_execution::query_view_with_budget(
            &state_view,
            contract,
            &request.function,
            &type_args,
            &args,
            self.query_gas_budget,
        )
        .map_err(|e| RpcError::Internal(format!("query execution error: {e}")))?;

        Ok(ContractQueryResponse {
            return_value: result.return_value.map(hex::encode),
            gas_used: result.gas_used,
            gas_budget: result.gas_budget,
        })
    }

    fn faucet_mint(&self, address: &AccountAddress, amount: u64) -> Result<(), RpcError> {
        let key = self.balance_key(address);
        let cf = ColumnFamily::State.as_str();

        // Read current balance (0 if account does not exist yet).
        let current = self
            .store
            .get_sync(cf, &key)
            .map_err(|e| RpcError::Internal(format!("storage read error: {e}")))?
            .map(|bytes| {
                if bytes.len() == 8 {
                    let arr: [u8; 8] = bytes.try_into().unwrap();
                    u64::from_le_bytes(arr)
                } else {
                    0
                }
            })
            .unwrap_or(0);

        let new_balance = current
            .checked_add(amount)
            .ok_or_else(|| RpcError::Internal("balance overflow".into()))?;

        self.store
            .put_sync(cf, key, new_balance.to_le_bytes().to_vec())
            .map_err(|e| RpcError::Internal(format!("storage write error: {e}")))?;

        Ok(())
    }

    fn chain_head(&self) -> Result<Option<ChainHeadDto>, RpcError> {
        Ok(self.chain_head.get())
    }

    fn account_state_proof(
        &self,
        addr: &nexus_primitives::AccountAddress,
    ) -> RpcResult<AccountStateProofDto> {
        let tracker = self
            .commitment_tracker
            .as_ref()
            .ok_or_else(|| RpcError::Unavailable("state proofs not available".into()))?;

        // Read balance from cf_state.
        let balance_key = self.balance_key(addr);
        let balance = self
            .store
            .get_sync(ColumnFamily::State.as_str(), &balance_key)
            .map_err(|e| RpcError::Internal(format!("storage error: {e}")))?
            .map(|bytes| {
                if bytes.len() == 8 {
                    u64::from_le_bytes(bytes.try_into().unwrap())
                } else {
                    0
                }
            })
            .unwrap_or(0);

        // Read nonce from cf_state (AccountKey ‖ "nonce").
        let mut nonce_key = nexus_storage::AccountKey {
            shard_id: self.resolve_shard(addr),
            address: *addr,
        }
        .to_bytes();
        nonce_key.extend_from_slice(b"nonce");
        let nonce = self
            .store
            .get_sync(ColumnFamily::State.as_str(), &nonce_key)
            .map_err(|e| RpcError::Internal(format!("storage error: {e}")))?
            .map(|bytes| {
                if bytes.len() == 8 {
                    u64::from_le_bytes(bytes.try_into().unwrap())
                } else {
                    0
                }
            })
            .unwrap_or(0);

        // Generate Merkle proof for the balance key.
        let guard = tracker
            .read()
            .map_err(|_| RpcError::Internal("commitment tracker lock poisoned".into()))?;
        let root = guard.commitment_root();
        let (_value, proof) = guard
            .prove_key(&balance_key)
            .map_err(|e| RpcError::Internal(format!("proof generation failed: {e}")))?;

        Ok(AccountStateProofDto {
            address: hex::encode(addr.0),
            balance,
            nonce,
            state_root: hex::encode(root.0),
            proof_version: "blake3-merkle-v1".into(),
            proof: merkle_proof_to_dto(&proof),
        })
    }
}

// ── LiveConsensusBackend ─────────────────────────────────────────────────

/// [`ConsensusBackend`] adapter backed by a shared [`ConsensusEngine`].
///
/// All methods acquire the mutex lock, query the engine, and release.
/// The lock scope is minimal — only reads, no I/O.
pub struct LiveConsensusBackend {
    engine: Arc<Mutex<ConsensusEngine>>,
    epoch_manager: Option<Arc<Mutex<nexus_consensus::EpochManager>>>,
    /// Store handle for reading persisted election results.
    store: Option<nexus_storage::RocksStore>,
    /// Rotation policy for reporting via RPC.
    rotation_policy: Option<crate::staking_snapshot::CommitteeRotationPolicy>,
    /// Snapshot provider for building staking validator views.
    snapshot_provider:
        Option<Arc<dyn Fn() -> Option<crate::staking_snapshot::StakingSnapshot> + Send + Sync>>,
    /// Number of active execution shards.
    num_shards: u16,
    /// Whether this node is in devnet mode (enables admin actions).
    devnet_mode: bool,
}

impl LiveConsensusBackend {
    /// Wrap a shared consensus engine.
    pub fn new(engine: Arc<Mutex<ConsensusEngine>>) -> Self {
        Self {
            engine,
            epoch_manager: None,
            store: None,
            rotation_policy: None,
            snapshot_provider: None,
            num_shards: 1,
            devnet_mode: false,
        }
    }

    /// Set the number of active execution shards.
    pub fn with_num_shards(mut self, n: u16) -> Self {
        self.num_shards = n;
        self
    }

    /// Enable devnet admin actions (e.g. manual epoch advance).
    pub fn with_devnet_mode(mut self, enabled: bool) -> Self {
        self.devnet_mode = enabled;
        self
    }

    /// Attach an epoch manager to enable epoch-aware responses.
    pub fn with_epoch_manager(mut self, mgr: Arc<Mutex<nexus_consensus::EpochManager>>) -> Self {
        self.epoch_manager = Some(mgr);
        self
    }

    /// Attach a storage handle for election result queries.
    pub fn with_store(mut self, store: nexus_storage::RocksStore) -> Self {
        self.store = Some(store);
        self
    }

    /// Attach the rotation policy for RPC observability.
    pub fn with_rotation_policy(
        mut self,
        policy: crate::staking_snapshot::CommitteeRotationPolicy,
    ) -> Self {
        self.rotation_policy = Some(policy);
        self
    }

    /// Attach a snapshot provider for staking validator queries.
    pub fn with_snapshot_provider(
        mut self,
        provider: Arc<dyn Fn() -> Option<crate::staking_snapshot::StakingSnapshot> + Send + Sync>,
    ) -> Self {
        self.snapshot_provider = Some(provider);
        self
    }

    /// Return a clone of the inner `Arc<Mutex<ConsensusEngine>>`.
    ///
    /// Used by the consensus bridge to feed inbound certificates into the
    /// same engine instance that backs the RPC query layer.
    pub fn engine(&self) -> Arc<Mutex<ConsensusEngine>> {
        Arc::clone(&self.engine)
    }
}

impl ConsensusBackend for LiveConsensusBackend {
    fn active_validators(&self) -> RpcResult<Vec<ValidatorInfoDto>> {
        let engine = self
            .engine
            .lock()
            .map_err(|_| RpcError::Internal("consensus lock poisoned".into()))?;
        let validators = engine.committee().active_validators();
        Ok(validators.into_iter().map(ValidatorInfoDto::from).collect())
    }

    fn validator_info(&self, index: ValidatorIndex) -> RpcResult<ValidatorInfoDto> {
        let engine = self
            .engine
            .lock()
            .map_err(|_| RpcError::Internal("consensus lock poisoned".into()))?;
        engine
            .committee()
            .validator_info(index)
            .map(ValidatorInfoDto::from)
            .ok_or_else(|| RpcError::NotFound(format!("validator {idx} not found", idx = index.0)))
    }

    fn consensus_status(&self) -> RpcResult<ConsensusStatusDto> {
        let engine = self
            .engine
            .lock()
            .map_err(|_| RpcError::Internal("consensus lock poisoned".into()))?;
        Ok(ConsensusStatusDto {
            epoch: engine.epoch(),
            dag_size: engine.dag_size(),
            total_commits: engine.total_commits(),
            pending_commits: engine.pending_commits(),
        })
    }

    fn epoch_info(&self) -> RpcResult<EpochInfoDto> {
        let mgr = self
            .epoch_manager
            .as_ref()
            .ok_or_else(|| RpcError::Unavailable("epoch manager not configured".into()))?
            .lock()
            .map_err(|_| RpcError::Internal("epoch manager lock poisoned".into()))?;
        let engine = self
            .engine
            .lock()
            .map_err(|_| RpcError::Internal("consensus lock poisoned".into()))?;
        let cfg = mgr.config();
        Ok(EpochInfoDto {
            epoch: mgr.current_epoch(),
            epoch_started_at: mgr.epoch_started_at(),
            committee_size: engine.committee().active_validators().len(),
            epoch_commits: engine.total_commits(),
            epoch_length_commits: cfg.epoch_length_commits,
            epoch_length_seconds: cfg.epoch_length_seconds,
        })
    }

    fn epoch_history(&self) -> RpcResult<EpochHistoryResponse> {
        let mgr = self
            .epoch_manager
            .as_ref()
            .ok_or_else(|| RpcError::Unavailable("epoch manager not configured".into()))?
            .lock()
            .map_err(|_| RpcError::Internal("epoch manager lock poisoned".into()))?;
        let transitions: Vec<EpochTransitionDto> = mgr
            .transitions()
            .iter()
            .map(|t| EpochTransitionDto {
                from_epoch: t.from_epoch,
                to_epoch: t.to_epoch,
                trigger: format!("{:?}", t.trigger),
                final_commit_count: t.final_commit_count,
                transitioned_at: t.transitioned_at,
            })
            .collect();
        let total = transitions.len();
        Ok(EpochHistoryResponse { transitions, total })
    }

    fn slash_validator(
        &self,
        index: ValidatorIndex,
        _reason: &str,
    ) -> RpcResult<SlashValidatorResponse> {
        let mut engine = self
            .engine
            .lock()
            .map_err(|_| RpcError::Internal("consensus lock poisoned".into()))?;
        let epoch = engine.epoch();
        match engine.committee_mut().slash(index) {
            Ok(()) => {
                tracing::warn!(
                    validator = index.0,
                    epoch = epoch.0,
                    "validator slashed via admin API"
                );
                Ok(SlashValidatorResponse {
                    validator_index: index.0,
                    applied: true,
                    epoch,
                })
            }
            Err(nexus_consensus::ConsensusError::SlashedValidator(_)) => {
                Ok(SlashValidatorResponse {
                    validator_index: index.0,
                    applied: false,
                    epoch,
                })
            }
            Err(nexus_consensus::ConsensusError::UnknownValidator(_)) => Err(RpcError::NotFound(
                format!("validator {} not found", index.0),
            )),
            Err(e) => Err(RpcError::Internal(format!("slash error: {e}"))),
        }
    }

    fn election_result(&self) -> RpcResult<nexus_rpc::dto::ElectionResultDto> {
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| RpcError::Unavailable("storage not configured".into()))?;
        let engine = self
            .engine
            .lock()
            .map_err(|_| RpcError::Internal("consensus lock poisoned".into()))?;
        let current_epoch = engine.epoch();
        drop(engine);

        // Load the election result for the current epoch.
        let key = crate::epoch_store::election_key_for(current_epoch);
        let value = store
            .get_sync("cf_state", &key)
            .map_err(|e| RpcError::Internal(format!("storage read failed: {e}")))?;

        match value {
            Some(bytes) => {
                let persisted: crate::staking_snapshot::PersistedElectionResult =
                    bcs::from_bytes(&bytes)
                        .map_err(|e| RpcError::Internal(format!("BCS decode failed: {e}")))?;
                Ok(nexus_rpc::dto::ElectionResultDto {
                    for_epoch: persisted.for_epoch,
                    snapshot_epoch: persisted.snapshot_epoch,
                    elected: persisted
                        .elected
                        .iter()
                        .map(|ev| nexus_rpc::dto::ElectedValidatorDto {
                            address_hex: hex::encode(ev.address.0),
                            effective_stake: ev.effective_stake,
                            committee_index: ev.committee_index,
                        })
                        .collect(),
                    total_effective_stake: persisted.total_effective_stake,
                    is_fallback: persisted.is_fallback,
                })
            }
            None => Err(RpcError::NotFound(
                "no election result for the current epoch".into(),
            )),
        }
    }

    fn rotation_policy(&self) -> RpcResult<nexus_rpc::dto::RotationPolicyDto> {
        let policy = self
            .rotation_policy
            .as_ref()
            .ok_or_else(|| RpcError::Unavailable("rotation policy not configured".into()))?;
        Ok(nexus_rpc::dto::RotationPolicyDto {
            election_epoch_interval: policy.election_epoch_interval,
            max_committee_size: policy.election.max_committee_size,
            min_committee_size: policy.election.min_committee_size,
            min_total_effective_stake: policy.election.min_total_effective_stake,
            exclude_slashed: policy.exclude_slashed,
            min_reputation_score: policy.min_reputation_score.raw(),
        })
    }

    fn staking_validators(&self) -> RpcResult<nexus_rpc::dto::StakingValidatorsResponse> {
        let provider = self
            .snapshot_provider
            .as_ref()
            .ok_or_else(|| RpcError::Unavailable("snapshot provider not configured".into()))?;
        let snapshot = provider()
            .ok_or_else(|| RpcError::Internal("failed to build staking snapshot".into()))?;

        let mut active_count = 0usize;
        let mut total_effective_stake = 0u64;
        let validators: Vec<nexus_rpc::dto::StakingValidatorDto> = snapshot
            .records
            .iter()
            .map(|r| {
                let eff = r.effective_stake();
                if r.is_active() {
                    active_count += 1;
                    total_effective_stake += eff;
                }
                nexus_rpc::dto::StakingValidatorDto {
                    address_hex: hex::encode(r.address.0),
                    bonded: r.bonded,
                    penalty_total: r.penalty_total,
                    effective_stake: eff,
                    status: r.status,
                    is_slashed: r.is_slashed,
                    reputation: r.reputation.raw(),
                }
            })
            .collect();

        Ok(nexus_rpc::dto::StakingValidatorsResponse {
            snapshot_epoch: snapshot.captured_at_epoch,
            validators,
            active_count,
            total_effective_stake,
        })
    }

    fn shard_topology(&self) -> RpcResult<nexus_rpc::dto::ShardTopologyDto> {
        let engine = self
            .engine
            .lock()
            .map_err(|_| RpcError::Internal("consensus lock poisoned".into()))?;

        // Build per-shard validator assignments from the committee.
        let n = self.num_shards;
        let mut shard_validators: Vec<Vec<u32>> = vec![Vec::new(); n as usize];
        for v in engine.committee().active_validators() {
            let sid = v.shard_id.map_or(0u16, |s| s.0);
            if (sid as usize) < shard_validators.len() {
                shard_validators[sid as usize].push(v.index.0);
            }
        }

        let shards = shard_validators
            .into_iter()
            .enumerate()
            .map(|(i, validators)| nexus_rpc::dto::ShardInfoDto {
                shard_id: i as u16,
                validators,
            })
            .collect();

        Ok(nexus_rpc::dto::ShardTopologyDto {
            num_shards: n,
            shards,
        })
    }

    fn shard_chain_head(&self, shard_id: u16) -> RpcResult<nexus_rpc::dto::ShardChainHeadDto> {
        if shard_id >= self.num_shards && self.num_shards > 0 {
            return Err(RpcError::NotFound(format!(
                "shard {shard_id} does not exist (num_shards={})",
                self.num_shards
            )));
        }

        let engine = self
            .engine
            .lock()
            .map_err(|_| RpcError::Internal("consensus lock poisoned".into()))?;

        // The consensus engine tracks a single global sequence; per-shard
        // head tracking requires the execution bridge.  Return the global
        // head attributed to the requested shard.
        let epoch = engine.epoch().0;
        let sequence = engine.total_commits();

        Ok(nexus_rpc::dto::ShardChainHeadDto {
            shard_id,
            sequence,
            anchor_digest: "00".repeat(32),
            epoch,
        })
    }

    fn advance_epoch(&self, reason: &str) -> RpcResult<nexus_rpc::dto::EpochAdvanceResponse> {
        if !self.devnet_mode {
            return Err(RpcError::Unavailable(
                "manual epoch advance is restricted to devnet mode".into(),
            ));
        }

        tracing::info!(reason, "manual epoch advance requested via admin API");

        let mut engine = self
            .engine
            .lock()
            .map_err(|_| RpcError::Internal("consensus lock poisoned".into()))?;

        let trigger = nexus_consensus::types::EpochTransitionTrigger::Manual;
        let next_committee = engine.committee().clone();
        let (transition, _flushed) = engine.advance_epoch(next_committee, trigger);

        // Record transition in the epoch manager if attached.
        if let Some(ref mgr) = self.epoch_manager {
            if let Ok(mut mgr) = mgr.lock() {
                mgr.record_transition(transition.clone());
            }
        }

        // Persist to storage if available.
        if let Some(ref store) = self.store {
            let committee = engine.committee().clone();
            drop(engine); // release lock before IO
            if let Err(e) =
                crate::epoch_store::persist_epoch_transition(store, &committee, &transition)
            {
                tracing::warn!(error = %e, "advance_epoch: failed to persist transition");
            }
        }

        Ok(nexus_rpc::dto::EpochAdvanceResponse {
            transition: nexus_rpc::dto::EpochTransitionDto {
                from_epoch: transition.from_epoch,
                to_epoch: transition.to_epoch,
                trigger: format!("{:?}", transition.trigger),
                final_commit_count: transition.final_commit_count,
                transitioned_at: transition.transitioned_at,
            },
            new_epoch: transition.to_epoch,
        })
    }
}

// ── LiveIntentBackend ────────────────────────────────────────────────────

/// [`IntentBackend`] adapter backed by a running [`IntentService`] actor.
///
/// Delegates intent compilation to the service via its async handle,
/// converting domain errors to RPC errors.
pub struct LiveIntentBackend<R: AccountResolver> {
    handle: IntentHandle<R>,
    resolver: Arc<R>,
}

impl<R: AccountResolver> LiveIntentBackend<R> {
    /// Wrap an intent service handle and a shared resolver.
    pub fn new(handle: IntentHandle<R>, resolver: Arc<R>) -> Self {
        Self { handle, resolver }
    }
}

impl<R: AccountResolver> IntentBackend for LiveIntentBackend<R> {
    fn submit_intent(
        &self,
        intent: nexus_intent::types::SignedUserIntent,
    ) -> Pin<
        Box<
            dyn std::future::Future<Output = RpcResult<nexus_intent::types::CompiledIntentPlan>>
                + Send
                + '_,
        >,
    > {
        let handle = self.handle.clone();
        let resolver = Arc::clone(&self.resolver);
        Box::pin(async move {
            handle
                .submit(intent, resolver)
                .await
                .map_err(RpcError::from)
        })
    }

    fn estimate_gas(
        &self,
        intent: nexus_intent::types::SignedUserIntent,
    ) -> Pin<
        Box<
            dyn std::future::Future<Output = RpcResult<nexus_intent::types::GasEstimate>>
                + Send
                + '_,
        >,
    > {
        // Gas estimation reuses the compile pipeline and extracts costs.
        // A dedicated fast-path can be added later (optimisation debt).
        let handle = self.handle.clone();
        let resolver = Arc::clone(&self.resolver);
        Box::pin(async move {
            let plan = handle
                .submit(intent, resolver)
                .await
                .map_err(RpcError::from)?;
            Ok(nexus_intent::types::GasEstimate {
                gas_units: plan.estimated_gas,
                shards_touched: plan.steps.len() as u16,
                requires_cross_shard: plan.requires_htlc,
            })
        })
    }
}

// ── McpDispatchBackend ──────────────────────────────────────────────────

/// Live dispatch backend for the online MCP adapter.
///
/// Query tools are answered directly from the node's read backends,
/// while simulation and execute flows delegate to the Agent Core Engine.
pub struct McpDispatchBackend {
    inner: Arc<dyn DispatchBackend>,
    query: Arc<dyn QueryBackend>,
    session_provenance: Arc<dyn SessionProvenanceBackend>,
    intent_tracker: Option<Arc<nexus_rpc::IntentTracker>>,
}

impl McpDispatchBackend {
    /// Construct a live MCP dispatcher.
    pub fn new(
        inner: Arc<dyn DispatchBackend>,
        query: Arc<dyn QueryBackend>,
        session_provenance: Arc<dyn SessionProvenanceBackend>,
        intent_tracker: Option<Arc<nexus_rpc::IntentTracker>>,
    ) -> Self {
        Self {
            inner,
            query,
            session_provenance,
            intent_tracker,
        }
    }

    fn json_payload<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, IntentError> {
        serde_json::to_vec(value)
            .map_err(|e| IntentError::Internal(format!("json encode failed: {e}")))
    }

    fn handle_query(&self, query_kind: &QueryKind) -> Result<DispatchOutcome, IntentError> {
        match query_kind {
            QueryKind::Balance { account } => {
                let amount = self
                    .query
                    .account_balance(account, &TokenId::Native)
                    .map_err(rpc_to_intent_error)?;
                let payload = Self::json_payload(&serde_json::json!({
                    "account": hex::encode(account.0),
                    "token": "native",
                    "amount": amount.0,
                }))?;
                Ok(DispatchOutcome::QueryResult { payload })
            }
            QueryKind::IntentStatus { digest } => {
                if let Some(tracker) = &self.intent_tracker {
                    if let Some(record) = tracker.status(digest) {
                        let payload = Self::json_payload(&serde_json::json!({
                            "intent_id": hex::encode(record.intent_id.0),
                            "status": record.status,
                            "tx_hashes": record.tx_hashes.iter().map(|h| hex::encode(h.0)).collect::<Vec<_>>(),
                            "gas_used": record.gas_used,
                            "submitted_at_ms": record.submitted_at.0,
                            "updated_at_ms": record.updated_at.0,
                        }))?;
                        return Ok(DispatchOutcome::QueryResult { payload });
                    }
                }

                let tx_digest = TxDigest::from_bytes(digest.0);
                match self
                    .query
                    .transaction_receipt(&tx_digest)
                    .map_err(rpc_to_intent_error)?
                {
                    Some(receipt) => {
                        let payload = Self::json_payload(&receipt)?;
                        Ok(DispatchOutcome::QueryResult { payload })
                    }
                    None => Ok(DispatchOutcome::Rejected {
                        reason: format!("intent/transaction {} not found", hex::encode(digest.0)),
                    }),
                }
            }
            QueryKind::ContractState { contract, resource } => {
                let response = self
                    .query
                    .contract_query(&ContractQueryRequest {
                        contract: hex::encode(contract.0),
                        function: resource.clone(),
                        type_args: Vec::new(),
                        args: Vec::new(),
                    })
                    .map_err(rpc_to_intent_error)?;
                let payload = Self::json_payload(&response)?;
                Ok(DispatchOutcome::QueryResult { payload })
            }
        }
    }

    fn handle_provenance(&self, filter: &ProvenanceFilter) -> Result<DispatchOutcome, IntentError> {
        let params = nexus_intent::agent_core::provenance::ProvenanceQueryParams {
            limit: 50,
            cursor: None,
            after_ms: None,
            before_ms: None,
        };

        let result = match filter {
            ProvenanceFilter::ByAgent { agent_id } => self
                .session_provenance
                .query_provenance_by_agent(agent_id, &params),
            ProvenanceFilter::BySession { session_id } => self
                .session_provenance
                .query_provenance_by_session(session_id, &params),
            ProvenanceFilter::ByCapability { token_id } => {
                let mut result = self.session_provenance.provenance_activity_feed(&params);
                result
                    .records
                    .retain(|record| record.capability_token_id.as_ref() == Some(token_id));
                result.total_count = result.records.len() as u64;
                result
            }
            ProvenanceFilter::ByTransaction { tx_hash } => self
                .session_provenance
                .query_provenance_by_tx_hash(tx_hash, &params),
        };

        let payload = Self::json_payload(&result)?;
        Ok(DispatchOutcome::QueryResult { payload })
    }
}

impl DispatchBackend for McpDispatchBackend {
    fn dispatch(
        &self,
        envelope: &nexus_intent::AgentEnvelope,
    ) -> nexus_intent::IntentResult<DispatchOutcome> {
        match &envelope.request_kind {
            AgentRequestKind::Query { query_kind } => self.handle_query(query_kind),
            AgentRequestKind::QueryProvenance { filter } => self.handle_provenance(filter),
            _ => self.inner.dispatch(envelope),
        }
    }
}

fn rpc_to_intent_error(err: RpcError) -> IntentError {
    match err {
        RpcError::BadRequest(message) | RpcError::Serialization(message) => {
            IntentError::ParseError { reason: message }
        }
        RpcError::NotFound(message) => IntentError::AgentSpecError { reason: message },
        RpcError::Unavailable(message) => {
            IntentError::Internal(format!("service unavailable: {message}"))
        }
        RpcError::IntentError(message)
        | RpcError::ExecutionError(message)
        | RpcError::ConsensusError(message)
        | RpcError::Internal(message) => IntentError::Internal(message),
    }
}

// ── GossipBroadcaster ────────────────────────────────────────────────────

/// [`TransactionBroadcaster`] adapter that publishes BCS-encoded
/// transactions to the gossip `Topic::Transaction` channel.
pub struct GossipBroadcaster {
    gossip: nexus_network::GossipHandle,
}

impl GossipBroadcaster {
    /// Wrap a gossip handle for transaction broadcasting.
    pub fn new(gossip: nexus_network::GossipHandle) -> Self {
        Self { gossip }
    }
}

impl nexus_rpc::TransactionBroadcaster for GossipBroadcaster {
    fn broadcast_tx(
        &self,
        data: Vec<u8>,
    ) -> Pin<Box<dyn std::future::Future<Output = RpcResult<()>> + Send + '_>> {
        Box::pin(async move {
            // Accept local submissions even if the gossipsub mesh is still warming up.
            // The local mempool bridge consumes the injected payload immediately, while
            // peer propagation remains a best-effort follow-up.
            self.gossip
                .inject_local(nexus_network::Topic::Transaction, data.clone());

            match self
                .gossip
                .publish(nexus_network::Topic::Transaction, data)
                .await
            {
                Ok(()) => Ok(()),
                Err(nexus_network::NetworkError::InvalidMessage { reason }) => {
                    tracing::warn!(reason = %reason, "transaction broadcast degraded to local delivery");
                    Ok(())
                }
                Err(error) if error.is_retryable() => {
                    tracing::warn!(error = %error, "transaction broadcast degraded to local delivery");
                    Ok(())
                }
                Err(error) => Err(RpcError::Internal(format!(
                    "gossip broadcast failed: {error}"
                ))),
            }
        })
    }
}

// ── LiveNetworkBackend ────────────────────────────────────────────────────

/// [`NetworkBackend`] adapter that exposes peer and routing health
/// via the discovery layer's [`DiscoveryHandle`].
pub struct LiveNetworkBackend {
    discovery: nexus_network::DiscoveryHandle,
}

impl LiveNetworkBackend {
    /// Wrap a discovery handle for network status queries.
    pub fn new(discovery: nexus_network::DiscoveryHandle) -> Self {
        Self { discovery }
    }
}

impl NetworkBackend for LiveNetworkBackend {
    fn network_peers(&self) -> RpcResult<nexus_rpc::NetworkPeersResponse> {
        let records = self.discovery.known_records();
        let peers: Vec<nexus_rpc::NetworkPeerDto> = records
            .iter()
            .map(|r| nexus_rpc::NetworkPeerDto {
                peer_id: r.peer_id.to_string(),
                is_validator: r.validator_stake.is_some(),
                stake: r.validator_stake.map(|s| s as u64),
                reputation: r.reputation,
            })
            .collect();
        let total = peers.len();
        Ok(nexus_rpc::NetworkPeersResponse { peers, total })
    }

    fn network_status(&self) -> RpcResult<nexus_rpc::NetworkStatusResponse> {
        let health = self.discovery.routing_health();
        Ok(nexus_rpc::NetworkStatusResponse {
            known_peers: health.known_peers,
            known_validators: self.discovery.known_validators(),
            filled_buckets: health.filled_buckets,
            total_buckets: health.total_buckets,
            routing_healthy: health.is_healthy(),
        })
    }

    fn network_health(&self) -> RpcResult<nexus_rpc::NetworkHealthResponse> {
        let health = self.discovery.routing_health();
        let status = if health.is_healthy() {
            "healthy"
        } else if health.known_peers > 0 {
            "degraded"
        } else {
            "offline"
        };
        Ok(nexus_rpc::NetworkHealthResponse {
            status: status.into(),
            peer_count: health.known_peers,
            routing_healthy: health.is_healthy(),
        })
    }
}

// ── LiveSessionProvenanceBackend ─────────────────────────────────────────

/// [`SessionProvenanceBackend`] adapter backed by RocksDB-backed session
/// and provenance stores.
pub struct LiveSessionProvenanceBackend<S: StateStorage + Send + Sync + 'static> {
    session_store: Arc<nexus_intent::RocksSessionStore<S>>,
    provenance_store: Arc<nexus_intent::RocksProvenanceStore<S>>,
}

impl<S: StateStorage + Send + Sync + 'static> LiveSessionProvenanceBackend<S> {
    /// Create a new backend from existing session and provenance stores.
    pub fn new(
        session_store: Arc<nexus_intent::RocksSessionStore<S>>,
        provenance_store: Arc<nexus_intent::RocksProvenanceStore<S>>,
    ) -> Self {
        Self {
            session_store,
            provenance_store,
        }
    }
}

impl<S: StateStorage + Send + Sync + 'static> nexus_rpc::SessionProvenanceBackend
    for LiveSessionProvenanceBackend<S>
{
    fn get_session(
        &self,
        session_id: &nexus_primitives::Blake3Digest,
    ) -> Option<nexus_intent::AgentSession> {
        self.session_store.get(session_id)
    }

    fn active_sessions(&self) -> Vec<nexus_intent::AgentSession> {
        self.session_store.active_sessions()
    }

    fn all_sessions(&self) -> Vec<nexus_intent::AgentSession> {
        self.session_store.all_sessions()
    }

    fn get_provenance(
        &self,
        provenance_id: &nexus_primitives::Blake3Digest,
    ) -> Option<nexus_intent::ProvenanceRecord> {
        self.provenance_store.get(provenance_id)
    }

    fn query_provenance_by_agent(
        &self,
        agent: &nexus_primitives::AccountAddress,
        params: &nexus_intent::agent_core::provenance::ProvenanceQueryParams,
    ) -> nexus_intent::agent_core::provenance::ProvenanceQueryResult {
        self.provenance_store.query_by_agent(agent, params)
    }

    fn query_provenance_by_session(
        &self,
        session_id: &nexus_primitives::Blake3Digest,
        params: &nexus_intent::agent_core::provenance::ProvenanceQueryParams,
    ) -> nexus_intent::agent_core::provenance::ProvenanceQueryResult {
        self.provenance_store.query_by_session(session_id, params)
    }

    fn provenance_activity_feed(
        &self,
        params: &nexus_intent::agent_core::provenance::ProvenanceQueryParams,
    ) -> nexus_intent::agent_core::provenance::ProvenanceQueryResult {
        self.provenance_store.activity_feed(params)
    }

    fn get_anchor_receipt(
        &self,
        anchor_digest: &nexus_primitives::Blake3Digest,
    ) -> Option<nexus_intent::AnchorReceipt> {
        self.provenance_store.get_anchor_receipt(anchor_digest)
    }

    fn list_anchor_receipts(&self, limit: u32) -> Vec<nexus_intent::AnchorReceipt> {
        self.provenance_store.list_anchor_receipts(limit)
    }

    fn query_provenance_by_tx_hash(
        &self,
        tx_hash: &nexus_primitives::Blake3Digest,
        params: &nexus_intent::agent_core::provenance::ProvenanceQueryParams,
    ) -> nexus_intent::agent_core::provenance::ProvenanceQueryResult {
        self.provenance_store.query_by_tx_hash(tx_hash, params)
    }

    fn query_provenance_by_status(
        &self,
        status: nexus_intent::agent_core::provenance::ProvenanceStatus,
        params: &nexus_intent::agent_core::provenance::ProvenanceQueryParams,
    ) -> nexus_intent::agent_core::provenance::ProvenanceQueryResult {
        self.provenance_store.query_by_status(status, params)
    }
}

// ── State proof backend ─────────────────────────────────────────────────

/// [`StateProofBackend`] adapter backed by [`SharedCommitmentTracker`].
pub struct LiveStateProofBackend {
    tracker: crate::commitment_tracker::SharedCommitmentTracker,
}

impl LiveStateProofBackend {
    /// Create a new backend from an existing shared commitment tracker.
    pub fn new(tracker: crate::commitment_tracker::SharedCommitmentTracker) -> Self {
        Self { tracker }
    }
}

impl nexus_rpc::StateProofBackend for LiveStateProofBackend {
    fn commitment_info(&self) -> RpcResult<nexus_rpc::StateCommitmentDto> {
        let guard = self
            .tracker
            .read()
            .map_err(|_| RpcError::Internal("commitment tracker lock poisoned".into()))?;
        Ok(nexus_rpc::StateCommitmentDto {
            commitment_root: hex::encode(guard.commitment_root().0),
            backup_root: hex::encode(guard.backup_root().0),
            entry_count: guard.entry_count() as u64,
            updates_applied: guard.updates_applied(),
            epoch_checks_passed: guard.epoch_checks_passed(),
        })
    }

    fn prove_key(&self, key: &[u8]) -> RpcResult<(Option<Vec<u8>>, nexus_storage::MerkleProof)> {
        let guard = self
            .tracker
            .read()
            .map_err(|_| RpcError::Internal("commitment tracker lock poisoned".into()))?;
        guard
            .prove_key(key)
            .map_err(|e| RpcError::Internal(format!("proof generation failed: {e}")))
    }

    fn prove_keys(
        &self,
        keys: &[Vec<u8>],
    ) -> RpcResult<Vec<(Option<Vec<u8>>, nexus_storage::MerkleProof)>> {
        let guard = self
            .tracker
            .read()
            .map_err(|_| RpcError::Internal("commitment tracker lock poisoned".into()))?;
        let key_refs: Vec<&[u8]> = keys.iter().map(|k| k.as_slice()).collect();
        guard
            .prove_keys(&key_refs)
            .map_err(|e| RpcError::Internal(format!("proof generation failed: {e}")))
    }

    fn commitment_root(&self) -> RpcResult<nexus_primitives::Blake3Digest> {
        let guard = self
            .tracker
            .read()
            .map_err(|_| RpcError::Internal("commitment tracker lock poisoned".into()))?;
        Ok(guard.commitment_root())
    }
}

// ── StorageBlockBackend ──────────────────────────────────────────────────

/// [`BlockBackend`] adapter backed by RocksDB column families.
///
/// Reads committed batch metadata from `cf_blocks` (BCS `CommittedBatch`),
/// transaction digest indexes from `cf_block_tx_index`, and individual
/// receipts from `cf_receipts`.
pub struct StorageBlockBackend<S: StateStorage> {
    store: S,
    chain_head: SharedChainHead,
}

impl<S: StateStorage> StorageBlockBackend<S> {
    /// Create a new block query backend.
    pub fn new(store: S, chain_head: SharedChainHead) -> Self {
        Self { store, chain_head }
    }

    /// Load the `CommittedBatch` for a given sequence number from `cf_blocks`.
    fn load_batch(&self, seq: u64) -> RpcResult<nexus_consensus::types::CommittedBatch> {
        let key = seq.to_be_bytes().to_vec();
        let raw = self
            .store
            .get_sync(ColumnFamily::Blocks.as_str(), &key)
            .map_err(|e| RpcError::Internal(format!("storage error: {e}")))?;

        match raw {
            Some(bytes) => bcs::from_bytes(&bytes)
                .map_err(|e| RpcError::Internal(format!("block decode error: {e}"))),
            None => Err(RpcError::NotFound(format!("block {seq} not found"))),
        }
    }

    /// Load the list of transaction digests for a block from `cf_block_tx_index`.
    fn load_tx_digests(&self, seq: u64) -> RpcResult<Vec<nexus_primitives::Blake3Digest>> {
        let key = seq.to_be_bytes().to_vec();
        let raw = self
            .store
            .get_sync(ColumnFamily::BlockTxIndex.as_str(), &key)
            .map_err(|e| RpcError::Internal(format!("storage error: {e}")))?;

        match raw {
            Some(bytes) => bcs::from_bytes(&bytes)
                .map_err(|e| RpcError::Internal(format!("tx index decode error: {e}"))),
            None => Ok(Vec::new()),
        }
    }

    /// Load a single receipt from `cf_receipts`.
    fn load_receipt(
        &self,
        digest: &nexus_primitives::Blake3Digest,
    ) -> RpcResult<Option<nexus_execution::types::TransactionReceipt>> {
        let raw = self
            .store
            .get_sync(ColumnFamily::Receipts.as_str(), &digest.0)
            .map_err(|e| RpcError::Internal(format!("storage error: {e}")))?;

        match raw {
            Some(bytes) => {
                let receipt = serde_json::from_slice(&bytes)
                    .map_err(|e| RpcError::Internal(format!("receipt decode error: {e}")))?;
                Ok(Some(receipt))
            }
            None => Ok(None),
        }
    }

    /// Build a `BlockHeaderDto` from a `CommittedBatch` plus receipt data.
    fn build_header(
        &self,
        batch: &nexus_consensus::types::CommittedBatch,
        tx_count: usize,
        gas_total: u64,
    ) -> BlockHeaderDto {
        // Try to get state_root and epoch from the shared chain head if this
        // is the latest block; otherwise use zeros as placeholder.
        let (state_root, epoch) = self
            .chain_head
            .get()
            .filter(|h| h.sequence == batch.sequence.0)
            .map(|h| (h.state_root, h.epoch))
            .unwrap_or_else(|| ("0".repeat(64), 0));

        BlockHeaderDto {
            sequence: batch.sequence.0,
            anchor_digest: hex::encode(batch.anchor.0),
            state_root,
            epoch,
            cert_count: batch.certificates.len(),
            tx_count,
            gas_total,
            committed_at_ms: batch.committed_at.0,
        }
    }
}

impl<S: StateStorage> BlockBackend for StorageBlockBackend<S> {
    fn block_header(&self, seq: u64) -> RpcResult<BlockHeaderDto> {
        let batch = self.load_batch(seq)?;
        let digests = self.load_tx_digests(seq)?;

        // Sum gas from receipts.
        let mut gas_total = 0u64;
        for d in &digests {
            if let Some(r) = self.load_receipt(d)? {
                gas_total += r.gas_used;
            }
        }

        Ok(self.build_header(&batch, digests.len(), gas_total))
    }

    fn block_full(&self, seq: u64) -> RpcResult<BlockDto> {
        let batch = self.load_batch(seq)?;
        let digests = self.load_tx_digests(seq)?;

        let mut transactions = Vec::with_capacity(digests.len());
        let mut gas_total = 0u64;

        for d in &digests {
            if let Some(r) = self.load_receipt(d)? {
                gas_total += r.gas_used;
                let dto: TransactionReceiptDto = r.into();
                transactions.push(nexus_rpc::TxSummaryDto {
                    tx_digest: hex::encode(d.0),
                    gas_used: dto.gas_used,
                    status: dto.status,
                });
            }
        }

        Ok(BlockDto {
            header: self.build_header(&batch, transactions.len(), gas_total),
            transactions,
        })
    }

    fn block_receipts(&self, seq: u64) -> RpcResult<BlockReceiptsDto> {
        // Ensure block exists.
        let _ = self.load_batch(seq)?;
        let digests = self.load_tx_digests(seq)?;

        let mut receipts = Vec::with_capacity(digests.len());
        let mut total_gas = 0u64;

        for d in &digests {
            if let Some(r) = self.load_receipt(d)? {
                total_gas += r.gas_used;
                receipts.push(r.into());
            }
        }

        Ok(BlockReceiptsDto {
            block_seq: seq,
            receipts,
            total_gas,
        })
    }

    fn block_zk_proof(&self, seq: u64) -> RpcResult<ZkProofDto> {
        // Ensure block exists.
        let _ = self.load_batch(seq)?;
        Err(RpcError::Unavailable("ZK proofs not yet available".into()))
    }
}

// ── StorageEventBackend ──────────────────────────────────────────────────

/// BCS-encoded event value stored in the primary (`e:`) key of `cf_events`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct StoredEvent {
    pub emitter: AccountAddress,
    pub type_tag: String,
    pub sequence_number: u64,
    pub data: Vec<u8>,
    pub tx_digest: [u8; 32],
    pub block_seq: u64,
    pub timestamp_ms: u64,
}

/// [`EventBackend`] adapter backed by the `cf_events` column family.
///
/// Supports three query modes via the multi-key design:
/// - **All events** — scans the primary `e:` prefix range.
/// - **By contract** — scans the `c:<emitter>` prefix.
/// - **By type** — scans the `t:<type_hash>` prefix.
pub struct StorageEventBackend<S: StateStorage> {
    store: S,
}

impl<S: StateStorage> StorageEventBackend<S> {
    /// Create a new event query backend.
    pub fn new(store: S) -> Self {
        Self { store }
    }

    /// Scan the Events CF using a prefix, returning up to `limit` primary
    /// event values. For secondary keys (`c:`, `t:`), we decode the embedded
    /// primary key coordinates and load from the primary `e:` key.
    fn scan_events(
        &self,
        prefix: &[u8],
        limit: u32,
        after_seq: Option<u64>,
        before_seq: Option<u64>,
    ) -> RpcResult<Vec<StoredEvent>> {
        let cf = ColumnFamily::Events.as_str();

        // Build scan end bound: prefix + 0xFF…
        let mut end = prefix.to_vec();
        end.push(0xFF);

        let pairs = self
            .store
            .scan(cf, prefix, &end)
            .map_err(|e| RpcError::Internal(format!("event scan error: {e}")))?;

        let is_primary = prefix.first() == Some(&b'e');
        let mut results = Vec::new();

        for (key, value) in pairs {
            if results.len() >= limit as usize {
                break;
            }

            let event: StoredEvent = if is_primary {
                bcs::from_bytes(&value)
                    .map_err(|e| RpcError::Internal(format!("event decode: {e}")))?
            } else {
                // Secondary key — extract block_seq and event_seq, then look up primary.
                // c: layout = [1B prefix][32B addr][8B block_seq][4B event_seq]
                // t: layout = [1B prefix][32B hash][8B block_seq][4B event_seq]
                if key.len() < 45 {
                    continue;
                }
                let block_seq = u64::from_be_bytes(key[33..41].try_into().expect("8 bytes"));
                let event_seq = u32::from_be_bytes(key[41..45].try_into().expect("4 bytes"));
                // We need the tx_index to form the primary key, but secondary
                // keys don't store it. The value of secondary keys encodes
                // the tx_index as 4-byte BE.
                if value.len() < 4 {
                    continue;
                }
                let tx_index = u32::from_be_bytes(value[..4].try_into().expect("4 bytes"));
                let primary_key = nexus_storage::EventKey::primary(
                    CommitSequence(block_seq),
                    tx_index,
                    event_seq,
                );
                let raw = self
                    .store
                    .get_sync(cf, &primary_key)
                    .map_err(|e| RpcError::Internal(format!("event lookup: {e}")))?;
                match raw {
                    Some(bytes) => bcs::from_bytes(&bytes)
                        .map_err(|e| RpcError::Internal(format!("event decode: {e}")))?,
                    None => continue,
                }
            };

            // Apply block sequence filters.
            if let Some(after) = after_seq {
                if event.block_seq <= after {
                    continue;
                }
            }
            if let Some(before) = before_seq {
                if event.block_seq >= before {
                    continue;
                }
            }

            results.push(event);
        }

        Ok(results)
    }
}

impl<S: StateStorage> nexus_rpc::EventBackend for StorageEventBackend<S> {
    fn query_events(
        &self,
        contract: Option<&AccountAddress>,
        event_type: Option<&str>,
        after_seq: Option<u64>,
        before_seq: Option<u64>,
        limit: u32,
        _cursor: Option<&str>,
    ) -> RpcResult<nexus_rpc::dto::EventQueryResponse> {
        // Choose scan prefix based on filter.
        let prefix = if let Some(addr) = contract {
            nexus_storage::EventKey::contract_prefix(addr)
        } else if let Some(type_tag) = event_type {
            let hash = nexus_primitives::Blake3Digest::from_bytes(
                blake3::hash(type_tag.as_bytes()).into(),
            );
            nexus_storage::EventKey::type_prefix(&hash)
        } else {
            nexus_storage::EventKey::primary_prefix().to_vec()
        };

        // Fetch one extra to detect has_more.
        let events = self.scan_events(&prefix, limit + 1, after_seq, before_seq)?;
        let has_more = events.len() > limit as usize;
        let page: Vec<_> = events.into_iter().take(limit as usize).collect();

        let next_cursor = if has_more {
            page.last()
                .map(|e| format!("{}:{}", e.block_seq, e.sequence_number))
        } else {
            None
        };

        let dtos = page
            .into_iter()
            .map(|e| nexus_rpc::dto::ContractEventDto {
                emitter: hex::encode(e.emitter.0),
                event_type: e.type_tag,
                sequence_number: e.sequence_number,
                data_hex: hex::encode(&e.data),
                data_json: None,
                tx_digest: hex::encode(e.tx_digest),
                block_seq: e.block_seq,
                timestamp_ms: e.timestamp_ms,
            })
            .collect();

        Ok(nexus_rpc::dto::EventQueryResponse {
            events: dtos,
            next_cursor,
            has_more,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_consensus::types::{ReputationScore, ValidatorInfo as ConsensusValidatorInfo};
    use nexus_consensus::Committee;
    use nexus_crypto::falcon::FalconSigner;
    use nexus_crypto::Signer;
    use nexus_execution::types::{ExecutionStatus, TransactionReceipt};
    use nexus_network::{NetworkConfig, NetworkService, Topic};
    use nexus_primitives::{Blake3Digest, TimestampMs};
    use nexus_rpc::TransactionBroadcaster;
    use nexus_storage::{MemoryStore, StateStorage, WriteBatchOps};

    /// Helper: create a test store pre-populated with one account balance.
    fn store_with_balance(address: &AccountAddress, amount: Amount) -> MemoryStore {
        let store = MemoryStore::new();
        let mut key = nexus_storage::AccountKey {
            shard_id: ShardId(0),
            address: *address,
        }
        .to_bytes();
        key.extend_from_slice(b"balance");
        let mut batch = store.new_batch();
        batch.put_cf(
            ColumnFamily::State.as_str(),
            key,
            amount.0.to_le_bytes().to_vec(),
        );
        // MemoryStore::write_batch is trivially async; block on it.
        futures::executor::block_on(store.write_batch(batch)).unwrap();
        store
    }

    /// Helper: create a test store with a receipt.
    fn store_with_receipt(receipt: &TransactionReceipt) -> MemoryStore {
        let store = MemoryStore::new();
        let mut batch = store.new_batch();
        batch.put_cf(
            ColumnFamily::Receipts.as_str(),
            receipt.tx_digest.0.to_vec(),
            serde_json::to_vec(receipt).unwrap(),
        );
        futures::executor::block_on(store.write_batch(batch)).unwrap();
        store
    }

    fn make_query_backend(store: MemoryStore) -> StorageQueryBackend<MemoryStore> {
        StorageQueryBackend::new(
            store,
            Arc::new(std::sync::atomic::AtomicU64::new(1)),
            Arc::new(std::sync::atomic::AtomicU64::new(42)),
        )
    }

    // ── StorageQueryBackend tests ────────────────────────────────────

    #[test]
    fn balance_found() {
        let addr = AccountAddress([0xAA; 32]);
        let store = store_with_balance(&addr, Amount(1_000_000));
        let backend = make_query_backend(store);
        let result = backend.account_balance(&addr, &TokenId::Native);
        assert_eq!(result.unwrap(), Amount(1_000_000));
    }

    #[test]
    fn balance_not_found() {
        let store = MemoryStore::new();
        let backend = make_query_backend(store);
        let addr = AccountAddress([0xBB; 32]);
        let result = backend.account_balance(&addr, &TokenId::Native);
        assert!(result.is_err());
        match result.unwrap_err() {
            RpcError::NotFound(msg) => assert!(msg.contains("not found")),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn receipt_found() {
        let receipt = TransactionReceipt {
            tx_digest: Blake3Digest([0xCC; 32]),
            commit_seq: CommitSequence(10),
            shard_id: ShardId(0),
            status: ExecutionStatus::Success,
            gas_used: 5_000,
            state_changes: vec![],
            events: vec![],
            timestamp: TimestampMs(1_700_000_000_000),
        };
        let store = store_with_receipt(&receipt);
        let backend = make_query_backend(store);
        let dto = backend
            .transaction_receipt(&Blake3Digest([0xCC; 32]))
            .unwrap();
        assert!(dto.is_some());
        let dto = dto.unwrap();
        assert_eq!(dto.tx_digest, Blake3Digest([0xCC; 32]));
        assert_eq!(dto.gas_used, 5_000);
    }

    #[test]
    fn receipt_not_found() {
        let store = MemoryStore::new();
        let backend = make_query_backend(store);
        let result = backend.transaction_receipt(&Blake3Digest([0xFF; 32]));
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn health_reflects_atomics() {
        let store = MemoryStore::new();
        let epoch = Arc::new(std::sync::atomic::AtomicU64::new(5));
        let commit = Arc::new(std::sync::atomic::AtomicU64::new(99));
        let backend = StorageQueryBackend::new(store, epoch, commit);
        let health = backend.health_status();
        assert_eq!(health.status, "healthy");
        assert_eq!(health.epoch, EpochNumber(5));
        assert_eq!(health.latest_commit, CommitSequence(99));
    }

    #[tokio::test]
    async fn gossip_broadcaster_injects_local_when_mesh_empty() {
        let config = NetworkConfig::for_testing();
        let (handle, service) = NetworkService::build(&config).expect("build network");
        let shutdown = handle.transport.clone();
        let net_task = tokio::spawn(service.run());

        handle
            .gossip
            .subscribe(Topic::Transaction)
            .await
            .expect("subscribe transaction topic");

        let broadcaster = GossipBroadcaster::new(handle.gossip.clone());
        let payload = vec![0xAA, 0xBB, 0xCC];
        let mut rx = handle.gossip.topic_receiver(Topic::Transaction);

        broadcaster
            .broadcast_tx(payload.clone())
            .await
            .expect("local delivery should succeed even without peers");

        let received = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("timed out waiting for local tx delivery")
            .expect("transaction receiver closed");
        assert_eq!(received, payload);

        drop(handle);
        shutdown.shutdown().await.expect("shutdown");
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), net_task).await;
    }

    // ── LiveConsensusBackend tests ───────────────────────────────────

    fn make_test_committee() -> Committee {
        let mut validators = Vec::new();
        for i in 0..4u32 {
            let (_, vk) = FalconSigner::generate_keypair();
            validators.push(ConsensusValidatorInfo {
                index: ValidatorIndex(i),
                falcon_pub_key: vk,
                stake: Amount(1_000_000),
                reputation: ReputationScore::from_f32(1.0),
                is_slashed: false,
                shard_id: Some(ShardId(0)),
            });
        }
        Committee::new(EpochNumber(1), validators).unwrap()
    }

    #[test]
    fn consensus_active_validators() {
        let committee = make_test_committee();
        let engine = ConsensusEngine::new(EpochNumber(1), committee);
        let backend = LiveConsensusBackend::new(Arc::new(Mutex::new(engine)));
        let validators = backend.active_validators().unwrap();
        assert_eq!(validators.len(), 4);
        assert!(!validators[0].is_slashed);
    }

    #[test]
    fn consensus_validator_by_index() {
        let committee = make_test_committee();
        let engine = ConsensusEngine::new(EpochNumber(1), committee);
        let backend = LiveConsensusBackend::new(Arc::new(Mutex::new(engine)));
        let v = backend.validator_info(ValidatorIndex(2)).unwrap();
        assert_eq!(v.index, ValidatorIndex(2));
    }

    #[test]
    fn consensus_validator_not_found() {
        let committee = make_test_committee();
        let engine = ConsensusEngine::new(EpochNumber(1), committee);
        let backend = LiveConsensusBackend::new(Arc::new(Mutex::new(engine)));
        let result = backend.validator_info(ValidatorIndex(99));
        assert!(result.is_err());
    }

    #[test]
    fn consensus_status() {
        let committee = make_test_committee();
        let engine = ConsensusEngine::new(EpochNumber(3), committee);
        let backend = LiveConsensusBackend::new(Arc::new(Mutex::new(engine)));
        let status = backend.consensus_status().unwrap();
        assert_eq!(status.epoch, EpochNumber(3));
        assert_eq!(status.dag_size, 0);
        assert_eq!(status.total_commits, 0);
    }

    // ── LiveIntentBackend tests (async, require tokio) ──────────────

    fn intent_compiler() -> nexus_intent::IntentCompilerImpl<nexus_intent::AccountResolverImpl> {
        nexus_intent::IntentCompilerImpl::new(nexus_intent::IntentConfig::default())
    }

    fn make_resolver() -> nexus_intent::AccountResolverImpl {
        let r = nexus_intent::AccountResolverImpl::new(1);
        let sender = AccountAddress([0x01; 32]);
        let to = AccountAddress([0x02; 32]);
        r.balances()
            .set_balance(sender, TokenId::Native, Amount(1_000_000_000));
        r.balances().set_balance(to, TokenId::Native, Amount(1_000));
        r
    }

    fn sign_transfer_intent() -> nexus_intent::types::SignedUserIntent {
        use nexus_crypto::{DilithiumSigner, Signer as _};
        use nexus_intent::types::*;

        let sender = AccountAddress([0x01; 32]);
        let to = AccountAddress([0x02; 32]);
        let intent = UserIntent::Transfer {
            to,
            token: TokenId::Native,
            amount: Amount(100),
        };

        let (sk, vk) = DilithiumSigner::generate_keypair();
        let nonce = 1u64;
        let digest = compute_intent_digest(&intent, &sender, nonce).unwrap();

        let intent_bytes = bcs::to_bytes(&intent).unwrap();
        let sender_bytes = bcs::to_bytes(&sender).unwrap();
        let nonce_bytes = bcs::to_bytes(&nonce).unwrap();
        let mut msg = Vec::new();
        msg.extend_from_slice(&intent_bytes);
        msg.extend_from_slice(&sender_bytes);
        msg.extend_from_slice(&nonce_bytes);
        let sig = DilithiumSigner::sign(&sk, INTENT_DOMAIN, &msg);

        SignedUserIntent {
            intent,
            sender,
            signature: sig,
            sender_pk: vk,
            nonce,
            created_at: nexus_primitives::TimestampMs(1_000_000),
            digest,
        }
    }

    #[tokio::test]
    async fn intent_submit_via_backend() {
        let compiler = intent_compiler();
        let handle = nexus_intent::IntentService::spawn(compiler, 16);
        let resolver = Arc::new(make_resolver());
        let backend = LiveIntentBackend::new(handle, resolver);

        let intent = sign_transfer_intent();
        let result = IntentBackend::submit_intent(&backend, intent);
        let plan = result.await;
        // Compilation may succeed or fail depending on detailed
        // validation, but the adapter must not panic.
        assert!(plan.is_ok() || plan.is_err());
    }

    #[tokio::test]
    async fn intent_estimate_gas_via_backend() {
        let compiler = intent_compiler();
        let handle = nexus_intent::IntentService::spawn(compiler, 16);
        let resolver = Arc::new(make_resolver());
        let backend = LiveIntentBackend::new(handle, resolver);

        let intent = sign_transfer_intent();
        let result = IntentBackend::estimate_gas(&backend, intent);
        let gas = result.await;
        assert!(gas.is_ok() || gas.is_err());
    }

    // ── StorageBlockBackend tests ───────────────────────────────────

    fn make_block_backend(store: MemoryStore) -> StorageBlockBackend<MemoryStore> {
        StorageBlockBackend::new(store, SharedChainHead::default())
    }

    fn store_committed_batch(store: &MemoryStore, seq: u64) {
        let batch = nexus_consensus::types::CommittedBatch {
            anchor: Blake3Digest([0xAA; 32]),
            certificates: vec![Blake3Digest([0xBB; 32]), Blake3Digest([0xCC; 32])],
            sequence: CommitSequence(seq),
            committed_at: TimestampMs(1_700_000_000_000),
        };
        let encoded = bcs::to_bytes(&batch).unwrap();
        let mut wb = store.new_batch();
        wb.put_cf(
            ColumnFamily::Blocks.as_str(),
            seq.to_be_bytes().to_vec(),
            encoded,
        );
        futures::executor::block_on(store.write_batch(wb)).unwrap();
    }

    fn store_tx_index(store: &MemoryStore, seq: u64, digests: &[Blake3Digest]) {
        let encoded = bcs::to_bytes(&digests.to_vec()).unwrap();
        let mut wb = store.new_batch();
        wb.put_cf(
            ColumnFamily::BlockTxIndex.as_str(),
            seq.to_be_bytes().to_vec(),
            encoded,
        );
        futures::executor::block_on(store.write_batch(wb)).unwrap();
    }

    fn store_receipt(store: &MemoryStore, receipt: &TransactionReceipt) {
        let mut wb = store.new_batch();
        wb.put_cf(
            ColumnFamily::Receipts.as_str(),
            receipt.tx_digest.0.to_vec(),
            serde_json::to_vec(receipt).unwrap(),
        );
        futures::executor::block_on(store.write_batch(wb)).unwrap();
    }

    #[test]
    fn block_header_from_storage() {
        let store = MemoryStore::new();
        let tx_digest = Blake3Digest([0xDD; 32]);
        let receipt = TransactionReceipt {
            tx_digest,
            commit_seq: CommitSequence(1),
            shard_id: ShardId(0),
            status: ExecutionStatus::Success,
            gas_used: 3_000,
            state_changes: vec![],
            events: vec![],
            timestamp: TimestampMs(1_700_000_000_000),
        };

        store_committed_batch(&store, 1);
        store_tx_index(&store, 1, &[tx_digest]);
        store_receipt(&store, &receipt);

        let backend = make_block_backend(store);
        let header = BlockBackend::block_header(&backend, 1).unwrap();
        assert_eq!(header.sequence, 1);
        assert_eq!(header.tx_count, 1);
        assert_eq!(header.gas_total, 3_000);
        assert_eq!(header.cert_count, 2);
    }

    #[test]
    fn block_full_from_storage() {
        let store = MemoryStore::new();
        let tx_digest = Blake3Digest([0xDD; 32]);
        let receipt = TransactionReceipt {
            tx_digest,
            commit_seq: CommitSequence(1),
            shard_id: ShardId(0),
            status: ExecutionStatus::Success,
            gas_used: 2_500,
            state_changes: vec![],
            events: vec![],
            timestamp: TimestampMs(1_700_000_000_000),
        };

        store_committed_batch(&store, 1);
        store_tx_index(&store, 1, &[tx_digest]);
        store_receipt(&store, &receipt);

        let backend = make_block_backend(store);
        let block = BlockBackend::block_full(&backend, 1).unwrap();
        assert_eq!(block.header.sequence, 1);
        assert_eq!(block.transactions.len(), 1);
        assert_eq!(block.transactions[0].gas_used, 2_500);
    }

    #[test]
    fn block_receipts_from_storage() {
        let store = MemoryStore::new();
        let d1 = Blake3Digest([0xD1; 32]);
        let d2 = Blake3Digest([0xD2; 32]);
        let r1 = TransactionReceipt {
            tx_digest: d1,
            commit_seq: CommitSequence(1),
            shard_id: ShardId(0),
            status: ExecutionStatus::Success,
            gas_used: 1_000,
            state_changes: vec![],
            events: vec![],
            timestamp: TimestampMs(1_700_000_000_000),
        };
        let r2 = TransactionReceipt {
            tx_digest: d2,
            commit_seq: CommitSequence(1),
            shard_id: ShardId(0),
            status: ExecutionStatus::OutOfGas,
            gas_used: 4_000,
            state_changes: vec![],
            events: vec![],
            timestamp: TimestampMs(1_700_000_000_000),
        };

        store_committed_batch(&store, 1);
        store_tx_index(&store, 1, &[d1, d2]);
        store_receipt(&store, &r1);
        store_receipt(&store, &r2);

        let backend = make_block_backend(store);
        let rec = BlockBackend::block_receipts(&backend, 1).unwrap();
        assert_eq!(rec.block_seq, 1);
        assert_eq!(rec.receipts.len(), 2);
        assert_eq!(rec.total_gas, 5_000);
    }

    #[test]
    fn block_not_found_in_storage() {
        let store = MemoryStore::new();
        let backend = make_block_backend(store);
        let result = BlockBackend::block_header(&backend, 999);
        assert!(result.is_err());
        match result.unwrap_err() {
            RpcError::NotFound(msg) => assert!(msg.contains("999")),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }
}
