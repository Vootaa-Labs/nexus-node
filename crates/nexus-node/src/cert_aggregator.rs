//! Certificate aggregator — collects validator signatures to build
//! quorum certificates for Narwhal batches.
//!
//! Implements the multi-validator signing protocol:
//! 1. Local proposer submits a batch → aggregator broadcasts `BatchProposal`.
//! 2. Remote validators receive proposals, verify, store batch payloads,
//!    sign, and broadcast `BatchVote`.
//! 3. Aggregator collects votes; once stake-weighted quorum is reached,
//!    builds the certificate, inserts it into the consensus engine, and
//!    broadcasts the final certificate to the network.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nexus_consensus::certificate::cert_signing_payload;
use nexus_consensus::types::CERT_DOMAIN;
use nexus_consensus::{CertificateBuilder, ConsensusEngine, ValidatorRegistry};
use nexus_crypto::{Blake3Hasher, DilithiumSigner, FalconSigner, FalconSigningKey, Signer};
use nexus_network::GossipHandle;
use nexus_primitives::{Amount, BatchDigest, CertDigest, EpochNumber, RoundNumber, ValidatorIndex};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::batch_store::BatchStore;
use crate::consensus_bridge::{self, BatchProposal, BatchVote, ConsensusMessage};

/// Maximum age of a pending proposal before it is discarded.
const PROPOSAL_TIMEOUT: Duration = Duration::from_secs(30);

/// Cleanup interval for stale pending proposals.
const CLEANUP_INTERVAL: Duration = Duration::from_secs(10);

/// Maximum encoded size for a consensus message (matches consensus_bridge).
/// Maximum size for a consensus gossip message.  With ML-DSA-65 each
/// signed transaction is ~5.5 KB, so a full 512-tx batch can reach ~2.8 MB.
const MAX_CERT_MESSAGE_SIZE: usize = 4 * 1024 * 1024;

/// Maximum number of pending proposals tracked simultaneously (SEC-M4).
///
/// Prevents unbounded memory growth when proposals arrive faster than they
/// can be finalized. When this limit is reached the oldest pending proposal
/// is evicted before a new one is inserted.
const MAX_PENDING_PROPOSALS: usize = 1024;

// ── Local proposal request ───────────────────────────────────────────────────

/// A proposal from the local batch proposer to the cert aggregator.
pub struct LocalProposal {
    /// Current epoch.
    pub epoch: EpochNumber,
    /// Batch digest (BLAKE3 over the encoded payload).
    pub batch_digest: BatchDigest,
    /// BCS-encoded transaction bytes.
    pub batch_payload: Vec<Vec<u8>>,
    /// Proposing validator index.
    pub origin: ValidatorIndex,
    /// DAG round.
    pub round: RoundNumber,
    /// Parent certificate digests from the previous round.
    pub parents: Vec<CertDigest>,
    /// Total number of active validators.
    pub num_validators: u32,
    /// Stake-weighted quorum threshold.
    pub quorum_threshold: Amount,
}

// ── Pending proposal state ───────────────────────────────────────────────────

#[allow(dead_code)]
struct PendingProposal {
    builder: CertificateBuilder,
    epoch: EpochNumber,
    origin: ValidatorIndex,
    round: RoundNumber,
    parents: Vec<CertDigest>,
    quorum_threshold: Amount,
    created_at: Instant,
}

// ── Public API ───────────────────────────────────────────────────────────────

/// Shared infrastructure for the certificate aggregation loop (D-1 convergence).
///
/// Groups the per-loop references that were previously 6 positional
/// parameters passed to `handle_gossip_message` and `handle_remote_proposal`.
struct CertAggContext<'a> {
    gossip: &'a GossipHandle,
    engine: &'a Mutex<ConsensusEngine>,
    batch_store: &'a BatchStore,
    signing_key: &'a FalconSigningKey,
    local_validator: ValidatorIndex,
    current_epoch: &'a std::sync::atomic::AtomicU64,
}

/// Create a channel for the batch proposer to submit proposals.
pub fn proposal_channel() -> (mpsc::Sender<LocalProposal>, mpsc::Receiver<LocalProposal>) {
    mpsc::channel(256)
}

/// Spawn the certificate aggregator background task.
///
/// The aggregator:
/// - Receives local proposals from the batch proposer via `proposal_rx`.
/// - Listens on `Topic::Consensus` for remote `BatchProposal` / `BatchVote`.
/// - Manages pending proposals, collects votes, and builds certificates.
pub async fn spawn_cert_aggregator(
    gossip: GossipHandle,
    engine: Arc<Mutex<ConsensusEngine>>,
    batch_store: Arc<BatchStore>,
    signing_key: Arc<FalconSigningKey>,
    local_validator: ValidatorIndex,
    current_epoch: Arc<std::sync::atomic::AtomicU64>,
    mut proposal_rx: mpsc::Receiver<LocalProposal>,
) -> Result<JoinHandle<()>, nexus_network::NetworkError> {
    // Subscribe to the Consensus topic (independent from consensus_bridge's subscription).
    gossip
        .subscribe(nexus_network::types::Topic::Consensus)
        .await?;
    let mut gossip_rx = gossip.topic_receiver(nexus_network::types::Topic::Consensus);

    let handle = tokio::spawn(async move {
        let mut pending: HashMap<BatchDigest, PendingProposal> = HashMap::new();
        let mut cleanup_timer = tokio::time::interval(CLEANUP_INTERVAL);
        cleanup_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let agg_ctx = CertAggContext {
            gossip: &gossip,
            engine: &engine,
            batch_store: &batch_store,
            signing_key: &signing_key,
            local_validator,
            current_epoch: &current_epoch,
        };

        debug!(validator = local_validator.0, "cert aggregator started");

        loop {
            tokio::select! {
                // ── Local proposal from batch proposer ───────────────
                Some(lp) = proposal_rx.recv() => {
                    handle_local_proposal(
                        lp,
                        agg_ctx.gossip,
                        agg_ctx.engine,
                        agg_ctx.signing_key,
                        agg_ctx.local_validator,
                        &mut pending,
                    ).await;
                }

                // ── Remote gossip message ────────────────────────────
                result = gossip_rx.recv() => {
                    match result {
                        Ok(data) => {
                            handle_gossip_message(
                                &data,
                                &agg_ctx,
                                &mut pending,
                            ).await;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            warn!(skipped = n, "cert aggregator: gossip lagged");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            debug!("cert aggregator: gossip channel closed — stopping");
                            break;
                        }
                    }
                }

                // ── Periodic cleanup ─────────────────────────────────
                _ = cleanup_timer.tick() => {
                    let before = pending.len();
                    pending.retain(|_, p| p.created_at.elapsed() < PROPOSAL_TIMEOUT);
                    let evicted = before - pending.len();
                    if evicted > 0 {
                        debug!(evicted, remaining = pending.len(), "cert aggregator: cleaned stale proposals");
                    }
                }
            }
        }

        debug!("cert aggregator stopped");
    });

    Ok(handle)
}

