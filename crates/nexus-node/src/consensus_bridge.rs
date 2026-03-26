// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Consensus message relay — bridges the gossip P2P layer with the
//! local [`ConsensusEngine`].
//!
//! **Inbound**: receives BCS-encoded [`ConsensusMessage`]s from
//! [`Topic::Consensus`], decodes them, and feeds certificates into the
//! consensus engine for DAG insertion and BFT ordering.
//!
//! **Outbound**: accepts locally produced certificates and publishes
//! them to the gossip network so peers can incorporate them.

use std::sync::{Arc, Mutex};

use nexus_consensus::{ConsensusEngine, NarwhalCertificate};
use nexus_crypto::FalconSignature;
use nexus_network::types::Topic;
use nexus_network::GossipHandle;
use nexus_primitives::{BatchDigest, CertDigest, EpochNumber, RoundNumber, ValidatorIndex};
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::node_metrics;
use crate::readiness::SubsystemHandle;

/// Maximum encoded size for a consensus message (4 MB).
///
/// Defense-in-depth: rejects oversized payloads *before* BCS deserialization.
/// With ML-DSA-65 each signed transaction is ~5.5 KB, so a full 512-tx
/// batch can reach ~2.8 MB.  4 MB provides headroom while still guarding
/// against allocation DoS.
const MAX_CERT_MESSAGE_SIZE: usize = 4 * 1024 * 1024;

// ── Wire envelope ────────────────────────────────────────────────────────────

/// Envelope type for consensus messages exchanged over gossipsub.
///
/// All variants are BCS-serialized before publishing to `Topic::Consensus`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConsensusMessage {
    /// A fully certified Narwhal certificate (2f+1 signatures).
    Certificate(NarwhalCertificate),
    /// A batch proposal requesting validator signatures.
    BatchProposal(BatchProposal),
    /// A validator's vote (signature) on a batch proposal.
    BatchVote(BatchVote),
}

/// Batch proposal broadcast by a proposer to collect validator signatures.
///
/// Contains the batch header fields (for signing) and the batch payload
/// (transaction bytes) so receiving validators can store the data locally.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchProposal {
    /// Epoch this proposal belongs to.
    pub epoch: EpochNumber,
    /// Digest of the batch (BLAKE3 over encoded payload).
    pub batch_digest: BatchDigest,
    /// Index of the proposing validator.
    pub origin: ValidatorIndex,
    /// DAG round for this proposal.
    pub round: RoundNumber,
    /// Parent certificate digests from the previous round.
    pub parents: Vec<CertDigest>,
    /// BCS-encoded transaction bytes comprising the batch.
    pub batch_payload: Vec<Vec<u8>>,
}

/// A validator's vote (signature) on a batch proposal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchVote {
    /// Epoch this vote belongs to.
    pub epoch: EpochNumber,
    /// Batch digest being voted on.
    pub batch_digest: BatchDigest,
    /// The original proposer's validator index.
    pub origin: ValidatorIndex,
    /// DAG round of the proposal.
    pub round: RoundNumber,
    /// Index of the voting validator.
    pub voter: ValidatorIndex,
    /// FALCON-512 signature over the proposal header.
    pub signature: FalconSignature,
}

// ── Inbound bridge (gossip → engine) ─────────────────────────────────────────

/// Spawn a background task that relays consensus messages from gossip into
/// the local [`ConsensusEngine`].
///
/// The task subscribes to [`Topic::Consensus`] and processes incoming
/// certificates until the broadcast channel closes.
///
/// Returns a `JoinHandle` so the caller can await or abort.
pub async fn spawn_consensus_inbound_bridge(
    gossip: GossipHandle,
    engine: Arc<Mutex<ConsensusEngine>>,
    current_epoch: Arc<std::sync::atomic::AtomicU64>,
    readiness_handle: SubsystemHandle,
) -> Result<JoinHandle<()>, nexus_network::NetworkError> {
    gossip.subscribe(Topic::Consensus).await?;
    let mut rx = gossip.topic_receiver(Topic::Consensus);

    let handle = tokio::spawn(async move {
        debug!("consensus inbound bridge started");

        loop {
            match rx.recv().await {
                Ok(data) => {
                    handle_incoming_consensus_msg(&data, &engine, &current_epoch);
                    readiness_handle.report_progress();
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!(
                        skipped = n,
                        "consensus inbound bridge lagged, some messages dropped"
                    );
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    debug!("consensus broadcast channel closed — bridge stopping");
                    break;
                }
            }
        }

        debug!("consensus inbound bridge stopped");
    });

    Ok(handle)
}

