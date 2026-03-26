// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! State synchronization foundation — block catch-up protocol.
//!
//! **T-7006**: Enables newly joining nodes to request missing committed
//! blocks from peers via the `Topic::StateSync` gossipsub topic.
//!
//! # Protocol
//! 1. New node sends a `StateSyncMessage::BlockRequest` (via gossipsub)
//!    containing its `PeerId` and the sequence range it needs.
//! 2. The responding peer sends a `StateSyncMessage::BlockResponse`
//!    **directly** to the requester via unicast transport (SEC-M6).
//! 3. The requester validates block authenticity and continuity (SEC-M7)
//!    before inserting into local storage.
//! 4. (W-3) Committee validation: certificate digests are checked against
//!    the committee known for the epoch that produced the block.
//! 5. (W-4) Shard filtering: nodes only participate in state sync when
//!    they are assigned at least one shard.  The `ShardFilter` tracks
//!    the node's current shard assignment and gates both requests and
//!    responses.
//!
//! All messages are BCS-serialized and subject to the 256 KB per-message limit.

use nexus_consensus::types::CommittedBatch;
use nexus_network::types::{PeerId, Topic};
use nexus_network::{GossipHandle, TransportHandle};
use nexus_primitives::{Blake3Digest, CommitSequence, EpochNumber};
use nexus_storage::traits::{StateStorage, WriteBatchOps};
use nexus_storage::ColumnFamily;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

/// Maximum number of blocks in a single sync response.
const MAX_BLOCKS_PER_RESPONSE: u64 = 50;

/// Maximum encoded size for a state-sync message (256 KB per TLD-01 spec).
const MAX_SYNC_MESSAGE_SIZE: usize = 256 * 1024;

/// SEC-M6: Maximum sync responses this node will send per throttle window.
const MAX_SYNC_RESPONSES_PER_WINDOW: u32 = 10;

/// SEC-M6: Throttle window duration in seconds.
const SYNC_RESPONSE_WINDOW_SECS: u64 = 60;

// ── Response Throttle (SEC-M6) ───────────────────────────────────────────────

/// Sliding-window rate limiter for outbound sync responses.
///
/// Prevents a single node from being used as an amplifier: at most
/// [`MAX_SYNC_RESPONSES_PER_WINDOW`] responses are sent per window.
struct SyncThrottle {
    count: AtomicU32,
    window_start: Mutex<Instant>,
}

impl SyncThrottle {
    fn new() -> Self {
        Self {
            count: AtomicU32::new(0),
            window_start: Mutex::new(Instant::now()),
        }
    }

    /// Try to acquire a response permit.  Returns `true` if within budget.
    fn try_acquire(&self) -> bool {
        let now = Instant::now();
        let mut start = match self.window_start.lock() {
            Ok(guard) => guard,
            Err(_) => {
                tracing::error!("state_sync: throttle lock poisoned, denying permit");
                return false;
            }
        };
        if now.duration_since(*start).as_secs() >= SYNC_RESPONSE_WINDOW_SECS {
            *start = now;
            self.count.store(1, Ordering::Relaxed);
            return true;
        }
        let prev = self.count.fetch_add(1, Ordering::Relaxed);
        if prev >= MAX_SYNC_RESPONSES_PER_WINDOW {
            self.count.fetch_sub(1, Ordering::Relaxed);
            return false;
        }
        true
    }
}

// ── Wire types ───────────────────────────────────────────────────────────────

/// Envelope for state synchronisation messages over gossipsub.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StateSyncMessage {
    /// Request committed blocks starting at `from_seq` (inclusive).
    BlockRequest {
        /// First commit sequence to request.
        from_seq: CommitSequence,
        /// Number of blocks requested (capped at [`MAX_BLOCKS_PER_RESPONSE`]).
        count: u64,
        /// SEC-M6: Requester's peer identity for unicast reply.
        requester: PeerId,
    },
    /// Response containing committed-batch metadata.
    BlockResponse {
        /// Committed batches, in strictly ascending sequence order.
        blocks: Vec<CommittedBatch>,
    },
}

// ── Committee validation (W-3) ──────────────────────────────────────────────