// ── Internal handlers ────────────────────────────────────────────────────────

/// Handle a local proposal: broadcast it, self-sign, and track as pending.
async fn handle_local_proposal(
    lp: LocalProposal,
    gossip: &GossipHandle,
    engine: &Mutex<ConsensusEngine>,
    signing_key: &FalconSigningKey,
    local_validator: ValidatorIndex,
    pending: &mut HashMap<BatchDigest, PendingProposal>,
) {
    // 1. Broadcast the proposal to peers
    let proposal = BatchProposal {
        epoch: lp.epoch,
        batch_digest: lp.batch_digest,
        origin: lp.origin,
        round: lp.round,
        parents: lp.parents.clone(),
        batch_payload: lp.batch_payload,
    };

    if let Err(e) = consensus_bridge::publish_batch_proposal(gossip, &proposal).await {
        debug!(error = %e, "cert aggregator: failed to broadcast proposal");
    }

    // 2. Create a CertificateBuilder and self-sign
    let mut builder = CertificateBuilder::new(
        lp.epoch,
        lp.batch_digest,
        lp.origin,
        lp.round,
        lp.parents.clone(),
        lp.num_validators,
    );

    let payload =
        match cert_signing_payload(lp.epoch, &lp.batch_digest, lp.origin, lp.round, &lp.parents) {
            Ok(p) => p,
            Err(e) => {
                warn!(error = %e, "cert aggregator: failed to compute signing payload");
                return;
            }
        };

    let signature = FalconSigner::sign(signing_key, CERT_DOMAIN, &payload);
    builder.add_signature(local_validator, signature);

    // 3. Check if single-validator mode (only one active validator)
    if lp.num_validators <= 1 {
        finalize_certificate(builder, engine, gossip, lp.round).await;
        return;
    }

    // 4. Enforce capacity limit (SEC-M4) before inserting
    enforce_pending_capacity(pending);

    // 5. Store as pending, waiting for remote votes
    debug!(
        batch = %lp.batch_digest,
        round = lp.round.0,
        sigs = builder.signature_count(),
        needed = lp.quorum_threshold.0,
        "cert aggregator: proposal pending, awaiting votes"
    );

    pending.insert(
        lp.batch_digest,
        PendingProposal {
            builder,
            epoch: lp.epoch,
            origin: lp.origin,
            round: lp.round,
            parents: lp.parents,
            quorum_threshold: lp.quorum_threshold,
            created_at: Instant::now(),
        },
    );
}

/// Handle a gossip message (BatchProposal or BatchVote).
async fn handle_gossip_message(
    data: &[u8],
    ctx: &CertAggContext<'_>,
    pending: &mut HashMap<BatchDigest, PendingProposal>,
) {
    if data.len() > MAX_CERT_MESSAGE_SIZE {
        return;
    }

    let msg: ConsensusMessage = match bcs::from_bytes(data) {
        Ok(m) => m,
        Err(_) => return,
    };

    match msg {
        ConsensusMessage::BatchProposal(proposal) => {
            handle_remote_proposal(proposal, ctx, pending).await;
        }
        ConsensusMessage::BatchVote(vote) => {
            handle_remote_vote(vote, ctx.engine, ctx.gossip, pending).await;
        }
        ConsensusMessage::Certificate(_) => {
            // Handled by the consensus_bridge — ignore here.
        }
    }
}