/// Decode and process a single incoming consensus message.
fn handle_incoming_consensus_msg(
    data: &[u8],
    engine: &Mutex<ConsensusEngine>,
    current_epoch: &std::sync::atomic::AtomicU64,
) {
    // 0. Size guard — reject before BCS deserialization (defense-in-depth)
    if data.len() > MAX_CERT_MESSAGE_SIZE {
        debug!(
            size = data.len(),
            limit = MAX_CERT_MESSAGE_SIZE,
            "consensus bridge: message exceeds size limit — dropping"
        );
        return;
    }

    // 1. Decode BCS envelope
    let msg: ConsensusMessage = match bcs::from_bytes(data) {
        Ok(m) => m,
        Err(e) => {
            debug!(error = %e, "consensus bridge: failed to decode message — dropping");
            return;
        }
    };

    match msg {
        ConsensusMessage::Certificate(cert) => {
            handle_incoming_certificate(cert, engine, current_epoch);
        }
        ConsensusMessage::BatchProposal(_) | ConsensusMessage::BatchVote(_) => {
            // Handled by the cert_aggregator via its own topic receiver.
            // Silently ignore here to avoid double-processing.
        }
    }
}

/// Validate and insert a certificate into the consensus engine.
fn handle_incoming_certificate(
    cert: NarwhalCertificate,
    engine: &Mutex<ConsensusEngine>,
    current_epoch: &std::sync::atomic::AtomicU64,
) {
    let epoch = EpochNumber(current_epoch.load(std::sync::atomic::Ordering::Acquire));

    // 1. Epoch guard — drop certificates from stale or future epochs.
    if cert.epoch != epoch {
        debug!(
            cert_epoch = cert.epoch.0,
            current = epoch.0,
            "consensus bridge: epoch mismatch — dropping certificate"
        );
        return;
    }

    // 2. Feed into the consensus engine (verify + DAG insert + try commit).
    let digest = cert.cert_digest;
    let round = cert.round;
    let origin = cert.origin;

    let mut eng = match engine.lock() {
        Ok(guard) => guard,
        Err(_) => {
            warn!("consensus bridge: engine lock poisoned — dropping certificate");
            return;
        }
    };

    match eng.process_certificate(cert) {
        Ok(committed) => {
            if committed {
                let pending = eng.pending_commits();
                node_metrics::consensus_cert_committed();
                node_metrics::consensus_cert_accepted(round.0);
                info!(
                    %digest,
                    round = round.0,
                    origin = origin.0,
                    pending,
                    "consensus bridge: certificate committed a new sub-DAG"
                );
            } else {
                node_metrics::consensus_cert_accepted(round.0);
                debug!(
                    %digest,
                    round = round.0,
                    origin = origin.0,
                    "consensus bridge: certificate accepted into DAG"
                );
            }
        }
        Err(e) => {
            node_metrics::consensus_cert_rejected();
            debug!(
                %digest,
                round = round.0,
                origin = origin.0,
                error = %e,
                "consensus bridge: certificate rejected"
            );
        }
    }
}

// ── Outbound helper (engine → gossip) ────────────────────────────────────────

/// Publish a locally produced certificate to the gossip network.
///
/// This is called by the node when it participates in certificate aggregation.
/// The certificate is BCS-encoded inside a [`ConsensusMessage::Certificate`]
/// envelope and published to [`Topic::Consensus`].
pub async fn publish_certificate(
    gossip: &GossipHandle,
    cert: &NarwhalCertificate,
) -> Result<(), nexus_network::NetworkError> {
    let msg = ConsensusMessage::Certificate(cert.clone());
    let data = bcs::to_bytes(&msg).map_err(|e| nexus_network::NetworkError::InvalidMessage {
        reason: format!("failed to BCS-encode certificate: {e}"),
    })?;
    gossip.publish(Topic::Consensus, data).await
}

