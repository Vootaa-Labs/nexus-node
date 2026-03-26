//! Data Transfer Objects for the external API.
//!
//! DTOs isolate internal domain types from the public HTTP/JSON surface.
//! No private keys, internal state-machines, or raw crypto material is
//! exposed through these types.

use nexus_primitives::{
    AccountAddress, Amount, CommitSequence, EpochNumber, IntentId, ShardId, TimestampMs, TokenId,
    TxDigest, ValidatorIndex,
};
use serde::{Deserialize, Serialize};

// ── Health / Status ─────────────────────────────────────────────────────

/// Response from the `/health` endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    /// Service status: `"healthy"`, `"degraded"`, `"syncing"`,
    /// `"halted"`, or `"bootstrapping"`.
    pub status: &'static str,
    /// Software version.
    pub version: &'static str,
    /// Number of connected peers.
    pub peers: usize,
    /// Current epoch.
    pub epoch: EpochNumber,
    /// Latest commit sequence.
    pub latest_commit: CommitSequence,
    /// Seconds since the node process started.
    pub uptime_seconds: u64,
    /// Per-subsystem health breakdown (empty when unavailable).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subsystems: Vec<SubsystemHealthDto>,
    /// Human-readable root cause when the node is not healthy.
    /// `None` when status is `"healthy"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Health status of a single node subsystem.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubsystemHealthDto {
    /// Subsystem name (e.g. `"storage"`, `"consensus"`).
    pub name: &'static str,
    /// Status string: `"ready"`, `"starting"`, `"degraded"`, `"down"`.
    pub status: &'static str,
    /// Milliseconds since this subsystem last reported progress (0 = never).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub last_progress_ms: u64,
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}

// ── Account DTOs ────────────────────────────────────────────────────────

/// Account balance response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccountBalanceDto {
    /// Account address (hex).
    pub address: AccountAddress,
    /// Per-token balances.
    pub balances: Vec<TokenBalanceDto>,
}

/// Balance for a single token.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenBalanceDto {
    /// Token identifier.
    pub token: TokenId,
    /// Amount held.
    pub amount: Amount,
}

// ── Transaction DTOs ────────────────────────────────────────────────────

/// Transaction receipt returned by the query API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionReceiptDto {
    /// Transaction digest (hex).
    pub tx_digest: TxDigest,
    /// Global commit sequence.
    pub commit_seq: CommitSequence,
    /// Shard that executed the transaction.
    pub shard_id: ShardId,
    /// Execution outcome.
    pub status: ExecutionStatusDto,
    /// Gas consumed.
    pub gas_used: u64,
    /// Execution timestamp (unix millis).
    pub timestamp: TimestampMs,
}

/// Execution outcome (safe: no internal state references).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExecutionStatusDto {
    /// Executed successfully.
    Success,
    /// Move VM abort.
    MoveAbort {
        /// Abort location.
        location: String,
        /// Abort code.
        code: u64,
    },
    /// Ran out of gas.
    OutOfGas,
    /// Invalid signature or digest.
    InvalidSignature,
    /// `sender_pk` does not derive to `body.sender`.
    SenderMismatch,
    /// Sequence number mismatch.
    SequenceNumberMismatch {
        /// Expected nonce.
        expected: u64,
        /// Actual nonce submitted.
        got: u64,
    },
    /// Transaction expired.
    Expired,
    /// Chain ID mismatch.
    ChainIdMismatch,
    /// HTLC lock not found.
    HtlcLockNotFound,
    /// HTLC already claimed.
    HtlcAlreadyClaimed,
    /// HTLC already refunded.
    HtlcAlreadyRefunded,
    /// HTLC preimage mismatch.
    HtlcPreimageMismatch,
    /// HTLC refund too early.
    HtlcRefundTooEarly,
}