/// Handle a remote BatchProposal: validate, store payload, sign, broadcast vote.
async fn handle_remote_proposal(
    proposal: BatchProposal,
    ctx: &CertAggContext<'_>,
    pending: &mut HashMap<BatchDigest, PendingProposal>,
) {
    let epoch = EpochNumber(ctx.current_epoch.load(std::sync::atomic::Ordering::Relaxed));

    // 1. Epoch guard
    if proposal.epoch != epoch {
        debug!(
            proposal_epoch = proposal.epoch.0,
            current = epoch.0,
            "cert aggregator: epoch mismatch — dropping proposal"
        );
        return;
    }

    // 2. Skip if this is our own proposal (already tracked locally)
    if proposal.origin == ctx.local_validator {
        return;
    }

    // 3. Verify batch digest matches the payload
    let batch_payload_bytes = match bcs::to_bytes(&(
        proposal.origin,
        proposal.round,
        &proposal.batch_payload,
    )) {
        Ok(p) => p,
        Err(e) => {
            debug!(error = %e, "cert aggregator: failed to serialize proposal payload for verification");
            return;
        }
    };
    let computed_digest = Blake3Hasher::digest(b"nexus::narwhal::batch::v1", &batch_payload_bytes);
    if computed_digest != proposal.batch_digest {
        debug!(
            expected = %proposal.batch_digest,
            computed = %computed_digest,
            "cert aggregator: batch digest mismatch — dropping proposal"
        );
        return;
    }

    // 4. Decode and validate every transaction in the batch payload.
    //    If ANY transaction fails to decode, reject the entire proposal
    //    (SEC-H5, SEC-H6).
    let mut transactions: Vec<nexus_execution::types::SignedTransaction> =
        Vec::with_capacity(proposal.batch_payload.len());
    for (idx, tx_bytes) in proposal.batch_payload.iter().enumerate() {
        let tx: nexus_execution::types::SignedTransaction = match bcs::from_bytes(tx_bytes) {
            Ok(t) => t,
            Err(e) => {
                debug!(
                    batch = %proposal.batch_digest,
                    tx_index = idx,
                    error = %e,
                    "cert aggregator: tx decode failed — rejecting entire proposal"
                );
                return;
            }
        };

        // Per-tx signature verification: re-derive digest and check sig.
        let expected_digest = match nexus_execution::types::compute_tx_digest(&tx.body) {
            Ok(d) => d,
            Err(e) => {
                debug!(
                    batch = %proposal.batch_digest,
                    tx_index = idx,
                    error = %e,
                    "cert aggregator: tx digest computation failed — rejecting entire proposal"
                );
                return;
            }
        };
        if expected_digest != tx.digest {
            debug!(
                batch = %proposal.batch_digest,
                tx_index = idx,
                "cert aggregator: tx digest mismatch — rejecting entire proposal"
            );
            return;
        }

        if DilithiumSigner::verify(
            &tx.sender_pk,
            nexus_crypto::domains::USER_TX,
            tx.digest.as_bytes(),
            &tx.signature,
        )
        .is_err()
        {
            debug!(
                batch = %proposal.batch_digest,
                tx_index = idx,
                "cert aggregator: tx signature invalid — rejecting entire proposal"
            );
            return;
        }

        transactions.push(tx);
    }
    ctx.batch_store.insert(proposal.batch_digest, transactions);

    // 5. Sign the proposal header
    let payload = match cert_signing_payload(
        proposal.epoch,
        &proposal.batch_digest,
        proposal.origin,
        proposal.round,
        &proposal.parents,
    ) {
        Ok(p) => p,
        Err(e) => {
            debug!(error = %e, "cert aggregator: failed to compute signing payload for vote");
            return;
        }
    };

    let signature = FalconSigner::sign(ctx.signing_key, CERT_DOMAIN, &payload);

    // 6. Broadcast vote
    let vote = BatchVote {
        epoch: proposal.epoch,
        batch_digest: proposal.batch_digest,
        origin: proposal.origin,
        round: proposal.round,
        voter: ctx.local_validator,
        signature: signature.clone(),
    };

    if let Err(e) = consensus_bridge::publish_batch_vote(ctx.gossip, &vote).await {
        debug!(error = %e, "cert aggregator: failed to broadcast vote");
    }

    debug!(
        batch = %proposal.batch_digest,
        origin = proposal.origin.0,
        round = proposal.round.0,
        "cert aggregator: voted on remote proposal"
    );

    // 7. Also track this proposal (in case we're the one aggregating)
    //    Get num_validators and quorum from engine.
    let (num_validators, quorum_threshold) = {
        let eng = match ctx.engine.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let nv = eng.committee().active_validators().len() as u32;
        let qt = eng.committee().quorum_threshold();
        (nv, qt)
    };

    // Enforce capacity limit (SEC-M4) before inserting a new entry
    enforce_pending_capacity(pending);

    let entry = pending.entry(proposal.batch_digest).or_insert_with(|| {
        let builder = CertificateBuilder::new(
            proposal.epoch,
            proposal.batch_digest,
            proposal.origin,
            proposal.round,
            proposal.parents.clone(),
            num_validators,
        );
        PendingProposal {
            builder,
            epoch: proposal.epoch,
            origin: proposal.origin,
            round: proposal.round,
            parents: proposal.parents.clone(),
            quorum_threshold,
            created_at: Instant::now(),
        }
    });

    // Add our own vote
    entry.builder.add_signature(ctx.local_validator, signature);

    // Check if quorum reached (stake-weighted)
    let quorum_met = {
        let eng = match ctx.engine.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        eng.committee().is_quorum(entry.builder.signers())
    };
    if quorum_met {
        if let Some(pp) = pending.remove(&proposal.batch_digest) {
            finalize_certificate(pp.builder, ctx.engine, ctx.gossip, pp.round).await;
        }
    }
}

/// Enforce the `MAX_PENDING_PROPOSALS` capacity limit (SEC-M4).
///
/// When the pending map is at capacity, evict the oldest entry (by
/// `created_at`) to make room for a new proposal.
fn enforce_pending_capacity(pending: &mut HashMap<BatchDigest, PendingProposal>) {
    while pending.len() >= MAX_PENDING_PROPOSALS {
        // Find the oldest entry by creation time.
        if let Some(oldest_key) = pending
            .iter()
            .min_by_key(|(_, p)| p.created_at)
            .map(|(k, _)| *k)
        {
            warn!(
                evicted = %oldest_key,
                pending = pending.len(),
                max = MAX_PENDING_PROPOSALS,
                "cert aggregator: evicting oldest pending proposal — capacity limit reached"
            );
            pending.remove(&oldest_key);
        } else {
            break;
        }
    }
}