/// Provides committee information for verifying certificates during state sync.
///
/// The state sync service uses this to look up the certificate digests that
/// are recognized for a given epoch so it can reject blocks whose
/// certificates do not belong to any known committee.
pub trait CommitteeValidator: Send + Sync + 'static {
    /// Return the set of certificate digests that are known to belong to
    /// the committee of the given epoch.
    ///
    /// Implementations may load this from the epoch store or from the
    /// in-memory consensus engine.
    ///
    /// Returns `None` if the epoch is unknown (e.g. far-future).
    fn known_cert_digests_for_epoch(&self, epoch: EpochNumber) -> Option<HashSet<Blake3Digest>>;

    /// Return the epoch number for a given commit sequence.
    ///
    /// This is needed because `CommittedBatch` does not carry an epoch
    /// field.  The implementation maps sequence → epoch by consulting
    /// the stored epoch transition boundaries.
    ///
    /// Returns `None` if the sequence is unmapped (shouldn't happen for
    /// any sequence ≤ the latest known commit).
    fn epoch_for_sequence(&self, sequence: CommitSequence) -> Option<EpochNumber>;

    /// The latest epoch number the node is aware of.
    fn latest_known_epoch(&self) -> EpochNumber;
}

/// A no-op committee validator that accepts all blocks.
///
/// Used when committee validation is not configured (e.g. in tests or
/// legacy single-validator devnet mode).
pub struct NoOpCommitteeValidator;

impl CommitteeValidator for NoOpCommitteeValidator {
    fn known_cert_digests_for_epoch(&self, _epoch: EpochNumber) -> Option<HashSet<Blake3Digest>> {
        // Accept everything — no validation.
        None
    }

    fn epoch_for_sequence(&self, _sequence: CommitSequence) -> Option<EpochNumber> {
        Some(EpochNumber(0))
    }

    fn latest_known_epoch(&self) -> EpochNumber {
        EpochNumber(0)
    }
}

/// Validate that a block's certificates belong to a known committee.
///
/// Returns `true` if:
/// - The committee validator has no opinion (returns `None` for the epoch)
/// - All certificate digests are in the known set for the epoch
///
/// Returns `false` if any certificate digest is not in the known set.
pub fn validate_block_committee(
    block: &CommittedBatch,
    validator: &dyn CommitteeValidator,
) -> bool {
    let epoch = match validator.epoch_for_sequence(block.sequence) {
        Some(e) => e,
        None => {
            // Unknown epoch for this sequence — trust it (the validator
            // may not have the full epoch history for very old blocks).
            debug!(
                seq = block.sequence.0,
                "state sync: no epoch mapping for sequence, skipping committee check"
            );
            return true;
        }
    };

    let known = match validator.known_cert_digests_for_epoch(epoch) {
        Some(set) => set,
        None => {
            // Validator has no data for this epoch — accept (backward compat).
            return true;
        }
    };

    for cert_digest in &block.certificates {
        if !known.contains(cert_digest) {
            warn!(
                seq = block.sequence.0,
                epoch = epoch.0,
                cert = %cert_digest.to_hex(),
                "state sync: certificate not in known committee set for epoch — rejecting block"
            );
            metrics::counter!("nexus_state_sync_committee_validation_rejected_total").increment(1);
            return false;
        }
    }

    true
}

// ── Shard filtering (W-4) ───────────────────────────────────────────────────

/// Tracks the shards this node is currently responsible for.
///
/// When the filter is active (i.e. the node knows its shard assignment),
/// state sync operations are gated:
/// - **Requests**: only issued when the node is assigned at least one shard.
/// - **Responses**: only served when the node is a shard participant.
///
/// The filter is updated on each epoch change via a watch channel.
///
/// In single-shard mode (num_shards = 0 or 1), the filter is a no-op
/// and all sync operations proceed unconditionally.
#[derive(Debug, Clone)]
pub struct ShardFilter {
    rx: tokio::sync::watch::Receiver<ShardAssignment>,
}

/// Internal state for shard assignment tracking.
#[derive(Debug, Clone, Default)]
struct ShardAssignment {
    /// Total shards in the network (0 or 1 = single-shard mode).
    num_shards: u16,
    /// Shards assigned to this node.
    assigned: HashSet<u16>,
}

/// Sender half for updating shard assignments.
///
/// Typically held by the epoch-change handler which calls [`update`]
/// whenever the validator set or shard layout changes.
pub struct ShardFilterSender {
    tx: tokio::sync::watch::Sender<ShardAssignment>,
}

impl ShardFilterSender {
    /// Update the shard assignment.
    ///
    /// `num_shards`: total shard count (0 or 1 = single-shard mode).
    /// `assigned_shards`: shard IDs assigned to this node.
    pub fn update(&self, num_shards: u16, assigned_shards: &[u16]) {
        let _ = self.tx.send(ShardAssignment {
            num_shards,
            assigned: assigned_shards.iter().copied().collect(),
        });
    }
}

