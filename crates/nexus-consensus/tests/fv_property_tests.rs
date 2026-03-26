// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Formal Verification Property Tests — Consensus Layer
//!
//! Invariant references: Solutions/21-Formal-Verification-Object-Register.md
//! Proof workspace: proofs/property-tests/fv_consensus_properties.rs (design doc)
//!
//! Coverage:
//! - FV-CO-004: CommitSequence strict monotonicity
//! - FV-CO-006: Quorum threshold formula (2f+1)
//! - FV-CO-007: Certificate digest domain separation
//! - FV-CR-001: Domain separation tag uniqueness (consensus subset)

use nexus_consensus::certificate::{cert_signing_payload, CertificateBuilder};
use nexus_consensus::types::{
    ReputationScore, ValidatorBitset, ValidatorInfo, BATCH_DOMAIN, CERT_DOMAIN, VOTE_DOMAIN,
};
use nexus_consensus::validator::Committee;
use nexus_consensus::{compute_cert_digest, ConsensusEngine, ValidatorRegistry};
use nexus_crypto::{
    Blake3Hasher, CryptoHasher, FalconSigner, FalconSigningKey, FalconVerifyKey, Signer,
};
use nexus_primitives::{
    Amount, Blake3Digest, CertDigest, CommitSequence, EpochNumber, RoundNumber, ValidatorIndex,
};

// ── Helpers ──────────────────────────────────────────────────────────────────

struct FvHarness {
    engine: ConsensusEngine,
    keys: Vec<(FalconSigningKey, FalconVerifyKey)>,
    num_validators: u32,
}

impl FvHarness {
    fn new(n: u32) -> Self {
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
        let committee = Committee::new(EpochNumber(1), validators).expect("test committee");
        let engine = ConsensusEngine::new(EpochNumber(1), committee);
        Self {
            engine,
            keys,
            num_validators: n,
        }
    }

    fn genesis_cert(&self, origin: u32, seed: u8) -> nexus_consensus::types::NarwhalCertificate {
        let epoch = EpochNumber(1);
        let batch_digest = Blake3Digest([seed; 32]);
        let origin_idx = ValidatorIndex(origin);
        let round = RoundNumber(0);
        let parents = vec![];
        let cert_digest =
            nexus_consensus::compute_cert_digest(epoch, &batch_digest, origin_idx, round, &parents)
                .unwrap();
        nexus_consensus::types::NarwhalCertificate {
            epoch,
            batch_digest,
            origin: origin_idx,
            round,
            parents,
            signatures: vec![],
            signers: ValidatorBitset::new(self.num_validators),
            cert_digest,
        }
    }

    fn build_cert(
        &self,
        origin: u32,
        round: u64,
        parents: Vec<CertDigest>,
        batch_seed: u8,
    ) -> nexus_consensus::types::NarwhalCertificate {
        let epoch = EpochNumber(1);
        let batch_digest = Blake3Digest([batch_seed; 32]);
        let origin_idx = ValidatorIndex(origin);
        let round_num = RoundNumber(round);

        let mut builder = CertificateBuilder::new(
            epoch,
            batch_digest,
            origin_idx,
            round_num,
            parents.clone(),
            self.num_validators,
        );

        let payload = cert_signing_payload(epoch, &batch_digest, origin_idx, round_num, &parents)
            .expect("signing payload");

        // Sign with all validators (always meets stake-weighted quorum).
        for (i, (sk, _)) in self.keys.iter().enumerate() {
            let sig = FalconSigner::sign(sk, CERT_DOMAIN, &payload);
            builder.add_signature(ValidatorIndex(i as u32), sig);
        }

        builder.build(self.engine.committee()).expect("build cert")
    }
}

// ── FV-CO-004: CommitSequence strict monotonicity ────────────────────────────