impl From<nexus_execution::types::ExecutionStatus> for ExecutionStatusDto {
    fn from(s: nexus_execution::types::ExecutionStatus) -> Self {
        match s {
            nexus_execution::types::ExecutionStatus::Success => Self::Success,
            nexus_execution::types::ExecutionStatus::MoveAbort { location, code } => {
                Self::MoveAbort { location, code }
            }
            nexus_execution::types::ExecutionStatus::OutOfGas => Self::OutOfGas,
            nexus_execution::types::ExecutionStatus::InvalidSignature => Self::InvalidSignature,
            nexus_execution::types::ExecutionStatus::SenderMismatch => Self::SenderMismatch,
            nexus_execution::types::ExecutionStatus::SequenceNumberMismatch { expected, got } => {
                Self::SequenceNumberMismatch { expected, got }
            }
            nexus_execution::types::ExecutionStatus::Expired => Self::Expired,
            nexus_execution::types::ExecutionStatus::ChainIdMismatch => Self::ChainIdMismatch,
            nexus_execution::types::ExecutionStatus::HtlcLockNotFound => Self::HtlcLockNotFound,
            nexus_execution::types::ExecutionStatus::HtlcAlreadyClaimed => Self::HtlcAlreadyClaimed,
            nexus_execution::types::ExecutionStatus::HtlcAlreadyRefunded => {
                Self::HtlcAlreadyRefunded
            }
            nexus_execution::types::ExecutionStatus::HtlcPreimageMismatch => {
                Self::HtlcPreimageMismatch
            }
            nexus_execution::types::ExecutionStatus::HtlcRefundTooEarly => Self::HtlcRefundTooEarly,
        }
    }
}

impl From<nexus_execution::types::TransactionReceipt> for TransactionReceiptDto {
    fn from(r: nexus_execution::types::TransactionReceipt) -> Self {
        Self {
            tx_digest: r.tx_digest,
            commit_seq: r.commit_seq,
            shard_id: r.shard_id,
            status: r.status.into(),
            gas_used: r.gas_used,
            timestamp: r.timestamp,
        }
    }
}

/// Response returned after successfully submitting a transaction for broadcast.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TxSubmitResponse {
    /// Digest of the accepted transaction (hex).
    pub tx_digest: TxDigest,
    /// Whether the transaction was accepted for broadcast.
    pub accepted: bool,
}

// ── Intent DTOs ─────────────────────────────────────────────────────────

/// Intent submission response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntentSubmitResponse {
    /// Unique intent identifier.
    pub intent_id: IntentId,
    /// Number of compiled execution steps.
    pub steps: usize,
    /// Transaction digests for each step (for tracking via `/v2/tx/{hash}`).
    pub tx_hashes: Vec<TxDigest>,
    /// Whether cross-shard HTLC coordination is needed.
    pub requires_htlc: bool,
    /// Estimated gas cost.
    pub estimated_gas: u64,
}

impl From<nexus_intent::types::CompiledIntentPlan> for IntentSubmitResponse {
    fn from(plan: nexus_intent::types::CompiledIntentPlan) -> Self {
        let tx_hashes = plan.steps.iter().map(|s| s.transaction.digest).collect();
        Self {
            intent_id: plan.intent_id,
            steps: plan.steps.len(),
            tx_hashes,
            requires_htlc: plan.requires_htlc,
            estimated_gas: plan.estimated_gas,
        }
    }
}

/// Gas estimation response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GasEstimateDto {
    /// Estimated gas units.
    pub gas_units: u64,
    /// Number of shards touched.
    pub shards_touched: u16,
    /// Whether cross-shard coordination is required.
    pub requires_cross_shard: bool,
}

impl From<nexus_intent::types::GasEstimate> for GasEstimateDto {
    fn from(e: nexus_intent::types::GasEstimate) -> Self {
        Self {
            gas_units: e.gas_units,
            shards_touched: e.shards_touched,
            requires_cross_shard: e.requires_cross_shard,
        }
    }
}

/// Intent status query response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IntentStatusDto {
    /// Intent identifier.
    pub intent_id: IntentId,
    /// Current status.
    pub status: nexus_intent::types::IntentStatus,
}

// ── Validator DTOs ──────────────────────────────────────────────────────

/// Public validator information (no private keys or internal state).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatorInfoDto {
    /// Committee index.
    pub index: ValidatorIndex,
    /// Public key hex (FALCON-512).
    pub public_key_hex: String,
    /// Staked amount.
    pub stake: Amount,
    /// Reputation score (0.0 – 1.0, serialised as u16 fixed-point).
    pub reputation: u16,
    /// Whether the validator has been slashed.
    pub is_slashed: bool,
    /// Assigned shard (if any).
    pub shard_id: Option<ShardId>,
}

