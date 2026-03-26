//! Proptest-based formal verification — Consensus Layer
//!
//! Strengthens the deterministic tests in `fv_property_tests.rs` with
//! randomised property-based testing via the `proptest` framework.
//!
//! # Invariants Covered
//! - FV-CO-004: CommitSequence strict monotonicity (random validator counts & rounds)
//! - FV-CO-006: Quorum threshold formula 2f+1 for arbitrary n
//! - FV-CO-007: Certificate digest domain separation (random payloads)
//! - FV-CR-001: Domain separation tag pairwise uniqueness

use nexus_consensus::types::{
    ReputationScore, ValidatorBitset, ValidatorInfo, BATCH_DOMAIN, CERT_DOMAIN, VOTE_DOMAIN,
};
use nexus_consensus::validator::Committee;
use nexus_consensus::{compute_cert_digest, ConsensusEngine, ValidatorRegistry};
use nexus_crypto::{Blake3Hasher, CryptoHasher, FalconSigner, Signer};
use nexus_primitives::{Amount, Blake3Digest, EpochNumber, RoundNumber, ValidatorIndex};
use proptest::prelude::*;

// ── FV-CO-006: Quorum threshold = 2⌊(n-1)/3⌋ + 1 ──────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(15))]

    /// For any validator count n ∈ [1, 15], quorum_threshold must equal
    /// the stake-weighted formula ⌊total_stake × 2/3⌋ + 1.
    ///
    /// Falcon keygen is expensive (~0.5s per key), so we limit the range
    /// while still covering 50% more cases than the deterministic test.
    #[test]
    fn fv_co_006_quorum_formula_wide_range(n in 1u32..=15) {
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

        let committee = Committee::new(EpochNumber(0), validators).unwrap();
        prop_assert_eq!(committee.quorum_threshold(), expected);
    }
}

// ── FV-CO-007: Domain separation with random payloads ───────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    /// For any random batch digest and round number, computing the cert
    /// digest under CERT_DOMAIN must differ from hashing the same payload
    /// under a different domain tag.
    #[test]
    fn fv_co_007_domain_separation_random(
        batch_bytes in prop::array::uniform32(any::<u8>()),
        round in 0u64..10_000,
        origin in 0u32..16,
    ) {
        let epoch = EpochNumber(1);
        let batch = Blake3Digest(batch_bytes);
        let origin_idx = ValidatorIndex(origin);
        let round_num = RoundNumber(round);
        let parents = vec![];

        let cert_digest =
            compute_cert_digest(epoch, &batch, origin_idx, round_num, &parents).unwrap();

        // Same payload, wrong domain.
        let header =
            bcs::to_bytes(&(epoch, &batch, origin_idx, round_num, &parents)).unwrap();
        let wrong_digest = Blake3Hasher::hash(b"wrong::domain::v1", &header);

        prop_assert_ne!(cert_digest, wrong_digest);
    }
}

// ── FV-CR-001: Domain tags pairwise distinct ────────────────────────────

#[test]
fn fv_cr_001_all_domain_tags_pairwise_distinct() {
    let tags: &[(&str, &[u8])] = &[
        ("BATCH_DOMAIN", BATCH_DOMAIN),
        ("CERT_DOMAIN", CERT_DOMAIN),
        ("VOTE_DOMAIN", VOTE_DOMAIN),
    ];
    for i in 0..tags.len() {
        for j in (i + 1)..tags.len() {
            assert_ne!(
                tags[i].1, tags[j].1,
                "collision: {} vs {}",
                tags[i].0, tags[j].0
            );
        }
    }
}

// ── FV-CO-004: CommitSequence monotonicity (parameterised) ──────────────