/// For any sequence of committed batches [c₀, c₁, …, cₖ]:
/// - c₀.sequence = 0
/// - c_{i+1}.sequence = c_i.sequence + 1 for all i
#[test]
fn fv_co_004_commit_sequence_strictly_monotonic() {
    let mut h = FvHarness::new(4);
    let mut all_commits = Vec::new();

    // Round 0: genesis certs (pre-verified, no signatures needed).
    let mut prev_digests = Vec::new();
    for v in 0..4u32 {
        let g = h.genesis_cert(v, 10 + v as u8);
        prev_digests.push(g.cert_digest);
        h.engine
            .insert_verified_certificate(g)
            .expect("genesis insert");
    }

    // Rounds 1..7: fully signed certs from all validators.
    for round in 1..8u64 {
        let mut new_digests = Vec::new();
        for v in 0..4u32 {
            let cert = h.build_cert(
                v,
                round,
                prev_digests.clone(),
                (round * 10 + v as u64) as u8,
            );
            new_digests.push(cert.cert_digest);
            let _ = h.engine.process_certificate(cert);
        }
        all_commits.extend(h.engine.take_committed());
        prev_digests = new_digests;
    }

    // Must have committed at least once.
    assert!(
        !all_commits.is_empty(),
        "Expected at least one commit over 7 rounds"
    );

    // First commit starts at 0.
    assert_eq!(all_commits[0].sequence, CommitSequence(0));

    // Strict monotonicity + no gaps.
    for w in all_commits.windows(2) {
        assert_eq!(
            w[1].sequence.0,
            w[0].sequence.0 + 1,
            "CommitSequence gap: {} → {}",
            w[0].sequence.0,
            w[1].sequence.0,
        );
    }
}

// ── FV-CO-006: Quorum threshold formula ──────────────────────────────────────

/// Stake-weighted quorum: ⌊total_stake × 2/3⌋ + 1, for n ∈ [1, 10] equal-stake validators.
///
/// Note: limited range due to Falcon keygen cost (~0.5s per key). The formula
/// correctness is algebraic and does not depend on key material.
#[test]
fn fv_co_006_quorum_threshold_formula() {
    for n in 1u32..=10 {
        let stake_per = 100u64;
        let total_stake = n as u64 * stake_per;
        let expected = Amount(total_stake * 2 / 3 + 1);

        let validators: Vec<_> = (0..n)
            .map(|i| {
                let (_, vk) = FalconSigner::generate_keypair();
                ValidatorInfo {
                    index: ValidatorIndex(i),
                    falcon_pub_key: vk,
                    stake: Amount(stake_per),
                    reputation: ReputationScore::MAX,
                    is_slashed: false,
                    shard_id: None,
                }
            })
            .collect();

        let committee = Committee::new(EpochNumber(0), validators).expect("committee");
        assert_eq!(
            committee.quorum_threshold(),
            expected,
            "n={n}: expected {expected:?}, got {:?}",
            committee.quorum_threshold(),
        );
    }
}

// ── FV-CO-007: Certificate digest domain separation ──────────────────────────

/// compute_cert_digest(CERT_DOMAIN, data) ≠ BLAKE3(other_domain, same_data)
#[test]
fn fv_co_007_cert_digest_domain_separation() {
    let epoch = EpochNumber(0);
    let batch = Blake3Digest([42; 32]);
    let origin = ValidatorIndex(0);
    let round = RoundNumber(0);
    let parents: Vec<CertDigest> = vec![];

    let digest_cert =
        compute_cert_digest(epoch, &batch, origin, round, &parents).expect("cert digest");

    // Manually hash with a wrong domain — must produce a different result.
    let header = bcs::to_bytes(&(epoch, &batch, origin, round, &parents)).expect("bcs");
    let digest_wrong = Blake3Hasher::hash(b"wrong::domain::v1", &header);

    assert_ne!(
        digest_cert, digest_wrong,
        "CERT_DOMAIN did not produce a different digest from an arbitrary domain"
    );
}

// ── FV-CR-001: Domain tag uniqueness (consensus subset) ──────────────────────

/// All consensus domain tags are pairwise distinct.
#[test]
fn fv_cr_001_consensus_domain_tags_unique() {
    let tags: &[(&str, &[u8])] = &[
        ("BATCH_DOMAIN", BATCH_DOMAIN),
        ("CERT_DOMAIN", CERT_DOMAIN),
        ("VOTE_DOMAIN", VOTE_DOMAIN),
    ];

    for i in 0..tags.len() {
        for j in (i + 1)..tags.len() {
            assert_ne!(
                tags[i].1, tags[j].1,
                "Domain tag collision between {} and {}",
                tags[i].0, tags[j].0,
            );
        }
    }
}