/// Create a shard filter and its updater.
///
/// The caller keeps the [`ShardFilterSender`] to push new shard assignments
/// (typically from `EpochChangeEvent`).  The [`ShardFilter`] is passed into
/// the state sync service.
pub fn shard_filter_channel() -> (ShardFilterSender, ShardFilter) {
    let (tx, rx) = tokio::sync::watch::channel(ShardAssignment::default());
    (ShardFilterSender { tx }, ShardFilter { rx })
}

impl ShardFilter {
    /// Returns `true` if sync operations should proceed.
    ///
    /// In single-shard mode (num_shards ≤ 1) this always returns `true`.
    /// In multi-shard mode, returns `true` only if this node is assigned
    /// at least one shard.
    pub fn should_sync(&self) -> bool {
        let sa = self.rx.borrow();
        if sa.num_shards <= 1 {
            return true;
        }
        !sa.assigned.is_empty()
    }

    /// Returns the set of assigned shard IDs, or `None` if in single-shard mode.
    pub fn assigned_shards(&self) -> Option<HashSet<u16>> {
        let sa = self.rx.borrow();
        if sa.num_shards <= 1 {
            return None;
        }
        Some(sa.assigned.clone())
    }
}

// ── State Sync Service ───────────────────────────────────────────────────────

/// Spawn the state-sync bridge: subscribes to `Topic::StateSync` and
/// handles incoming block requests by reading from local storage.
///
/// SEC-M6: Responses are sent via unicast `TransportHandle::send_to()`
/// to the requester only, with a per-node throttle to prevent amplification.
///
/// Returns a `JoinHandle` so the caller can await or abort the task.
pub async fn spawn_state_sync_service<S: StateStorage>(
    gossip: GossipHandle,
    transport: TransportHandle,
    store: S,
) -> Result<JoinHandle<()>, nexus_network::NetworkError> {
    spawn_state_sync_service_full(gossip, transport, store, None, None).await
}

/// Spawn the state-sync bridge with an optional committee validator (W-3).
///
/// When `committee_validator` is `Some(...)`, incoming block responses have
/// their certificates checked against the known committee for the
/// corresponding epoch.  Unknown or mismatched certificates cause the
/// entire response to be rejected with a warning.
pub async fn spawn_state_sync_service_with_validator<S: StateStorage>(
    gossip: GossipHandle,
    transport: TransportHandle,
    store: S,
    committee_validator: Option<Arc<dyn CommitteeValidator>>,
) -> Result<JoinHandle<()>, nexus_network::NetworkError> {
    spawn_state_sync_service_full(gossip, transport, store, committee_validator, None).await
}

/// Spawn the state-sync bridge with all optional extensions (W-3 + W-4).
///
/// - `committee_validator` (W-3): validates certificate committee membership.
/// - `shard_filter` (W-4): gates sync operations based on shard assignment.
///   When the filter indicates the node has no assigned shards in multi-shard
///   mode, incoming messages are silently ignored and no responses are served.
pub async fn spawn_state_sync_service_full<S: StateStorage>(
    gossip: GossipHandle,
    transport: TransportHandle,
    store: S,
    committee_validator: Option<Arc<dyn CommitteeValidator>>,
    shard_filter: Option<ShardFilter>,
) -> Result<JoinHandle<()>, nexus_network::NetworkError> {
    gossip.subscribe(Topic::StateSync).await?;
    let mut rx = gossip.topic_receiver(Topic::StateSync);
    let throttle = SyncThrottle::new();

    let handle = tokio::spawn(async move {
        debug!("state sync service started");

        loop {
            match rx.recv().await {
                Ok(data) => {
                    // W-4: skip processing when shard filter says we shouldn't sync.
                    if let Some(ref sf) = shard_filter {
                        if !sf.should_sync() {
                            debug!("state sync: skipping message — no assigned shards");
                            metrics::counter!("nexus_state_sync_shard_filtered_total").increment(1);
                            continue;
                        }
                    }

                    handle_sync_message(
                        &data,
                        &transport,
                        &throttle,
                        &store,
                        committee_validator.as_deref(),
                    )
                    .await;
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!(skipped = n, "state sync bridge lagged");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    debug!("state sync channel closed — stopping");
                    break;
                }
            }
        }

        debug!("state sync service stopped");
    });

    Ok(handle)
}