/// Build a ConsensusEngine with `n` validators, run `rounds` rounds,
/// and verify that committed sequence numbers form a gap-free monotonic
/// sequence starting at 0.
///
/// This is a helper rather than a `proptest!` macro test because Falcon
/// keygen is expensive (~0.5s per key). We parameterise manually over a
/// few interesting validator counts.
fn assert_commit_monotonicity(n: u32, rounds: u64) {
    use nexus_consensus::certificate::{cert_signing_payload, CertificateBuilder};
    use nexus_consensus::types::ValidatorBitset;
    use nexus_crypto::Signer;
    use nexus_primitives::{CertDigest, CommitSequence};

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
    let committee = Committee::new(EpochNumber(1), validators).unwrap();
    let mut engine = ConsensusEngine::new(EpochNumber(1), committee);

    let mut prev_digests: Vec<CertDigest> = Vec::new();
    for v in 0..n {
        let epoch = EpochNumber(1);
        let batch_digest = Blake3Digest([10 + v as u8; 32]);
        let origin_idx = ValidatorIndex(v);
        let round = RoundNumber(0);
        let parents = vec![];
        let cert_digest =
            nexus_consensus::compute_cert_digest(epoch, &batch_digest, origin_idx, round, &parents)
                .unwrap();
        let g = nexus_consensus::types::NarwhalCertificate {
            epoch,
            batch_digest,
            origin: origin_idx,
            round,
            parents,
            signatures: vec![],
            signers: ValidatorBitset::new(n),
            cert_digest,
        };
        prev_digests.push(g.cert_digest);
        engine.insert_verified_certificate(g).unwrap();
    }

    let mut all_commits = Vec::new();
    for round in 1..=rounds {
        let mut new_digests = Vec::new();
        for v in 0..n {
            let epoch = EpochNumber(1);
            let batch_digest = Blake3Digest([(round * 10 + v as u64) as u8; 32]);
            let origin_idx = ValidatorIndex(v);
            let round_num = RoundNumber(round);

            let mut builder = CertificateBuilder::new(
                epoch,
                batch_digest,
                origin_idx,
                round_num,
                prev_digests.clone(),
                n,
            );

            let payload =
                cert_signing_payload(epoch, &batch_digest, origin_idx, round_num, &prev_digests)
                    .unwrap();

            // Sign with all validators (always meets stake-weighted quorum).
            for (i, (sk, _)) in keys.iter().enumerate() {
                let sig = FalconSigner::sign(sk, CERT_DOMAIN, &payload);
                builder.add_signature(ValidatorIndex(i as u32), sig);
            }

            let cert = builder.build(engine.committee()).unwrap();
            new_digests.push(cert.cert_digest);
            let _ = engine.process_certificate(cert);
        }
        all_commits.extend(engine.take_committed());
        prev_digests = new_digests;
    }

    assert!(!all_commits.is_empty(), "n={n} rounds={rounds}: no commits");
    assert_eq!(all_commits[0].sequence, CommitSequence(0));
    for w in all_commits.windows(2) {
        assert_eq!(
            w[1].sequence.0,
            w[0].sequence.0 + 1,
            "n={n}: gap at {} → {}",
            w[0].sequence.0,
            w[1].sequence.0,
        );
    }
}

#[test]
fn fv_co_004_monotonicity_4_validators() {
    assert_commit_monotonicity(4, 7);
}

#[test]
fn fv_co_004_monotonicity_7_validators() {
    assert_commit_monotonicity(7, 4);
}

// ── J-8: Stake-weighted quorum property tests ───────────────────────────