/// Handle a remote BatchVote: verify signature, add to pending, check quorum.
async fn handle_remote_vote(
    vote: BatchVote,
    engine: &Mutex<ConsensusEngine>,
    gossip: &GossipHandle,
    pending: &mut HashMap<BatchDigest, PendingProposal>,
) {
    let pp = match pending.get_mut(&vote.batch_digest) {
        Some(p) => p,
        None => {
            debug!(
                batch = %vote.batch_digest,
                voter = vote.voter.0,
                "cert aggregator: vote for unknown proposal — ignoring"
            );
            return;
        }
    };

    // Epoch guard
    if vote.epoch != pp.epoch {
        return;
    }

    // ── Signature verification (SEC-H2) ──────────────────────────────
    // Look up the voter's public key from the committee and verify the
    // Falcon-512 signature over the cert signing payload before accepting.
    {
        let eng = match engine.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let voter_info = match eng.committee().validator_info(vote.voter) {
            Some(v) => v,
            None => {
                warn!(
                    voter = vote.voter.0,
                    "cert aggregator: vote from unknown validator — rejecting"
                );
                return;
            }
        };
        let payload = match cert_signing_payload(
            pp.epoch,
            &vote.batch_digest,
            pp.origin,
            pp.round,
            &pp.parents,
        ) {
            Ok(p) => p,
            Err(e) => {
                warn!(error = %e, "cert aggregator: failed to compute signing payload for vote verification");
                return;
            }
        };
        if let Err(e) = FalconSigner::verify(
            &voter_info.falcon_pub_key,
            CERT_DOMAIN,
            &payload,
            &vote.signature,
        ) {
            warn!(
                voter = vote.voter.0,
                batch = %vote.batch_digest,
                error = %e,
                "cert aggregator: invalid vote signature — rejecting forged vote"
            );
            return;
        }
    }

    // Add the verified signature
    let is_new = pp.builder.add_signature(vote.voter, vote.signature);
    if !is_new {
        return; // duplicate
    }

    debug!(
        batch = %vote.batch_digest,
        voter = vote.voter.0,
        sigs = pp.builder.signature_count(),
        needed = pp.quorum_threshold.0,
        "cert aggregator: verified vote received"
    );

    // Check quorum (stake-weighted)
    let quorum_met = {
        let eng = match engine.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        eng.committee().is_quorum(pp.builder.signers())
    };
    if quorum_met {
        if let Some(pp) = pending.remove(&vote.batch_digest) {
            finalize_certificate(pp.builder, engine, gossip, pp.round).await;
        }
    }
}