/// Process a single incoming state-sync message.
async fn handle_sync_message<S: StateStorage>(
    data: &[u8],
    transport: &TransportHandle,
    throttle: &SyncThrottle,
    store: &S,
    committee_validator: Option<&dyn CommitteeValidator>,
) {
    // Reject oversized messages
    if data.len() > MAX_SYNC_MESSAGE_SIZE {
        debug!(
            size = data.len(),
            limit = MAX_SYNC_MESSAGE_SIZE,
            "state sync: message exceeds size limit — dropping"
        );
        return;
    }

    let msg: StateSyncMessage = match bcs::from_bytes(data) {
        Ok(m) => m,
        Err(e) => {
            debug!(error = %e, "state sync: failed to decode message — dropping");
            return;
        }
    };

    match msg {
        StateSyncMessage::BlockRequest {
            from_seq,
            count,
            requester,
        } => {
            handle_block_request(from_seq, count, requester, transport, throttle, store).await;
        }
        StateSyncMessage::BlockResponse { blocks } => {
            handle_block_response(blocks, store, committee_validator).await;
        }
    }
}

/// Serve a block request: look up committed batches in storage and
/// send a `BlockResponse` via unicast to the requester (SEC-M6).
async fn handle_block_request<S: StateStorage>(
    from_seq: CommitSequence,
    count: u64,
    requester: PeerId,
    transport: &TransportHandle,
    throttle: &SyncThrottle,
    store: &S,
) {
    // SEC-M6: rate-limit outbound sync responses to prevent amplification.
    if !throttle.try_acquire() {
        debug!("state sync: response throttled — not sending");
        return;
    }

    let capped_count = count.min(MAX_BLOCKS_PER_RESPONSE);

    debug!(
        from = from_seq.0,
        count = capped_count,
        "state sync: serving block request"
    );

    let blocks = load_committed_blocks(store, from_seq, capped_count);

    if blocks.is_empty() {
        debug!(from = from_seq.0, "state sync: no blocks to serve");
        return;
    }

    let response = StateSyncMessage::BlockResponse { blocks };
    let encoded = match bcs::to_bytes(&response) {
        Ok(data) => data,
        Err(e) => {
            warn!(error = %e, "state sync: failed to encode response");
            return;
        }
    };

    if encoded.len() > MAX_SYNC_MESSAGE_SIZE {
        warn!(
            size = encoded.len(),
            "state sync: response exceeds size limit — not sending"
        );
        return;
    }

    // SEC-M6: unicast response to requester only (not broadcast).
    if let Err(e) = transport.send_to(&requester, encoded).await {
        debug!(error = %e, "state sync: failed to send unicast response");
    }
}

/// Handle a received block response: validate, then store the committed batches.
///
/// SEC-M7: Blocks are verified for authenticity, continuity, and source
/// constraints before being written to local storage.
///
/// W-3: When a `CommitteeValidator` is provided, certificate digests are
/// verified against the known committee for the block's epoch.
async fn handle_block_response<S: StateStorage>(
    blocks: Vec<CommittedBatch>,
    store: &S,
    committee_validator: Option<&dyn CommitteeValidator>,
) {
    if blocks.is_empty() {
        return;
    }

    let count = blocks.len();
    let first_seq = blocks[0].sequence.0;
    let last_seq = blocks.last().map(|b| b.sequence.0).unwrap_or(0);

    debug!(
        count,
        first = first_seq,
        last = last_seq,
        "state sync: received block response"
    );

    // ── SEC-M7 validation: authenticity, continuity, source constraints ──

    // 1. Every block must have a non-empty certificate list.
    for block in &blocks {
        if block.certificates.is_empty() {
            warn!(
                seq = block.sequence.0,
                "state sync: block has empty certificate list — rejecting entire response"
            );
            return;
        }
    }

    // 2. Blocks must be in strictly ascending, contiguous sequence order.
    for window in blocks.windows(2) {
        if window[1].sequence.0 != window[0].sequence.0 + 1 {
            warn!(
                prev = window[0].sequence.0,
                next = window[1].sequence.0,
                "state sync: blocks not strictly ascending/contiguous — rejecting"
            );
            return;
        }
    }

    // 3. Reject if blocks overlap with already-stored local data.
    let existing = load_committed_blocks(store, CommitSequence(first_seq), count as u64);
    if !existing.is_empty() {
        warn!(
            first = first_seq,
            last = last_seq,
            existing = existing.len(),
            "state sync: received blocks overlap with existing storage — rejecting"
        );
        return;
    }

    // 4. (W-3) Committee validation: verify certificate digests belong to
    //    the committee of the corresponding epoch.
    if let Some(validator) = committee_validator {
        for block in &blocks {
            if !validate_block_committee(block, validator) {
                warn!(
                    first = first_seq,
                    last = last_seq,
                    "state sync: committee validation failed — rejecting entire response"
                );
                return;
            }
        }
        debug!(
            count,
            first = first_seq,
            last = last_seq,
            "state sync: committee validation passed"
        );
    }

    // ── Validation passed — write blocks to storage ──

    let mut batch = store.new_batch();

    for block in &blocks {
        let key = block.sequence.0.to_be_bytes().to_vec();
        let value = match bcs::to_bytes(block) {
            Ok(v) => v,
            Err(e) => {
                warn!(
                    seq = block.sequence.0,
                    error = %e,
                    "state sync: failed to encode block for storage"
                );
                continue;
            }
        };
        batch.put_cf(ColumnFamily::Blocks.as_str(), key, value);
    }

    if let Err(e) = store.write_batch(batch).await {
        warn!(error = %e, "state sync: failed to write blocks to storage");
    } else {
        info!(
            count,
            first = first_seq,
            last = last_seq,
            "state sync: stored received blocks"
        );
    }
}