impl From<nexus_consensus::types::ValidatorInfo> for ValidatorInfoDto {
    fn from(v: nexus_consensus::types::ValidatorInfo) -> Self {
        Self {
            index: v.index,
            public_key_hex: hex::encode(v.falcon_pub_key.as_bytes()),
            stake: v.stake,
            reputation: (v.reputation.as_f32() * 10_000.0) as u16,
            is_slashed: v.is_slashed,
            shard_id: v.shard_id,
        }
    }
}

// ── Consensus status ────────────────────────────────────────────────────

/// Node-level consensus status for the `/health` or admin endpoints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsensusStatusDto {
    /// Current epoch.
    pub epoch: EpochNumber,
    /// DAG size (number of certificates).
    pub dag_size: usize,
    /// Total commits since genesis.
    pub total_commits: u64,
    /// Pending commits waiting for execution.
    pub pending_commits: usize,
}

// ── Epoch DTOs ──────────────────────────────────────────────────────────

/// Epoch information response from `/v2/consensus/epoch`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpochInfoDto {
    /// Current epoch number.
    pub epoch: EpochNumber,
    /// Timestamp when this epoch started (ms since Unix epoch).
    pub epoch_started_at: TimestampMs,
    /// Number of validators in the current committee.
    pub committee_size: usize,
    /// Total commits in the current epoch so far.
    pub epoch_commits: u64,
    /// Configured maximum commits per epoch (0 = disabled).
    pub epoch_length_commits: u64,
    /// Configured maximum seconds per epoch (0 = disabled).
    pub epoch_length_seconds: u64,
}

/// Summary of a single epoch transition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpochTransitionDto {
    /// Epoch that ended.
    pub from_epoch: EpochNumber,
    /// Epoch that started.
    pub to_epoch: EpochNumber,
    /// What triggered the transition.
    pub trigger: String,
    /// Total commits in the ending epoch.
    pub final_commit_count: u64,
    /// When the transition occurred (ms since Unix epoch).
    pub transitioned_at: TimestampMs,
}

/// Response from `GET /v2/admin/epoch/history`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpochHistoryResponse {
    /// Epoch transitions in chronological order.
    pub transitions: Vec<EpochTransitionDto>,
    /// Total number of transitions since genesis.
    pub total: usize,
}

// ── Governance / admin DTOs ─────────────────────────────────────────────

/// Request body for `POST /v2/admin/epoch/advance`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpochAdvanceRequest {
    /// Reason for the manual advance (audit trail).
    pub reason: String,
}

/// Response from `POST /v2/admin/epoch/advance`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpochAdvanceResponse {
    /// The transition that was executed.
    pub transition: EpochTransitionDto,
    /// New epoch number after the advance.
    pub new_epoch: EpochNumber,
}

/// Request body for `POST /v2/admin/validator/slash`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlashValidatorRequest {
    /// Validator committee index to slash.
    pub validator_index: u32,
    /// Reason for slashing (audit trail).
    pub reason: String,
}

/// Response from `POST /v2/admin/validator/slash`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlashValidatorResponse {
    /// Validator that was slashed.
    pub validator_index: u32,
    /// Whether the slash was applied (false if already slashed).
    pub applied: bool,
    /// Current epoch at the time of slashing.
    pub epoch: EpochNumber,
}

// ── Election / Staking DTOs ─────────────────────────────────────────────

/// A single elected validator in an election result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElectedValidatorDto {
    /// Hex-encoded account address (32 bytes).
    pub address_hex: String,
    /// Effective stake at election time.
    pub effective_stake: u64,
    /// Committee position assigned by the election.
    pub committee_index: u32,
}

/// Election result summary returned by query endpoints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElectionResultDto {
    /// Epoch for which this committee was elected.
    pub for_epoch: EpochNumber,
    /// Epoch of the staking snapshot used as election input.
    pub snapshot_epoch: EpochNumber,
    /// Elected validators (ordered by committee index).
    pub elected: Vec<ElectedValidatorDto>,
    /// Total effective stake of all elected validators.
    pub total_effective_stake: u64,
    /// Whether this is a fallback (carry-forward) result.
    pub is_fallback: bool,
}