/// Helper: build a committee from a vector of heterogeneous stake values.
fn heterogeneous_committee(stakes: &[u64]) -> Committee {
    let validators: Vec<_> = stakes
        .iter()
        .enumerate()
        .map(|(i, &s)| {
            let (_, vk) = FalconSigner::generate_keypair();
            ValidatorInfo {
                index: ValidatorIndex(i as u32),
                falcon_pub_key: vk,
                stake: Amount(s),
                reputation: ReputationScore::MAX,
                is_slashed: false,
                shard_id: None,
            }
        })
        .collect();
    Committee::new(EpochNumber(0), validators).expect("heterogeneous committee")
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    /// J-8 Property 1: For arbitrary heterogeneous stakes, quorum = ⌊total_stake × 2/3⌋ + 1.
    #[test]
    fn j8_heterogeneous_quorum_formula(
        stakes in prop::collection::vec(1u64..10_000, 1..=8)
    ) {
        let total: u64 = stakes.iter().sum();
        let expected = Amount(total * 2 / 3 + 1);
        let committee = heterogeneous_committee(&stakes);
        prop_assert_eq!(committee.quorum_threshold(), expected);
        prop_assert_eq!(committee.total_stake(), Amount(total));
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(30))]

    /// J-8 Property 2: Exactly meeting quorum threshold → is_quorum() returns true.
    #[test]
    fn j8_exactly_at_threshold_is_quorum(
        stakes in prop::collection::vec(100u64..1_000, 3..=7)
    ) {
        let committee = heterogeneous_committee(&stakes);
        let threshold = committee.quorum_threshold().0;

        // Sort indices by descending stake so we can accumulate to reach threshold exactly.
        let mut indexed: Vec<(usize, u64)> = stakes.iter().enumerate().map(|(i, &s)| (i, s)).collect();
        indexed.sort_by(|a, b| b.1.cmp(&a.1));

        let mut bs = ValidatorBitset::new(stakes.len() as u32);
        let mut accumulated = 0u64;
        for &(idx, stake) in &indexed {
            if accumulated >= threshold {
                break;
            }
            bs.set(ValidatorIndex(idx as u32));
            accumulated += stake;
        }
        // Once accumulated >= threshold, is_quorum must be true.
        prop_assert!(committee.is_quorum(&bs), "accumulated {} >= threshold {}", accumulated, threshold);
    }

    /// J-8 Property 3: Accumulated stake 1 below threshold → is_quorum() returns false.
    #[test]
    fn j8_one_below_threshold_not_quorum(
        stakes in prop::collection::vec(100u64..1_000, 3..=7)
    ) {
        let committee = heterogeneous_committee(&stakes);
        let threshold = committee.quorum_threshold().0;

        // Sort indices by ascending stake so the smallest validators come first
        // and we can stay below threshold.
        let mut indexed: Vec<(usize, u64)> = stakes.iter().enumerate().map(|(i, &s)| (i, s)).collect();
        indexed.sort_by_key(|&(_, s)| s);

        let mut bs = ValidatorBitset::new(stakes.len() as u32);
        let mut accumulated = 0u64;
        for &(idx, stake) in &indexed {
            if accumulated + stake >= threshold {
                break;
            }
            bs.set(ValidatorIndex(idx as u32));
            accumulated += stake;
        }
        // If we managed to stay below threshold, is_quorum must be false.
        if accumulated < threshold {
            prop_assert!(!committee.is_quorum(&bs), "accumulated {} < threshold {} but is_quorum true", accumulated, threshold);
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(20))]

    /// J-8 Property 4: Slashing the highest-stake validator updates total_stake and quorum correctly.
    #[test]
    fn j8_slash_high_stake_updates_quorum(
        stakes in prop::collection::vec(100u64..5_000, 3..=7)
    ) {
        let mut committee = heterogeneous_committee(&stakes);
        let total_before: u64 = stakes.iter().sum();

        // Find the validator with the highest stake.
        let max_idx = stakes
            .iter()
            .enumerate()
            .max_by_key(|&(_, &s)| s)
            .map(|(i, _)| i)
            .unwrap();
        let max_stake = stakes[max_idx];

        committee.slash(ValidatorIndex(max_idx as u32)).unwrap();

        let new_total = total_before - max_stake;
        let new_quorum = new_total * 2 / 3 + 1;
        prop_assert_eq!(committee.total_stake(), Amount(new_total));
        prop_assert_eq!(committee.quorum_threshold(), Amount(new_quorum));
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(15))]

    /// J-8 Property 5: Equal stakes → stake-weighted quorum behaves equivalently to old 2f+1.
    ///
    /// With equal stake `s` per validator and `n` validators, the old count-based
    /// quorum was `2⌊(n-1)/3⌋ + 1`. This is equivalent to needing that many signers,
    /// i.e. the minimum number of signers `k` such that `k * s >= ⌊n*s*2/3⌋ + 1`.
    #[test]
    fn j8_equal_stake_equivalent_to_count_based(n in 1u32..=10) {
        let stake_per = 100u64;
        let total = n as u64 * stake_per;
        let weighted_quorum = total * 2 / 3 + 1;

        // Minimum signers needed under stake-weighted:
        // k such that k * stake_per >= weighted_quorum
        let min_signers = weighted_quorum.div_ceil(stake_per);

        let committee = heterogeneous_committee(
            &vec![stake_per; n as usize],
        );

        // With min_signers - 1 signers → must NOT be quorum
        if min_signers > 1 {
            let mut bs = ValidatorBitset::new(n);
            for i in 0..(min_signers - 1) as u32 {
                bs.set(ValidatorIndex(i));
            }
            prop_assert!(!committee.is_quorum(&bs));
        }

        // With min_signers → must be quorum
        if min_signers <= n as u64 {
            let mut bs = ValidatorBitset::new(n);
            for i in 0..min_signers as u32 {
                bs.set(ValidatorIndex(i));
            }
            prop_assert!(committee.is_quorum(&bs));
        }
    }
}