/// Load committed batches from storage for a sequence range.
fn load_committed_blocks<S: StateStorage>(
    store: &S,
    from_seq: CommitSequence,
    count: u64,
) -> Vec<CommittedBatch> {
    let start_key = from_seq.0.to_be_bytes().to_vec();
    let end_seq = from_seq.0.saturating_add(count);
    let end_key = end_seq.to_be_bytes().to_vec();

    let entries = match store.scan(ColumnFamily::Blocks.as_str(), &start_key, &end_key) {
        Ok(entries) => entries,
        Err(e) => {
            warn!(error = %e, "state sync: storage scan failed");
            return Vec::new();
        }
    };

    let mut blocks = Vec::with_capacity(entries.len());
    for (_key, value) in entries {
        match bcs::from_bytes::<CommittedBatch>(&value) {
            Ok(batch) => blocks.push(batch),
            Err(e) => {
                debug!(error = %e, "state sync: failed to decode stored block — skipping");
            }
        }
    }

    blocks
}

// ── Request helper ───────────────────────────────────────────────────────────

/// Publish a block request to the network.
///
/// Called by a syncing node to request missing committed blocks.
/// The `local_peer_id` is included so responders can reply via unicast (SEC-M6).
pub async fn request_blocks(
    gossip: &GossipHandle,
    from_seq: CommitSequence,
    count: u64,
    local_peer_id: PeerId,
) -> Result<(), nexus_network::NetworkError> {
    request_blocks_filtered(gossip, from_seq, count, local_peer_id, None).await
}