/// Current committee rotation policy configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RotationPolicyDto {
    /// Re-election happens every N epochs (0 = every epoch).
    pub election_epoch_interval: u64,
    /// Maximum committee size (0 = unlimited).
    pub max_committee_size: usize,
    /// Minimum committee size.
    pub min_committee_size: usize,
    /// Minimum total effective stake required for a valid election.
    pub min_total_effective_stake: u64,
    /// Whether slashed validators are excluded from election.
    pub exclude_slashed: bool,
    /// Minimum reputation score to be eligible.
    pub min_reputation_score: u16,
}

/// A single validator's staking state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StakingValidatorDto {
    /// Hex-encoded account address.
    pub address_hex: String,
    /// Total bonded stake.
    pub bonded: u64,
    /// Cumulative penalties.
    pub penalty_total: u64,
    /// Effective stake (bonded - penalty).
    pub effective_stake: u64,
    /// Status (0=Active, 1=Unbonding, 2=Withdrawn).
    pub status: u8,
    /// Whether slashed in consensus.
    pub is_slashed: bool,
    /// Reputation score (0–10000).
    pub reputation: u16,
}

/// Response from `GET /v2/staking/validators`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StakingValidatorsResponse {
    /// Current staking snapshot epoch.
    pub snapshot_epoch: EpochNumber,
    /// Staking validator records.
    pub validators: Vec<StakingValidatorDto>,
    /// Total active validators.
    pub active_count: usize,
    /// Total effective stake across all active validators.
    pub total_effective_stake: u64,
}

// ── Network DTOs ────────────────────────────────────────────────────────

/// Peer information returned by `/v2/network/peers`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkPeerDto {
    /// Hex-encoded Nexus PeerId (32 bytes).
    pub peer_id: String,
    /// Whether this peer is a known validator.
    pub is_validator: bool,
    /// Validator stake (if known).
    pub stake: Option<u64>,
    /// Local reputation score.
    pub reputation: u32,
}

/// Network peers listing response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkPeersResponse {
    /// Known peers.
    pub peers: Vec<NetworkPeerDto>,
    /// Total number of known peers.
    pub total: usize,
}

/// Network status response from `/v2/network/status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkStatusResponse {
    /// Number of known peers in the routing table.
    pub known_peers: usize,
    /// Number of known validators.
    pub known_validators: usize,
    /// Number of filled Kademlia buckets.
    pub filled_buckets: usize,
    /// Total Kademlia buckets (256).
    pub total_buckets: usize,
    /// Whether the routing table is healthy.
    pub routing_healthy: bool,
}

/// Network health response from `/v2/network/health`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkHealthResponse {
    /// Overall P2P layer status: `"healthy"`, `"degraded"`, or `"offline"`.
    pub status: String,
    /// Number of known peers.
    pub peer_count: usize,
    /// Whether DHT routing is healthy.
    pub routing_healthy: bool,
}

// ── Chain head ──────────────────────────────────────────────────────────

/// Response from `GET /v2/chain/head` — latest committed block summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainHeadDto {
    /// Latest commit sequence number.
    pub sequence: u64,
    /// Hex-encoded anchor certificate digest.
    pub anchor_digest: String,
    /// Hex-encoded post-execution state root.
    pub state_root: String,
    /// Current epoch.
    pub epoch: u64,
    /// Current round.
    pub round: u64,
    /// Number of certificates in this commit.
    pub cert_count: usize,
    /// Number of transactions executed.
    pub tx_count: usize,
    /// Total gas consumed.
    pub gas_total: u64,
    /// Wall-clock commit timestamp (ms since Unix epoch).
    pub committed_at_ms: u64,
}

// ── Contract query DTOs ─────────────────────────────────────────────────

/// Request body for `POST /v2/contract/query`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractQueryRequest {
    /// Contract address (hex, 64 chars).
    pub contract: String,
    /// View function name (e.g. "counter::get_count").
    pub function: String,
    /// Type arguments (hex-encoded BCS tags).
    #[serde(default)]
    pub type_args: Vec<String>,
    /// BCS-encoded arguments (hex strings).
    #[serde(default)]
    pub args: Vec<String>,
}

/// Response from `POST /v2/contract/query`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractQueryResponse {
    /// Hex-encoded return value (if any).
    pub return_value: Option<String>,
    /// Gas consumed by the view function.
    pub gas_used: u64,
    /// Gas budget that was applied (0 = unbounded).
    pub gas_budget: u64,
}