/// Build the certificate from the builder, insert into engine, and broadcast.
async fn finalize_certificate(
    builder: CertificateBuilder,
    engine: &Mutex<ConsensusEngine>,
    gossip: &GossipHandle,
    round: RoundNumber,
) {
    let cert = {
        let eng = match engine.lock() {
            Ok(g) => g,
            Err(_) => {
                warn!("cert aggregator: engine lock poisoned in finalize");
                return;
            }
        };
        match builder.build(eng.committee()) {
            Ok(c) => c,
            Err(e) => {
                warn!(error = %e, "cert aggregator: failed to build certificate");
                return;
            }
        }
    };

    let cert_digest = cert.cert_digest;

    // Insert into local consensus engine (pre-verified)
    {
        let mut eng = match engine.lock() {
            Ok(g) => g,
            Err(_) => {
                warn!("cert aggregator: engine lock poisoned");
                return;
            }
        };
        match eng.insert_verified_certificate(cert.clone()) {
            Ok(committed) => {
                if committed {
                    info!(
                        round = round.0,
                        cert = %cert_digest,
                        "cert aggregator: certificate committed a sub-DAG"
                    );
                } else {
                    debug!(
                        round = round.0,
                        cert = %cert_digest,
                        "cert aggregator: certificate inserted into DAG"
                    );
                }
            }
            Err(e) => {
                warn!(
                    round = round.0,
                    error = %e,
                    "cert aggregator: certificate insertion failed"
                );
            }
        }
    }

    // Broadcast to peers
    if let Err(e) = consensus_bridge::publish_certificate(gossip, &cert).await {
        debug!(error = %e, "cert aggregator: failed to broadcast certificate");
    }

    info!(
        round = round.0,
        cert = %cert_digest,
        "cert aggregator: certificate finalized and broadcast"
    );
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_consensus::types::{ReputationScore, ValidatorBitset, ValidatorInfo};
    use nexus_consensus::{
        certificate::cert_signing_payload, types::CERT_DOMAIN, Committee, NarwhalCertificate,
        ValidatorRegistry,
    };
    use nexus_crypto::{FalconSigner, FalconVerifyKey, Signer};
    use nexus_network::{NetworkConfig, NetworkService};
    use nexus_primitives::{Amount, Blake3Digest};

    /// Minimal harness for cert aggregator unit tests.
    struct TestHarness {
        engine: Arc<Mutex<ConsensusEngine>>,
        keys: Vec<(Arc<FalconSigningKey>, FalconVerifyKey)>,
        epoch: EpochNumber,
    }

    impl TestHarness {
        fn new(n: u32, epoch_val: u64) -> Self {
            let mut keys = Vec::new();
            let mut validators = Vec::new();
            for i in 0..n {
                let (sk, vk) = FalconSigner::generate_keypair();
                validators.push(ValidatorInfo {
                    index: ValidatorIndex(i),
                    falcon_pub_key: vk.clone(),
                    stake: Amount(100),
                    reputation: ReputationScore::MAX,
                    is_slashed: false,
                    shard_id: None,
                });
                keys.push((Arc::new(sk), vk));
            }
            let committee = Committee::new(EpochNumber(epoch_val), validators).expect("committee");
            let engine = ConsensusEngine::new(EpochNumber(epoch_val), committee);
            Self {
                engine: Arc::new(Mutex::new(engine)),
                keys,
                epoch: EpochNumber(epoch_val),
            }
        }

        fn quorum_threshold(&self) -> Amount {
            self.engine.lock().unwrap().committee().quorum_threshold()
        }
    }

    /// Build a lightweight GossipHandle for tests (no peers, publish is fire-and-forget).
    async fn test_gossip() -> nexus_network::GossipHandle {
        let config = NetworkConfig::for_testing();
        let (net_handle, _service) = NetworkService::build(&config).expect("test network");
        // We intentionally drop `_service` — the GossipHandle's publish calls
        // will fail with ShuttingDown, which is fine since the cert aggregator's
        // handlers log and ignore publish errors.
        net_handle.gossip
    }

    // ── proposal_channel tests ──────────────────────────────────────────

    #[test]
    fn proposal_channel_send_receive() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let (tx, mut rx) = proposal_channel();
            let prop = LocalProposal {
                epoch: EpochNumber(1),
                batch_digest: Blake3Digest([42u8; 32]),
                batch_payload: vec![vec![1, 2, 3]],
                origin: ValidatorIndex(0),
                round: RoundNumber(0),
                parents: vec![],
                num_validators: 4,
                quorum_threshold: Amount(267),
            };
            tx.send(prop).await.unwrap();
            let received = rx.recv().await.unwrap();
            assert_eq!(received.batch_digest, Blake3Digest([42u8; 32]));
            assert_eq!(received.origin, ValidatorIndex(0));
        });
    }

    // ── handle_local_proposal tests ─────────────────────────────────────

    #[tokio::test]
    async fn local_proposal_single_validator_finalizes_immediately() {
        let h = TestHarness::new(1, 1);
        let signing_key = h.keys[0].0.clone();
        let gossip = test_gossip().await;
        let mut pending: HashMap<BatchDigest, PendingProposal> = HashMap::new();

        let lp = LocalProposal {
            epoch: h.epoch,
            batch_digest: Blake3Digest([10u8; 32]),
            batch_payload: vec![],
            origin: ValidatorIndex(0),
            round: RoundNumber(0),
            parents: vec![],
            num_validators: 1,
            quorum_threshold: h.quorum_threshold(),
        };

        handle_local_proposal(
            lp,
            &gossip,
            &h.engine,
            &signing_key,
            ValidatorIndex(0),
            &mut pending,
        )
        .await;

        assert!(
            pending.is_empty(),
            "single-validator proposal should finalize immediately"
        );
        let eng = h.engine.lock().unwrap();
        assert_eq!(eng.dag_size(), 1, "certificate should be inserted into DAG");
    }

    #[tokio::test]
    async fn local_proposal_multi_validator_goes_pending() {
        let h = TestHarness::new(4, 1);
        let signing_key = h.keys[0].0.clone();
        let gossip = test_gossip().await;
        let mut pending: HashMap<BatchDigest, PendingProposal> = HashMap::new();

        let digest = Blake3Digest([20u8; 32]);
        let lp = LocalProposal {
            epoch: h.epoch,
            batch_digest: digest,
            batch_payload: vec![],
            origin: ValidatorIndex(0),
            round: RoundNumber(0),
            parents: vec![],
            num_validators: 4,
            quorum_threshold: h.quorum_threshold(),
        };

        handle_local_proposal(
            lp,
            &gossip,
            &h.engine,
            &signing_key,
            ValidatorIndex(0),
            &mut pending,
        )
        .await;

        assert_eq!(pending.len(), 1);
        assert!(pending.contains_key(&digest));
        let pp = &pending[&digest];
        assert_eq!(
            pp.builder.signature_count(),
            1,
            "should have self-signature"
        );
    }

    // ── handle_remote_vote tests ────────────────────────────────────────

    #[tokio::test]
    async fn remote_vote_reaches_quorum_and_finalizes() {
        let h = TestHarness::new(4, 1);
        let quorum = h.quorum_threshold();
        let epoch = h.epoch;
        let digest = Blake3Digest([30u8; 32]);
        let origin = ValidatorIndex(0);
        let round = RoundNumber(0);
        let parents = vec![];

        let gossip = test_gossip().await;
        let mut pending: HashMap<BatchDigest, PendingProposal> = HashMap::new();

        let mut builder = CertificateBuilder::new(epoch, digest, origin, round, parents.clone(), 4);
        let payload = cert_signing_payload(epoch, &digest, origin, round, &parents).unwrap();
        let sig0 = FalconSigner::sign(&h.keys[0].0, CERT_DOMAIN, &payload);
        builder.add_signature(ValidatorIndex(0), sig0);

        pending.insert(
            digest,
            PendingProposal {
                builder,
                epoch,
                origin,
                round,
                parents: parents.clone(),
                quorum_threshold: quorum,
                created_at: Instant::now(),
            },
        );

        // Add votes from remaining validators until quorum is reached.
        // With 4 validators × 100 stake and quorum=267, we need 3 validators.
        for i in 1..4u32 {
            let sig = FalconSigner::sign(&h.keys[i as usize].0, CERT_DOMAIN, &payload);
            let vote = BatchVote {
                epoch,
                batch_digest: digest,
                origin,
                round,
                voter: ValidatorIndex(i),
                signature: sig,
            };
            handle_remote_vote(vote, &h.engine, &gossip, &mut pending).await;
        }

        assert!(pending.is_empty(), "should finalize after quorum reached");
        let eng = h.engine.lock().unwrap();
        assert!(eng.dag_size() >= 1, "certificate should be in DAG");
    }

    #[tokio::test]
    async fn remote_vote_for_unknown_proposal_ignored() {
        let h = TestHarness::new(4, 1);
        let gossip = test_gossip().await;
        let mut pending: HashMap<BatchDigest, PendingProposal> = HashMap::new();

        let payload = vec![1, 2, 3];
        let sig = FalconSigner::sign(&h.keys[1].0, CERT_DOMAIN, &payload);
        let vote = BatchVote {
            epoch: h.epoch,
            batch_digest: Blake3Digest([99u8; 32]),
            origin: ValidatorIndex(0),
            round: RoundNumber(0),
            voter: ValidatorIndex(1),
            signature: sig,
        };

        handle_remote_vote(vote, &h.engine, &gossip, &mut pending).await;

        assert!(pending.is_empty());
        let eng = h.engine.lock().unwrap();
        assert_eq!(eng.dag_size(), 0);
    }

    #[tokio::test]
    async fn duplicate_vote_is_no_op() {
        let h = TestHarness::new(4, 1);
        let quorum = h.quorum_threshold();
        let epoch = h.epoch;
        let digest = Blake3Digest([40u8; 32]);
        let origin = ValidatorIndex(0);
        let round = RoundNumber(0);
        let parents = vec![];

        let gossip = test_gossip().await;
        let mut pending: HashMap<BatchDigest, PendingProposal> = HashMap::new();

        let mut builder = CertificateBuilder::new(epoch, digest, origin, round, parents.clone(), 4);
        let payload = cert_signing_payload(epoch, &digest, origin, round, &parents).unwrap();
        let sig0 = FalconSigner::sign(&h.keys[0].0, CERT_DOMAIN, &payload);
        builder.add_signature(ValidatorIndex(0), sig0);

        pending.insert(
            digest,
            PendingProposal {
                builder,
                epoch,
                origin,
                round,
                parents: parents.clone(),
                quorum_threshold: quorum,
                created_at: Instant::now(),
            },
        );

        let sig1 = FalconSigner::sign(&h.keys[1].0, CERT_DOMAIN, &payload);
        for _ in 0..2 {
            let vote = BatchVote {
                epoch,
                batch_digest: digest,
                origin,
                round,
                voter: ValidatorIndex(1),
                signature: sig1.clone(),
            };
            handle_remote_vote(vote, &h.engine, &gossip, &mut pending).await;
        }

        assert_eq!(pending.len(), 1, "duplicate vote should not double-count");
        assert_eq!(
            pending[&digest].builder.signature_count(),
            2,
            "should have 2 unique signatures"
        );
    }

    #[tokio::test]
    async fn vote_with_wrong_epoch_ignored() {
        let h = TestHarness::new(4, 1);
        let quorum = h.quorum_threshold();
        let epoch = h.epoch;
        let digest = Blake3Digest([50u8; 32]);
        let origin = ValidatorIndex(0);
        let round = RoundNumber(0);
        let parents = vec![];

        let gossip = test_gossip().await;
        let mut pending: HashMap<BatchDigest, PendingProposal> = HashMap::new();

        let builder = CertificateBuilder::new(epoch, digest, origin, round, parents.clone(), 4);

        pending.insert(
            digest,
            PendingProposal {
                builder,
                epoch,
                origin,
                round,
                parents: parents.clone(),
                quorum_threshold: quorum,
                created_at: Instant::now(),
            },
        );

        let sig = FalconSigner::sign(&h.keys[1].0, CERT_DOMAIN, &[1, 2, 3]);
        let vote = BatchVote {
            epoch: EpochNumber(99), // wrong epoch
            batch_digest: digest,
            origin,
            round,
            voter: ValidatorIndex(1),
            signature: sig,
        };

        handle_remote_vote(vote, &h.engine, &gossip, &mut pending).await;

        assert_eq!(
            pending[&digest].builder.signature_count(),
            0,
            "wrong-epoch vote should be ignored"
        );
    }

    // ── handle_gossip_message tests ─────────────────────────────────────

    #[tokio::test]
    async fn gossip_message_oversized_dropped() {
        let h = TestHarness::new(4, 1);
        let gossip = test_gossip().await;
        let batch_store = Arc::new(BatchStore::new());
        let signing_key = h.keys[0].0.clone();
        let current_epoch = Arc::new(std::sync::atomic::AtomicU64::new(1));
        let mut pending: HashMap<BatchDigest, PendingProposal> = HashMap::new();

        let oversized = vec![0u8; MAX_CERT_MESSAGE_SIZE + 1];
        let ctx = CertAggContext {
            gossip: &gossip,
            engine: &h.engine,
            batch_store: &batch_store,
            signing_key: &signing_key,
            local_validator: ValidatorIndex(0),
            current_epoch: &current_epoch,
        };
        handle_gossip_message(&oversized, &ctx, &mut pending).await;

        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn gossip_message_invalid_bcs_dropped() {
        let h = TestHarness::new(4, 1);
        let gossip = test_gossip().await;
        let batch_store = Arc::new(BatchStore::new());
        let signing_key = h.keys[0].0.clone();
        let current_epoch = Arc::new(std::sync::atomic::AtomicU64::new(1));
        let mut pending: HashMap<BatchDigest, PendingProposal> = HashMap::new();

        let bad_data = vec![0xFF, 0xFE, 0xFD];
        let ctx = CertAggContext {
            gossip: &gossip,
            engine: &h.engine,
            batch_store: &batch_store,
            signing_key: &signing_key,
            local_validator: ValidatorIndex(0),
            current_epoch: &current_epoch,
        };
        handle_gossip_message(&bad_data, &ctx, &mut pending).await;

        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn gossip_certificate_message_ignored_by_aggregator() {
        let h = TestHarness::new(4, 1);
        let gossip = test_gossip().await;
        let batch_store = Arc::new(BatchStore::new());
        let signing_key = h.keys[0].0.clone();
        let current_epoch = Arc::new(std::sync::atomic::AtomicU64::new(1));
        let mut pending: HashMap<BatchDigest, PendingProposal> = HashMap::new();

        let cert = NarwhalCertificate {
            epoch: h.epoch,
            batch_digest: Blake3Digest([10u8; 32]),
            origin: ValidatorIndex(0),
            round: RoundNumber(0),
            parents: vec![],
            signatures: vec![],
            signers: ValidatorBitset::new(4),
            cert_digest: Blake3Digest([10u8; 32]),
        };
        let msg = ConsensusMessage::Certificate(cert);
        let data = bcs::to_bytes(&msg).unwrap();

        let ctx = CertAggContext {
            gossip: &gossip,
            engine: &h.engine,
            batch_store: &batch_store,
            signing_key: &signing_key,
            local_validator: ValidatorIndex(0),
            current_epoch: &current_epoch,
        };
        handle_gossip_message(&data, &ctx, &mut pending).await;

        assert!(
            pending.is_empty(),
            "Certificate should be silently ignored by cert_aggregator"
        );
    }

    // ── Cleanup tests ───────────────────────────────────────────────────

    #[test]
    fn stale_proposals_cleaned_up() {
        let digest_fresh = Blake3Digest([1u8; 32]);
        let digest_stale = Blake3Digest([2u8; 32]);

        let mut pending: HashMap<BatchDigest, PendingProposal> = HashMap::new();

        pending.insert(
            digest_fresh,
            PendingProposal {
                builder: CertificateBuilder::new(
                    EpochNumber(1),
                    digest_fresh,
                    ValidatorIndex(0),
                    RoundNumber(0),
                    vec![],
                    4,
                ),
                epoch: EpochNumber(1),
                origin: ValidatorIndex(0),
                round: RoundNumber(0),
                parents: vec![],
                quorum_threshold: Amount(267),
                created_at: Instant::now(),
            },
        );

        pending.insert(
            digest_stale,
            PendingProposal {
                builder: CertificateBuilder::new(
                    EpochNumber(1),
                    digest_stale,
                    ValidatorIndex(0),
                    RoundNumber(0),
                    vec![],
                    4,
                ),
                epoch: EpochNumber(1),
                origin: ValidatorIndex(0),
                round: RoundNumber(0),
                parents: vec![],
                quorum_threshold: Amount(267),
                created_at: Instant::now() - Duration::from_secs(60),
            },
        );

        pending.retain(|_, p| p.created_at.elapsed() < PROPOSAL_TIMEOUT);

        assert_eq!(pending.len(), 1);
        assert!(pending.contains_key(&digest_fresh));
        assert!(!pending.contains_key(&digest_stale));
    }

    // ── Phase A acceptance tests ─────────────────────────────────────────

    #[tokio::test]
    async fn forged_vote_should_be_rejected() {
        // A-3 / SEC-H2: a vote with an invalid signature must never be
        // aggregated; the pending proposal signature count must not increase.
        let h = TestHarness::new(4, 1);
        let quorum = h.quorum_threshold();
        let epoch = h.epoch;
        let digest = Blake3Digest([0xEE; 32]);
        let origin = ValidatorIndex(0);
        let round = RoundNumber(0);
        let parents = vec![];

        let gossip = test_gossip().await;
        let mut pending: HashMap<BatchDigest, PendingProposal> = HashMap::new();

        // Set up a pending proposal with the origin's valid self-signature.
        let mut builder = CertificateBuilder::new(epoch, digest, origin, round, parents.clone(), 4);
        let payload = cert_signing_payload(epoch, &digest, origin, round, &parents).unwrap();
        let sig0 = FalconSigner::sign(&h.keys[0].0, CERT_DOMAIN, &payload);
        builder.add_signature(ValidatorIndex(0), sig0);

        pending.insert(
            digest,
            PendingProposal {
                builder,
                epoch,
                origin,
                round,
                parents: parents.clone(),
                quorum_threshold: quorum,
                created_at: Instant::now(),
            },
        );

        // Create a forged vote: sign garbage data instead of the correct payload.
        let forged_sig = FalconSigner::sign(&h.keys[1].0, CERT_DOMAIN, b"garbage");
        let forged_vote = BatchVote {
            epoch,
            batch_digest: digest,
            origin,
            round,
            voter: ValidatorIndex(1),
            signature: forged_sig,
        };

        handle_remote_vote(forged_vote, &h.engine, &gossip, &mut pending).await;

        // Proposal should still be pending with only 1 signature (the origin's).
        assert!(
            pending.contains_key(&digest),
            "proposal should still be pending"
        );
        assert_eq!(
            pending[&digest].builder.signature_count(),
            1,
            "forged vote must not be counted — signature count should remain 1"
        );
    }

    // ── Phase B acceptance tests ─────────────────────────────────────────

    #[test]
    fn pending_proposals_should_be_bounded() {
        // B-3 / SEC-M4: the pending proposals map must respect MAX_PENDING_PROPOSALS.
        let mut pending: HashMap<BatchDigest, PendingProposal> = HashMap::new();

        // Fill to capacity
        for i in 0..MAX_PENDING_PROPOSALS {
            let mut seed = [0u8; 32];
            seed[0] = (i & 0xFF) as u8;
            seed[1] = ((i >> 8) & 0xFF) as u8;
            let digest = Blake3Digest(seed);
            pending.insert(
                digest,
                PendingProposal {
                    builder: CertificateBuilder::new(
                        EpochNumber(1),
                        digest,
                        ValidatorIndex(0),
                        RoundNumber(0),
                        vec![],
                        4,
                    ),
                    epoch: EpochNumber(1),
                    origin: ValidatorIndex(0),
                    round: RoundNumber(0),
                    parents: vec![],
                    quorum_threshold: Amount(267),
                    created_at: Instant::now(),
                },
            );
        }

        assert_eq!(pending.len(), MAX_PENDING_PROPOSALS);

        // enforce_pending_capacity should evict one to make room
        enforce_pending_capacity(&mut pending);
        assert!(
            pending.len() < MAX_PENDING_PROPOSALS,
            "capacity enforcement should evict at least one entry, got {}",
            pending.len(),
        );
    }

    // ── Phase B: per-tx verification in remote proposals ─────────────

    /// Helper: build a valid BCS-encoded signed transaction.
    fn make_valid_tx_bytes() -> Vec<u8> {
        use nexus_crypto::{DilithiumSigner, DilithiumSigningKey, DilithiumVerifyKey};
        use nexus_execution::types::{
            compute_tx_digest, TransactionBody, TransactionPayload, TX_DOMAIN,
        };
        use nexus_primitives::{Amount, TokenId};

        let (sk, pk): (DilithiumSigningKey, DilithiumVerifyKey) =
            DilithiumSigner::generate_keypair();
        let sender = nexus_primitives::AccountAddress::from_dilithium_pubkey(pk.as_bytes());
        let body = TransactionBody {
            sender,
            sequence_number: 0,
            expiry_epoch: EpochNumber(1000),
            gas_limit: 50_000,
            gas_price: 1,
            target_shard: None,
            payload: TransactionPayload::Transfer {
                recipient: nexus_primitives::AccountAddress([0xBB; 32]),
                amount: Amount(100),
                token: TokenId::Native,
            },
            chain_id: 1,
        };
        let digest = compute_tx_digest(&body).unwrap();
        let sig = DilithiumSigner::sign(&sk, TX_DOMAIN, digest.as_bytes());
        let tx = nexus_execution::types::SignedTransaction {
            body,
            signature: sig,
            sender_pk: pk,
            digest,
        };
        bcs::to_bytes(&tx).unwrap()
    }

    /// Helper: compute batch digest for given payload + origin + round.
    fn make_batch_digest(
        origin: ValidatorIndex,
        round: RoundNumber,
        payload: &[Vec<u8>],
    ) -> Blake3Digest {
        let bytes = bcs::to_bytes(&(origin, round, payload)).unwrap();
        Blake3Hasher::digest(b"nexus::narwhal::batch::v1", &bytes)
    }

    /// B-5: A remote proposal containing a transaction with an invalid
    /// signature must be rejected entirely.
    #[tokio::test]
    async fn remote_proposal_with_invalid_tx_should_be_rejected() {
        use nexus_crypto::DilithiumSigner;
        use nexus_execution::types::{
            compute_tx_digest, TransactionBody, TransactionPayload, TX_DOMAIN,
        };
        use nexus_primitives::{Amount, TokenId};

        let h = TestHarness::new(4, 1);
        let gossip = test_gossip().await;
        let batch_store = Arc::new(BatchStore::new());
        let signing_key = h.keys[0].0.clone();
        let current_epoch = Arc::new(std::sync::atomic::AtomicU64::new(1));
        let mut pending: HashMap<BatchDigest, PendingProposal> = HashMap::new();

        // Build a tx with a forged signature (signed by wrong key).
        let (_sk_good, pk_good) = DilithiumSigner::generate_keypair();
        let sender = nexus_primitives::AccountAddress::from_dilithium_pubkey(pk_good.as_bytes());
        let body = TransactionBody {
            sender,
            sequence_number: 0,
            expiry_epoch: EpochNumber(1000),
            gas_limit: 50_000,
            gas_price: 1,
            target_shard: None,
            payload: TransactionPayload::Transfer {
                recipient: nexus_primitives::AccountAddress([0xBB; 32]),
                amount: Amount(100),
                token: TokenId::Native,
            },
            chain_id: 1,
        };
        let digest = compute_tx_digest(&body).unwrap();
        let (sk_evil, _) = DilithiumSigner::generate_keypair();
        let bad_sig = DilithiumSigner::sign(&sk_evil, TX_DOMAIN, digest.as_bytes());
        let bad_tx = nexus_execution::types::SignedTransaction {
            body,
            signature: bad_sig,
            sender_pk: pk_good,
            digest,
        };
        let bad_bytes = bcs::to_bytes(&bad_tx).unwrap();

        let origin = ValidatorIndex(1);
        let round = RoundNumber(0);
        let payload = vec![bad_bytes];
        let batch_digest = make_batch_digest(origin, round, &payload);

        let proposal = BatchProposal {
            epoch: h.epoch,
            batch_digest,
            batch_payload: payload,
            origin,
            round,
            parents: vec![],
        };

        handle_remote_proposal(
            proposal,
            &CertAggContext {
                gossip: &gossip,
                engine: &h.engine,
                batch_store: &batch_store,
                signing_key: &signing_key,
                local_validator: ValidatorIndex(0),
                current_epoch: &current_epoch,
            },
            &mut pending,
        )
        .await;

        // Proposal must have been dropped — no pending entry, no batch stored.
        assert!(
            pending.is_empty(),
            "proposal with invalid tx signature must be dropped"
        );
        assert!(
            batch_store.get(&batch_digest).is_none(),
            "batch with invalid tx must not be stored"
        );
    }

    /// B-5: A remote proposal containing one valid tx and one
    /// un-decodable tx must be rejected entirely.
    #[tokio::test]
    async fn remote_proposal_with_partial_decode_failure_should_be_rejected() {
        let h = TestHarness::new(4, 1);
        let gossip = test_gossip().await;
        let batch_store = Arc::new(BatchStore::new());
        let signing_key = h.keys[0].0.clone();
        let current_epoch = Arc::new(std::sync::atomic::AtomicU64::new(1));
        let mut pending: HashMap<BatchDigest, PendingProposal> = HashMap::new();

        let good_bytes = make_valid_tx_bytes();
        let garbage_bytes = vec![0xFF, 0xFE, 0xFD, 0xFC];

        let origin = ValidatorIndex(1);
        let round = RoundNumber(0);
        let payload = vec![good_bytes, garbage_bytes];
        let batch_digest = make_batch_digest(origin, round, &payload);

        let proposal = BatchProposal {
            epoch: h.epoch,
            batch_digest,
            batch_payload: payload,
            origin,
            round,
            parents: vec![],
        };

        handle_remote_proposal(
            proposal,
            &CertAggContext {
                gossip: &gossip,
                engine: &h.engine,
                batch_store: &batch_store,
                signing_key: &signing_key,
                local_validator: ValidatorIndex(0),
                current_epoch: &current_epoch,
            },
            &mut pending,
        )
        .await;

        assert!(
            pending.is_empty(),
            "proposal with decode failure must be dropped entirely"
        );
        assert!(
            batch_store.get(&batch_digest).is_none(),
            "batch with decode failure must not be stored"
        );
    }
}