/// Publish a batch proposal to the gossip network.
pub async fn publish_batch_proposal(
    gossip: &GossipHandle,
    proposal: &BatchProposal,
) -> Result<(), nexus_network::NetworkError> {
    let msg = ConsensusMessage::BatchProposal(proposal.clone());
    let data = bcs::to_bytes(&msg).map_err(|e| nexus_network::NetworkError::InvalidMessage {
        reason: format!("failed to BCS-encode batch proposal: {e}"),
    })?;
    gossip.publish(Topic::Consensus, data).await
}

/// Publish a batch vote to the gossip network.
pub async fn publish_batch_vote(
    gossip: &GossipHandle,
    vote: &BatchVote,
) -> Result<(), nexus_network::NetworkError> {
    let msg = ConsensusMessage::BatchVote(vote.clone());
    let data = bcs::to_bytes(&msg).map_err(|e| nexus_network::NetworkError::InvalidMessage {
        reason: format!("failed to BCS-encode batch vote: {e}"),
    })?;
    gossip.publish(Topic::Consensus, data).await
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_consensus::types::{ReputationScore, ValidatorBitset, ValidatorInfo};
    use nexus_consensus::{
        certificate::{cert_signing_payload, CertificateBuilder},
        types::CERT_DOMAIN,
        Committee,
    };
    use nexus_crypto::{FalconSigner, FalconSigningKey, FalconVerifyKey, Signer};
    use nexus_primitives::{Amount, Blake3Digest, RoundNumber, ValidatorIndex};

    /// Test harness — builds a committee and keyset to produce valid certs.
    struct TestHarness {
        engine: Arc<Mutex<ConsensusEngine>>,
        keys: Vec<(FalconSigningKey, FalconVerifyKey)>,
        epoch: Arc<std::sync::atomic::AtomicU64>,
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
                keys.push((sk, vk));
            }
            let committee = Committee::new(EpochNumber(epoch_val), validators).expect("committee");
            let engine = ConsensusEngine::new(EpochNumber(epoch_val), committee);
            Self {
                engine: Arc::new(Mutex::new(engine)),
                keys,
                epoch: Arc::new(std::sync::atomic::AtomicU64::new(epoch_val)),
            }
        }

        /// Build a properly signed certificate for the test committee.
        fn build_cert(
            &self,
            origin: u32,
            round: u64,
            parents: Vec<nexus_primitives::CertDigest>,
            batch_seed: u8,
        ) -> NarwhalCertificate {
            let epoch = EpochNumber(self.epoch.load(std::sync::atomic::Ordering::Acquire));
            let batch_digest = Blake3Digest([batch_seed; 32]);
            let origin_idx = ValidatorIndex(origin);
            let round_num = RoundNumber(round);

            let mut builder = CertificateBuilder::new(
                epoch,
                batch_digest,
                origin_idx,
                round_num,
                parents.clone(),
                self.keys.len() as u32,
            );

            let payload =
                cert_signing_payload(epoch, &batch_digest, origin_idx, round_num, &parents)
                    .unwrap();

            // Sign with all validators (always meets stake-weighted quorum).
            for (i, (sk, _)) in self.keys.iter().enumerate() {
                let sig = FalconSigner::sign(sk, CERT_DOMAIN, &payload);
                builder.add_signature(ValidatorIndex(i as u32), sig);
            }

            builder
                .build(self.engine.lock().unwrap().committee())
                .unwrap()
        }

        /// Build a pre-verified genesis certificate (round 0, no signatures).
        fn genesis_cert(&self, origin: u32, seed: u8) -> NarwhalCertificate {
            let epoch_val = self.epoch.load(std::sync::atomic::Ordering::Acquire);
            let epoch = EpochNumber(epoch_val);
            let batch_digest = Blake3Digest([seed; 32]);
            let origin_idx = ValidatorIndex(origin);
            let round = RoundNumber(0);
            let parents = vec![];
            let cert_digest = nexus_consensus::compute_cert_digest(
                epoch,
                &batch_digest,
                origin_idx,
                round,
                &parents,
            )
            .unwrap();
            NarwhalCertificate {
                epoch,
                batch_digest,
                origin: origin_idx,
                round,
                parents,
                signatures: vec![],
                signers: ValidatorBitset::new(self.keys.len() as u32),
                cert_digest,
            }
        }

        /// Encode a certificate as a consensus message (BCS).
        fn encode_cert(&self, cert: &NarwhalCertificate) -> Vec<u8> {
            let msg = ConsensusMessage::Certificate(cert.clone());
            bcs::to_bytes(&msg).expect("encode")
        }
    }

    // ── Unit tests for handle_incoming_consensus_msg ────────────────────

    #[test]
    fn valid_certificate_accepted_into_dag() {
        let h = TestHarness::new(4, 1);

        // Insert genesis certs directly (pre-verified) so round-1 parents exist.
        let g0 = h.genesis_cert(0, 10);
        let g1 = h.genesis_cert(1, 11);
        let d0 = g0.cert_digest;
        let d1 = g1.cert_digest;
        {
            let mut eng = h.engine.lock().unwrap();
            eng.insert_verified_certificate(g0).unwrap();
            eng.insert_verified_certificate(g1).unwrap();
        }

        // Build a valid round-1 certificate and relay through the bridge.
        let cert = h.build_cert(0, 1, vec![d0, d1], 20);
        let data = h.encode_cert(&cert);

        handle_incoming_consensus_msg(&data, &h.engine, &h.epoch);

        let eng = h.engine.lock().unwrap();
        // 2 genesis + 1 = 3
        assert_eq!(eng.dag_size(), 3);
    }

    #[test]
    fn certificate_with_wrong_epoch_dropped() {
        let h = TestHarness::new(4, 1);

        // Build a certificate for epoch 99 (doesn't match engine epoch 1).
        let mut cert = h.genesis_cert(0, 10);
        cert.epoch = EpochNumber(99);
        let msg = ConsensusMessage::Certificate(cert);
        let data = bcs::to_bytes(&msg).expect("encode");

        handle_incoming_consensus_msg(&data, &h.engine, &h.epoch);

        let eng = h.engine.lock().unwrap();
        assert_eq!(eng.dag_size(), 0, "wrong-epoch cert should be dropped");
    }

    #[test]
    fn invalid_bcs_dropped() {
        let h = TestHarness::new(4, 1);
        let bad_data = vec![0xFF, 0xFE, 0xFD];

        handle_incoming_consensus_msg(&bad_data, &h.engine, &h.epoch);

        let eng = h.engine.lock().unwrap();
        assert_eq!(eng.dag_size(), 0, "invalid BCS should be dropped");
    }

    #[test]
    fn duplicate_certificate_rejected_gracefully() {
        let h = TestHarness::new(4, 1);

        let g0 = h.genesis_cert(0, 10);
        let data = h.encode_cert(&g0);

        // Insert the genesis cert directly first.
        {
            let mut eng = h.engine.lock().unwrap();
            eng.insert_verified_certificate(g0).unwrap();
        }

        // Now relay the same cert via bridge — should be rejected but not panic.
        // (The cert has no signatures so process_certificate will reject it on
        // quorum check, which is the expected path for genesis-style certs via
        // the network. Real duplicate certs would hit DuplicateCertificate.)
        handle_incoming_consensus_msg(&data, &h.engine, &h.epoch);

        let eng = h.engine.lock().unwrap();
        assert_eq!(eng.dag_size(), 1, "duplicate should not increase DAG size");
    }

    #[test]
    fn certificate_triggers_commit() {
        let h = TestHarness::new(4, 1);

        // Round 0: genesis certs.
        let g0 = h.genesis_cert(0, 10);
        let g1 = h.genesis_cert(1, 11);
        let d0 = g0.cert_digest;
        let d1 = g1.cert_digest;
        {
            let mut eng = h.engine.lock().unwrap();
            eng.insert_verified_certificate(g0).unwrap();
            eng.insert_verified_certificate(g1).unwrap();
        }

        // Round 1: valid cert that triggers a commit.
        let cert = h.build_cert(0, 1, vec![d0, d1], 20);
        let data = h.encode_cert(&cert);

        handle_incoming_consensus_msg(&data, &h.engine, &h.epoch);

        let mut eng = h.engine.lock().unwrap();
        assert!(eng.pending_commits() > 0, "should have pending commits");
        let batches = eng.take_committed();
        assert!(!batches.is_empty(), "committed batch should be available");
    }

    // ── Outbound publish test ───────────────────────────────────────────

    #[test]
    fn consensus_message_round_trip_serialization() {
        let h = TestHarness::new(4, 1);
        let cert = h.genesis_cert(0, 10);
        let msg = ConsensusMessage::Certificate(cert.clone());

        let encoded = bcs::to_bytes(&msg).expect("encode");
        let decoded: ConsensusMessage = bcs::from_bytes(&encoded).expect("decode");

        match decoded {
            ConsensusMessage::Certificate(dec_cert) => {
                assert_eq!(dec_cert.cert_digest, cert.cert_digest);
                assert_eq!(dec_cert.epoch, cert.epoch);
                assert_eq!(dec_cert.origin, cert.origin);
                assert_eq!(dec_cert.round, cert.round);
            }
            _ => panic!("expected Certificate"),
        }
    }

    // ── Async bridge lifecycle test ─────────────────────────────────────

    #[tokio::test]
    async fn inbound_bridge_spawns_and_aborts_cleanly() {
        use nexus_network::{NetworkConfig, NetworkService};

        let config = NetworkConfig::for_testing();
        let (net_handle, service) = NetworkService::build(&config).expect("build");
        let shutdown = net_handle.transport.clone();
        let net_task = tokio::spawn(service.run());

        let h = TestHarness::new(4, 1);

        let bridge = spawn_consensus_inbound_bridge(
            net_handle.gossip.clone(),
            h.engine.clone(),
            h.epoch.clone(),
            crate::readiness::NodeReadiness::new().consensus_handle(),
        )
        .await
        .expect("bridge should spawn");

        // Bridge is running — abort it.
        bridge.abort();
        let _ = bridge.await; // JoinError::Cancelled is expected

        // Engine should still be usable.
        {
            let eng = h.engine.lock().unwrap();
            assert_eq!(eng.dag_size(), 0);
        }

        // Cleanup: shutdown network.
        drop(net_handle);
        shutdown.shutdown().await.expect("shutdown");
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), net_task).await;
    }

    #[tokio::test]
    async fn publish_certificate_encodes_correctly() {
        use nexus_network::{NetworkConfig, NetworkService};

        let config = NetworkConfig::for_testing();
        let (net_handle, service) = NetworkService::build(&config).expect("build");
        let shutdown = net_handle.transport.clone();
        let net_task = tokio::spawn(service.run());

        // Subscribe to consensus topic so gossipsub considers us a valid subscriber.
        net_handle
            .gossip
            .subscribe(Topic::Consensus)
            .await
            .expect("subscribe");

        let h = TestHarness::new(4, 1);
        let cert = h.genesis_cert(0, 42);

        // Verify the encoding path works (BCS round-trip).
        let msg = ConsensusMessage::Certificate(cert.clone());
        let encoded = bcs::to_bytes(&msg).expect("encode");
        let decoded: ConsensusMessage = bcs::from_bytes(&encoded).expect("decode");
        match decoded {
            ConsensusMessage::Certificate(dec) => {
                assert_eq!(dec.cert_digest, cert.cert_digest);
            }
            _ => panic!("expected Certificate"),
        }

        // Publish may fail with no peers (gossipsub mesh empty) — that's expected.
        // We only verify the function doesn't panic and returns a proper Result.
        let _result = publish_certificate(&net_handle.gossip, &cert).await;

        // Cleanup.
        drop(net_handle);
        shutdown.shutdown().await.expect("shutdown");
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), net_task).await;
    }

    #[test]
    fn handle_oversized_cert_drops() {
        let h = TestHarness::new(4, 1);
        let epoch = h.epoch.clone();

        // Craft a payload larger than MAX_CERT_MESSAGE_SIZE
        let oversized = vec![0u8; MAX_CERT_MESSAGE_SIZE + 1];
        handle_incoming_consensus_msg(&oversized, &h.engine, &epoch);
        // Should be silently dropped — no panic, engine state unchanged
        let eng = h.engine.lock().unwrap();
        assert_eq!(eng.pending_commits(), 0);
    }

    #[test]
    fn handle_at_limit_cert_attempts_decode() {
        let h = TestHarness::new(4, 1);
        let epoch = h.epoch.clone();

        // Exactly at limit — should pass size check (but fail BCS decode)
        let at_limit = vec![0u8; MAX_CERT_MESSAGE_SIZE];
        handle_incoming_consensus_msg(&at_limit, &h.engine, &epoch);
        let eng = h.engine.lock().unwrap();
        assert_eq!(eng.pending_commits(), 0);
    }

    // ── BatchProposal and BatchVote serialization tests ─────────────────

    #[test]
    fn batch_proposal_round_trip_serialization() {
        let proposal = BatchProposal {
            epoch: EpochNumber(5),
            batch_digest: Blake3Digest([0xAA; 32]),
            origin: ValidatorIndex(2),
            round: RoundNumber(10),
            parents: vec![Blake3Digest([0xBB; 32]), Blake3Digest([0xCC; 32])],
            batch_payload: vec![vec![1, 2, 3], vec![4, 5, 6]],
        };

        let msg = ConsensusMessage::BatchProposal(proposal.clone());
        let encoded = bcs::to_bytes(&msg).expect("encode");
        let decoded: ConsensusMessage = bcs::from_bytes(&encoded).expect("decode");

        match decoded {
            ConsensusMessage::BatchProposal(dec) => {
                assert_eq!(dec.epoch, proposal.epoch);
                assert_eq!(dec.batch_digest, proposal.batch_digest);
                assert_eq!(dec.origin, proposal.origin);
                assert_eq!(dec.round, proposal.round);
                assert_eq!(dec.parents.len(), 2);
                assert_eq!(dec.batch_payload.len(), 2);
                assert_eq!(dec.batch_payload[0], vec![1, 2, 3]);
            }
            other => panic!("expected BatchProposal, got {other:?}"),
        }
    }

    #[test]
    fn batch_vote_round_trip_serialization() {
        let h = TestHarness::new(4, 1);
        let payload = b"test payload".to_vec();
        let sig = FalconSigner::sign(&h.keys[1].0, CERT_DOMAIN, &payload);

        let vote = BatchVote {
            epoch: EpochNumber(3),
            batch_digest: Blake3Digest([0xDD; 32]),
            origin: ValidatorIndex(0),
            round: RoundNumber(7),
            voter: ValidatorIndex(1),
            signature: sig.clone(),
        };

        let msg = ConsensusMessage::BatchVote(vote.clone());
        let encoded = bcs::to_bytes(&msg).expect("encode");
        let decoded: ConsensusMessage = bcs::from_bytes(&encoded).expect("decode");

        match decoded {
            ConsensusMessage::BatchVote(dec) => {
                assert_eq!(dec.epoch, vote.epoch);
                assert_eq!(dec.batch_digest, vote.batch_digest);
                assert_eq!(dec.origin, vote.origin);
                assert_eq!(dec.round, vote.round);
                assert_eq!(dec.voter, vote.voter);
                assert_eq!(dec.signature.as_bytes(), sig.as_bytes());
            }
            other => panic!("expected BatchVote, got {other:?}"),
        }
    }

    #[test]
    fn batch_proposal_and_vote_ignored_by_inbound_bridge() {
        let h = TestHarness::new(4, 1);
        let epoch = EpochNumber(h.epoch.load(std::sync::atomic::Ordering::Acquire));

        // BatchProposal should be silently ignored by the inbound bridge
        let proposal = BatchProposal {
            epoch,
            batch_digest: Blake3Digest([0xEE; 32]),
            origin: ValidatorIndex(0),
            round: RoundNumber(0),
            parents: vec![],
            batch_payload: vec![vec![1]],
        };
        let msg = ConsensusMessage::BatchProposal(proposal);
        let data = bcs::to_bytes(&msg).unwrap();
        handle_incoming_consensus_msg(&data, &h.engine, &h.epoch);

        let eng = h.engine.lock().unwrap();
        assert_eq!(
            eng.dag_size(),
            0,
            "BatchProposal should be ignored by inbound bridge"
        );
        drop(eng);

        // BatchVote should also be silently ignored
        let sig = FalconSigner::sign(&h.keys[0].0, CERT_DOMAIN, &[1, 2, 3]);
        let vote = BatchVote {
            epoch,
            batch_digest: Blake3Digest([0xFF; 32]),
            origin: ValidatorIndex(0),
            round: RoundNumber(0),
            voter: ValidatorIndex(1),
            signature: sig,
        };
        let msg = ConsensusMessage::BatchVote(vote);
        let data = bcs::to_bytes(&msg).unwrap();
        handle_incoming_consensus_msg(&data, &h.engine, &h.epoch);

        let eng = h.engine.lock().unwrap();
        assert_eq!(
            eng.dag_size(),
            0,
            "BatchVote should be ignored by inbound bridge"
        );
    }
}