// ── Session DTOs ────────────────────────────────────────────────────────

/// Response from `GET /v2/sessions/:id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionDto {
    /// Session ID (hex).
    pub session_id: String,
    /// Current lifecycle state.
    pub state: String,
    /// Creation timestamp (ms since epoch).
    pub created_at_ms: u64,
    /// Plan hash (hex, if bound).
    pub plan_hash: Option<String>,
    /// Confirmation reference (hex, if present).
    pub confirmation_ref: Option<String>,
}

/// Response from `GET /v2/sessions?active=true`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionListResponse {
    /// Sessions matching the query.
    pub sessions: Vec<SessionDto>,
    /// Total count.
    pub total: usize,
}

// ── Provenance DTOs ─────────────────────────────────────────────────────

/// Response from `GET /v2/provenance/:digest`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvenanceRecordDto {
    /// Provenance record ID (hex).
    pub provenance_id: String,
    /// Session ID (hex).
    pub session_id: String,
    /// Agent address (hex).
    pub agent_id: String,
    /// Parent agent address (hex, if delegated).
    pub parent_agent_id: Option<String>,
    /// Intent hash (hex).
    pub intent_hash: String,
    /// Plan hash (hex).
    pub plan_hash: String,
    /// Transaction hash (hex, if executed).
    pub tx_hash: Option<String>,
    /// Status (Pending/Committed/Failed/Aborted/Expired).
    pub status: String,
    /// Creation timestamp (ms).
    pub created_at_ms: u64,
}

/// Paginated query response for provenance records.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvenanceQueryResponse {
    /// Records matching the query.
    pub records: Vec<ProvenanceRecordDto>,
    /// Total matching count.
    pub total_count: u64,
    /// Cursor for next page (hex, if more results).
    pub cursor: Option<String>,
}

// ── State Proof DTOs ────────────────────────────────────────────────────

/// Request body for `POST /v2/state/proof`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateProofRequest {
    /// Hex-encoded storage key to prove.
    pub key: String,
}

/// Request body for `POST /v2/state/proofs` (batch).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchStateProofRequest {
    /// Hex-encoded storage keys to prove.
    pub keys: Vec<String>,
}

/// Response from `POST /v2/state/proof`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateProofResponse {
    /// Hex-encoded commitment root at the time of proof.
    pub commitment_root: String,
    /// Hex-encoded value (if the key exists).
    pub value: Option<String>,
    /// Merkle proof details.
    pub proof: MerkleProofDto,
}

/// Response from `POST /v2/state/proofs` (batch).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchStateProofResponse {
    /// Hex-encoded commitment root at the time of proof.
    pub commitment_root: String,
    /// Individual proofs.
    pub proofs: Vec<SingleProofDto>,
}

/// A single proof entry in a batch response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SingleProofDto {
    /// Hex-encoded key that was queried.
    pub key: String,
    /// Hex-encoded value (if the key exists).
    pub value: Option<String>,
    /// Merkle proof details.
    pub proof: MerkleProofDto,
}

/// Merkle proof details for RPC responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MerkleProofDto {
    /// Proof semantic kind.
    pub proof_type: String,
    /// Total number of leaves.
    pub leaf_count: u64,
    /// Index of the leaf for inclusion proofs.
    pub leaf_index: Option<u64>,
    /// Hex-encoded sibling hashes for inclusion proofs (bottom-up).
    pub siblings: Vec<String>,
    /// Immediate predecessor witness for exclusion proofs.
    pub left_neighbor: Option<MerkleNeighborProofDto>,
    /// Immediate successor witness for exclusion proofs.
    pub right_neighbor: Option<MerkleNeighborProofDto>,
}

/// Neighbour witness details for exclusion proofs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MerkleNeighborProofDto {
    /// Hex-encoded witnessed key.
    pub key: String,
    /// Hex-encoded witnessed value.
    pub value: String,
    /// Index of the witnessed leaf.
    pub leaf_index: u64,
    /// Hex-encoded sibling hashes (bottom-up).
    pub siblings: Vec<String>,
}