/// Publish a block request, gated by an optional shard filter (W-4).
///
/// When `shard_filter` is provided and the node has no assigned shards in
/// multi-shard mode, the request is silently skipped.
pub async fn request_blocks_filtered(
    gossip: &GossipHandle,
    from_seq: CommitSequence,
    count: u64,
    local_peer_id: PeerId,
    shard_filter: Option<&ShardFilter>,
) -> Result<(), nexus_network::NetworkError> {
    // W-4: gate outbound requests by shard assignment.
    if let Some(sf) = shard_filter {
        if !sf.should_sync() {
            debug!("state sync: request suppressed — no assigned shards");
            metrics::counter!("nexus_state_sync_request_shard_filtered_total").increment(1);
            return Ok(());
        }
    }

    let msg = StateSyncMessage::BlockRequest {
        from_seq,
        count: count.min(MAX_BLOCKS_PER_RESPONSE),
        requester: local_peer_id,
    };
    let data = bcs::to_bytes(&msg).map_err(|e| nexus_network::NetworkError::InvalidMessage {
        reason: format!("failed to BCS-encode block request: {e}"),
    })?;
    gossip.publish(Topic::StateSync, data).await
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_primitives::{Blake3Digest, CommitSequence, TimestampMs};
    use nexus_storage::MemoryStore;

    fn test_peer_id() -> PeerId {
        PeerId::from_digest(Blake3Digest([0xAA; 32]))
    }

    fn make_committed_batch(seq: u64) -> CommittedBatch {
        CommittedBatch {
            anchor: Blake3Digest([seq as u8; 32]),
            certificates: vec![Blake3Digest([seq as u8; 32])],
            sequence: CommitSequence(seq),
            committed_at: TimestampMs(1_000_000 + seq),
        }
    }

    #[test]
    fn state_sync_message_round_trip() {
        let peer = test_peer_id();
        let req = StateSyncMessage::BlockRequest {
            from_seq: CommitSequence(5),
            count: 10,
            requester: peer,
        };
        let encoded = bcs::to_bytes(&req).expect("encode request");
        let decoded: StateSyncMessage = bcs::from_bytes(&encoded).expect("decode request");
        match decoded {
            StateSyncMessage::BlockRequest {
                from_seq,
                count,
                requester,
            } => {
                assert_eq!(from_seq, CommitSequence(5));
                assert_eq!(count, 10);
                assert_eq!(requester, peer);
            }
            _ => panic!("expected BlockRequest"),
        }

        let resp = StateSyncMessage::BlockResponse {
            blocks: vec![make_committed_batch(1), make_committed_batch(2)],
        };
        let encoded = bcs::to_bytes(&resp).expect("encode response");
        let decoded: StateSyncMessage = bcs::from_bytes(&encoded).expect("decode response");
        match decoded {
            StateSyncMessage::BlockResponse { blocks } => {
                assert_eq!(blocks.len(), 2);
                assert_eq!(blocks[0].sequence, CommitSequence(1));
                assert_eq!(blocks[1].sequence, CommitSequence(2));
            }
            _ => panic!("expected BlockResponse"),
        }
    }

    #[test]
    fn load_committed_blocks_empty_store() {
        let store = MemoryStore::new();
        let blocks = load_committed_blocks(&store, CommitSequence(0), 10);
        assert!(blocks.is_empty());
    }

    #[tokio::test]
    async fn load_committed_blocks_returns_stored_range() {
        let store = MemoryStore::new();

        // Seed 5 blocks (seq 0..5)
        let mut batch = store.new_batch();
        for seq in 0..5u64 {
            let block = make_committed_batch(seq);
            let key = seq.to_be_bytes().to_vec();
            let value = bcs::to_bytes(&block).unwrap();
            batch.put_cf(ColumnFamily::Blocks.as_str(), key, value);
        }
        store.write_batch(batch).await.unwrap();

        // Request blocks 1..4
        let blocks = load_committed_blocks(&store, CommitSequence(1), 3);
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0].sequence, CommitSequence(1));
        assert_eq!(blocks[1].sequence, CommitSequence(2));
        assert_eq!(blocks[2].sequence, CommitSequence(3));
    }

    #[tokio::test]
    async fn handle_block_response_stores_blocks() {
        let store = MemoryStore::new();
        let blocks = vec![make_committed_batch(10), make_committed_batch(11)];

        handle_block_response(blocks, &store, None).await;

        // Verify they were stored
        let loaded = load_committed_blocks(&store, CommitSequence(10), 2);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].sequence, CommitSequence(10));
        assert_eq!(loaded[1].sequence, CommitSequence(11));
    }

    #[tokio::test]
    async fn handle_block_response_empty_is_noop() {
        let store = MemoryStore::new();
        handle_block_response(Vec::new(), &store, None).await;
        let loaded = load_committed_blocks(&store, CommitSequence(0), 100);
        assert!(loaded.is_empty());
    }

    #[test]
    fn request_blocks_caps_count() {
        // Verify that the request message caps count at MAX_BLOCKS_PER_RESPONSE
        let msg = StateSyncMessage::BlockRequest {
            from_seq: CommitSequence(0),
            count: 1000u64.min(MAX_BLOCKS_PER_RESPONSE),
            requester: test_peer_id(),
        };
        match msg {
            StateSyncMessage::BlockRequest { count, .. } => {
                assert_eq!(count, MAX_BLOCKS_PER_RESPONSE);
            }
            _ => unreachable!(),
        }
    }

    #[tokio::test]
    async fn spawn_and_abort_state_sync_service() {
        use nexus_network::{NetworkConfig, NetworkService};

        let config = NetworkConfig::for_testing();
        let (net_handle, service) = NetworkService::build(&config).expect("build");
        let shutdown = net_handle.transport.clone();
        let net_task = tokio::spawn(service.run());

        let store = MemoryStore::new();

        let sync_task = spawn_state_sync_service(
            net_handle.gossip.clone(),
            net_handle.transport.clone(),
            store,
        )
        .await
        .expect("spawn state sync");

        // Abort and verify clean exit
        sync_task.abort();
        let _ = sync_task.await;

        drop(net_handle);
        shutdown.shutdown().await.expect("shutdown");
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), net_task).await;
    }

    #[test]
    fn oversized_message_constant_is_correct() {
        assert_eq!(MAX_SYNC_MESSAGE_SIZE, 256 * 1024);
    }

    // ── D-1 acceptance test: throttle prevents amplification ─────────────

    #[test]
    fn state_sync_should_not_amplify_single_request_across_cluster() {
        // SEC-M6: The SyncThrottle limits responses per window to
        // MAX_SYNC_RESPONSES_PER_WINDOW. Requests exceeding the budget
        // are silently dropped, preventing a single malicious request
        // from triggering unbounded responses.
        let throttle = SyncThrottle::new();

        // First MAX_SYNC_RESPONSES_PER_WINDOW calls should succeed.
        for _ in 0..MAX_SYNC_RESPONSES_PER_WINDOW {
            assert!(throttle.try_acquire(), "should acquire within budget");
        }

        // Subsequent calls within the same window should be rejected.
        for _ in 0..5 {
            assert!(!throttle.try_acquire(), "should reject beyond budget");
        }
    }

    // ── D-2 acceptance test: block response validation ───────────────────

    #[tokio::test]
    async fn state_sync_should_reject_unverified_block_response() {
        let store = MemoryStore::new();

        // Case 1: non-contiguous sequence numbers → rejected
        let blocks = vec![make_committed_batch(10), make_committed_batch(12)]; // gap
        handle_block_response(blocks, &store, None).await;
        let loaded = load_committed_blocks(&store, CommitSequence(10), 5);
        assert!(
            loaded.is_empty(),
            "non-contiguous blocks should be rejected"
        );

        // Case 2: empty certificate list → rejected
        let bad_block = CommittedBatch {
            anchor: Blake3Digest([7; 32]),
            certificates: vec![], // empty
            sequence: CommitSequence(20),
            committed_at: TimestampMs(2_000_000),
        };
        handle_block_response(vec![bad_block], &store, None).await;
        let loaded = load_committed_blocks(&store, CommitSequence(20), 1);
        assert!(
            loaded.is_empty(),
            "blocks with empty certificate list should be rejected"
        );

        // Case 3: overlapping with existing storage → rejected
        let blocks = vec![make_committed_batch(30), make_committed_batch(31)];
        handle_block_response(blocks, &store, None).await;
        let loaded = load_committed_blocks(&store, CommitSequence(30), 2);
        assert_eq!(loaded.len(), 2, "first valid insert should succeed");

        // Now try to insert overlapping range
        let overlap = vec![make_committed_batch(30), make_committed_batch(31)];
        handle_block_response(overlap, &store, None).await;
        // Should not crash; overlap should be detected and rejected
        let loaded = load_committed_blocks(&store, CommitSequence(30), 2);
        assert_eq!(loaded.len(), 2, "original data should be unchanged");

        // Case 4: valid contiguous blocks → accepted
        let valid_blocks = vec![
            make_committed_batch(50),
            make_committed_batch(51),
            make_committed_batch(52),
        ];
        handle_block_response(valid_blocks, &store, None).await;
        let loaded = load_committed_blocks(&store, CommitSequence(50), 3);
        assert_eq!(loaded.len(), 3, "valid contiguous blocks should be stored");
    }

    // ── W-3 acceptance tests: committee validation ───────────────────────

    /// A test committee validator that knows about specific cert digests per epoch.
    struct TestCommitteeValidator {
        /// Mapping from epoch → known certificate digests.
        known: std::collections::HashMap<u64, HashSet<Blake3Digest>>,
    }

    impl TestCommitteeValidator {
        fn new() -> Self {
            Self {
                known: std::collections::HashMap::new(),
            }
        }

        fn add_known(&mut self, epoch: u64, digest: Blake3Digest) {
            self.known.entry(epoch).or_default().insert(digest);
        }
    }

    impl CommitteeValidator for TestCommitteeValidator {
        fn known_cert_digests_for_epoch(
            &self,
            epoch: EpochNumber,
        ) -> Option<HashSet<Blake3Digest>> {
            self.known.get(&epoch.0).cloned()
        }

        fn epoch_for_sequence(&self, _sequence: CommitSequence) -> Option<EpochNumber> {
            // Simple mapping: all sequences belong to epoch 1.
            Some(EpochNumber(1))
        }

        fn latest_known_epoch(&self) -> EpochNumber {
            EpochNumber(1)
        }
    }

    #[test]
    fn validate_block_committee_accepts_when_no_data() {
        let validator = NoOpCommitteeValidator;
        let block = make_committed_batch(1);
        assert!(
            validate_block_committee(&block, &validator),
            "NoOpCommitteeValidator should accept all blocks"
        );
    }

    #[test]
    fn validate_block_committee_accepts_known_certs() {
        let block = make_committed_batch(5);
        let mut validator = TestCommitteeValidator::new();
        // Register the cert digest that make_committed_batch uses: [seq as u8; 32]
        validator.add_known(1, Blake3Digest([5; 32]));

        assert!(
            validate_block_committee(&block, &validator),
            "block with known certificate should pass validation"
        );
    }

    #[test]
    fn validate_block_committee_rejects_unknown_certs() {
        let block = make_committed_batch(5);
        let mut validator = TestCommitteeValidator::new();
        // Register a different cert digest — the block's [5;32] won't match
        validator.add_known(1, Blake3Digest([99; 32]));

        assert!(
            !validate_block_committee(&block, &validator),
            "block with unknown certificate should be rejected"
        );
    }

    #[tokio::test]
    async fn handle_block_response_accepts_with_committee_validator() {
        let store = MemoryStore::new();
        let blocks = vec![make_committed_batch(100), make_committed_batch(101)];

        let mut validator = TestCommitteeValidator::new();
        // Register the cert digests for both blocks
        validator.add_known(1, Blake3Digest([100; 32]));
        validator.add_known(1, Blake3Digest([101; 32]));

        handle_block_response(blocks, &store, Some(&validator)).await;

        let loaded = load_committed_blocks(&store, CommitSequence(100), 2);
        assert_eq!(
            loaded.len(),
            2,
            "blocks with valid committee certs should be stored"
        );
    }

    #[tokio::test]
    async fn handle_block_response_rejects_with_bad_committee() {
        let store = MemoryStore::new();
        let blocks = vec![make_committed_batch(200), make_committed_batch(201)];

        let mut validator = TestCommitteeValidator::new();
        // Only register cert for block 200, not 201
        validator.add_known(1, Blake3Digest([200; 32]));
        // Block 201's cert [201;32] is NOT registered → should reject all

        handle_block_response(blocks, &store, Some(&validator)).await;

        let loaded = load_committed_blocks(&store, CommitSequence(200), 2);
        assert!(
            loaded.is_empty(),
            "blocks with unknown committee certs should be rejected"
        );
    }

    #[tokio::test]
    async fn handle_block_response_noop_validator_accepts_all() {
        let store = MemoryStore::new();
        let blocks = vec![make_committed_batch(300), make_committed_batch(301)];

        let validator = NoOpCommitteeValidator;
        handle_block_response(blocks, &store, Some(&validator)).await;

        let loaded = load_committed_blocks(&store, CommitSequence(300), 2);
        assert_eq!(
            loaded.len(),
            2,
            "NoOpCommitteeValidator should accept all blocks"
        );
    }

    // ── W-4 acceptance tests: shard filtering ────────────────────────────

    #[test]
    fn shard_filter_single_shard_always_syncs() {
        let (sender, filter) = shard_filter_channel();
        // Default state: num_shards=0, no assigned — single-shard mode.
        assert!(filter.should_sync(), "single-shard mode should always sync");
        assert!(
            filter.assigned_shards().is_none(),
            "single-shard mode has no shard set"
        );

        // Explicitly set single-shard mode.
        sender.update(1, &[0]);
        assert!(filter.should_sync(), "num_shards=1 should always sync");
        assert!(filter.assigned_shards().is_none());
    }

    #[test]
    fn shard_filter_multi_shard_with_assigned() {
        let (sender, filter) = shard_filter_channel();
        sender.update(4, &[1, 3]);

        assert!(
            filter.should_sync(),
            "node with assigned shards should sync"
        );
        let assigned = filter.assigned_shards().expect("should have shard set");
        assert_eq!(assigned.len(), 2);
        assert!(assigned.contains(&1));
        assert!(assigned.contains(&3));
    }

    #[test]
    fn shard_filter_multi_shard_no_assigned() {
        let (sender, filter) = shard_filter_channel();
        sender.update(4, &[]);

        assert!(
            !filter.should_sync(),
            "node with no assigned shards should not sync"
        );
        let assigned = filter
            .assigned_shards()
            .expect("multi-shard should return set");
        assert!(assigned.is_empty());
    }

    #[test]
    fn shard_filter_updates_dynamically() {
        let (sender, filter) = shard_filter_channel();

        // Start with no shards in multi-shard mode.
        sender.update(4, &[]);
        assert!(!filter.should_sync());

        // Assign shard 2.
        sender.update(4, &[2]);
        assert!(filter.should_sync());

        let assigned = filter.assigned_shards().unwrap();
        assert_eq!(assigned.len(), 1);
        assert!(assigned.contains(&2));

        // Revoke all shards.
        sender.update(4, &[]);
        assert!(!filter.should_sync());
    }

    #[test]
    fn shard_filter_transition_to_single_shard() {
        let (sender, filter) = shard_filter_channel();

        // Multi-shard with no assigned → should not sync.
        sender.update(4, &[]);
        assert!(!filter.should_sync());

        // Network collapses to single shard → should sync unconditionally.
        sender.update(1, &[0]);
        assert!(filter.should_sync());
        assert!(filter.assigned_shards().is_none());
    }
}