/// Response from `GET /v2/state/commitment`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateCommitmentDto {
    /// Hex-encoded primary commitment root.
    pub commitment_root: String,
    /// Hex-encoded backup tree root.
    pub backup_root: String,
    /// Number of entries in the commitment tree.
    pub entry_count: u64,
    /// Total state changes applied.
    pub updates_applied: u64,
    /// Epoch boundary checks passed.
    pub epoch_checks_passed: u64,
}

// ── Anchor receipt DTOs ─────────────────────────────────────────────────

/// Response from `GET /v2/provenance/anchor/:digest`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnchorReceiptDto {
    /// Batch sequence number.
    pub batch_seq: u64,
    /// Anchor digest (hex).
    pub anchor_digest: String,
    /// Transaction hash of the anchoring transaction (hex).
    pub tx_hash: String,
    /// Block height at which the anchor was included.
    pub block_height: u64,
    /// Timestamp of anchor inclusion (ms).
    pub anchored_at_ms: u64,
}

/// Response from `GET /v2/provenance/anchors`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnchorReceiptListResponse {
    /// Anchor receipts (in batch-sequence order).
    pub receipts: Vec<AnchorReceiptDto>,
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_execution::types::{ExecutionStatus, TransactionReceipt};
    use nexus_intent::types::{CompiledIntentPlan, GasEstimate};
    use nexus_primitives::Blake3Digest;

    #[test]
    fn execution_status_dto_roundtrip() {
        let success = ExecutionStatus::Success;
        let dto: ExecutionStatusDto = success.into();
        assert_eq!(dto, ExecutionStatusDto::Success);

        let abort = ExecutionStatus::MoveAbort {
            location: "0x1::coin::transfer".into(),
            code: 42,
        };
        let dto: ExecutionStatusDto = abort.into();
        assert_eq!(
            dto,
            ExecutionStatusDto::MoveAbort {
                location: "0x1::coin::transfer".into(),
                code: 42,
            }
        );

        let oog = ExecutionStatus::OutOfGas;
        let dto: ExecutionStatusDto = oog.into();
        assert_eq!(dto, ExecutionStatusDto::OutOfGas);
    }

    #[test]
    fn transaction_receipt_dto_conversion() {
        let receipt = TransactionReceipt {
            tx_digest: Blake3Digest([0xAA; 32]),
            commit_seq: CommitSequence(100),
            shard_id: ShardId(1),
            status: ExecutionStatus::Success,
            gas_used: 5_000,
            state_changes: vec![],
            timestamp: TimestampMs(1_700_000_000_000),
        };
        let dto: TransactionReceiptDto = receipt.into();
        assert_eq!(dto.tx_digest, Blake3Digest([0xAA; 32]));
        assert_eq!(dto.commit_seq, CommitSequence(100));
        assert_eq!(dto.gas_used, 5_000);
        assert_eq!(dto.status, ExecutionStatusDto::Success);
    }

    #[test]
    fn intent_submit_response_from_plan() {
        let plan = CompiledIntentPlan {
            intent_id: Blake3Digest([0xBB; 32]),
            steps: vec![],
            requires_htlc: false,
            estimated_gas: 10_000,
            expires_at: EpochNumber(50),
        };
        let dto: IntentSubmitResponse = plan.into();
        assert_eq!(dto.intent_id, Blake3Digest([0xBB; 32]));
        assert_eq!(dto.steps, 0);
        assert!(!dto.requires_htlc);
        assert_eq!(dto.estimated_gas, 10_000);
    }

    #[test]
    fn gas_estimate_dto_conversion() {
        let estimate = GasEstimate {
            gas_units: 25_000,
            shards_touched: 2,
            requires_cross_shard: true,
        };
        let dto: GasEstimateDto = estimate.into();
        assert_eq!(dto.gas_units, 25_000);
        assert_eq!(dto.shards_touched, 2);
        assert!(dto.requires_cross_shard);
    }

    #[test]
    fn health_response_serializes() {
        let resp = HealthResponse {
            status: "healthy",
            version: "0.1.3",
            peers: 42,
            epoch: EpochNumber(10),
            latest_commit: CommitSequence(999),
            uptime_seconds: 120,
            subsystems: Vec::new(),
            reason: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("healthy"));
        assert!(json.contains("42"));
        // Empty subsystems should be omitted.
        assert!(!json.contains("subsystems"));
    }

    #[test]
    fn health_response_with_subsystems_serializes() {
        let resp = HealthResponse {
            status: "degraded",
            version: "0.1.4",
            peers: 3,
            epoch: EpochNumber(1),
            latest_commit: CommitSequence(10),
            uptime_seconds: 60,
            subsystems: vec![
                SubsystemHealthDto {
                    name: "storage",
                    status: "ready",
                    last_progress_ms: 0,
                },
                SubsystemHealthDto {
                    name: "network",
                    status: "degraded",
                    last_progress_ms: 0,
                },
            ],
            reason: Some("network=degraded".into()),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("degraded"));
        assert!(json.contains("subsystems"));
        assert!(json.contains("storage"));
    }

    #[test]
    fn account_balance_dto_serializes() {
        let dto = AccountBalanceDto {
            address: AccountAddress([0xCC; 32]),
            balances: vec![TokenBalanceDto {
                token: TokenId::Native,
                amount: Amount(1_000_000),
            }],
        };
        let json = serde_json::to_string(&dto).unwrap();
        let parsed: AccountBalanceDto = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, dto);
    }

    #[test]
    fn consensus_status_dto_serializes() {
        let dto = ConsensusStatusDto {
            epoch: EpochNumber(5),
            dag_size: 1_000,
            total_commits: 500,
            pending_commits: 3,
        };
        let json = serde_json::to_string(&dto).unwrap();
        assert!(json.contains("\"dag_size\":1000"));
    }
}

// ── Shard topology DTOs (W-5) ───────────────────────────────────────────

/// Response from `GET /v2/shards` — current shard topology.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ShardTopologyDto {
    /// Total shard count for the network.
    pub num_shards: u16,
    /// Per-shard information.
    pub shards: Vec<ShardInfoDto>,
}

/// Per-shard summary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ShardInfoDto {
    /// Shard identifier.
    pub shard_id: u16,
    /// Validator indices that serve this shard.
    pub validators: Vec<u32>,
}

/// Response from `GET /v2/shards/{shard_id}/head` — per-shard chain head.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ShardChainHeadDto {
    /// Shard identifier.
    pub shard_id: u16,
    /// Latest commit sequence that includes this shard.
    pub sequence: u64,
    /// Hex-encoded anchor digest.
    pub anchor_digest: String,
    /// Current epoch.
    pub epoch: u64,
}

// ── HTLC DTOs (W-5) ────────────────────────────────────────────────────

/// Status of an HTLC lock.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum HtlcStatusDto {
    /// Lock is active — not yet claimed or refunded.
    Pending,
    /// Lock was claimed with preimage.
    Claimed,
    /// Lock was refunded after timeout.
    Refunded,
    /// Lock expired without action.
    Expired,
}

/// Single HTLC lock state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HtlcLockDto {
    /// Hex-encoded lock digest (32 bytes).
    pub lock_digest: String,
    /// Sender account address (hex).
    pub sender: String,
    /// Recipient account address (hex).
    pub recipient: String,
    /// Locked amount (smallest unit).
    pub amount: u64,
    /// Target shard for this lock.
    pub target_shard: u16,
    /// Epoch after which the lock can be refunded.
    pub timeout_epoch: u64,
    /// Current lock status.
    pub status: HtlcStatusDto,
}

/// Response from `GET /v2/htlc/pending` — list of pending HTLC locks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HtlcPendingListDto {
    /// Pending locks.
    pub locks: Vec<HtlcLockDto>,
    /// Total number of pending locks in the system.
    pub total: usize,
}

/// Internal transaction lifecycle snapshot used by benchmark tooling.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TxLifecycleDto {
    /// Digest of the tracked transaction.
    pub tx_digest: TxDigest,
    /// Timestamp when `POST /v2/tx/submit` accepted the transaction.
    pub submit_accepted_at: Option<TimestampMs>,
    /// Timestamp when the local node admitted the transaction into mempool.
    pub mempool_admitted_at: Option<TimestampMs>,
    /// Timestamp when the transaction was included in a committed consensus batch.
    pub consensus_included_at: Option<TimestampMs>,
    /// Timestamp when the local receipt query path first observed the receipt.
    pub receipt_visible_at: Option<TimestampMs>,
}
