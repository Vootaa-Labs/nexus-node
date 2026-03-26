//! J-9: Stake-weighted certificate build & verify — integration tests.
//!
//! All five acceptance criteria from the roadmap:
//! 1. 7 validators heterogeneous stake → build cert → verify
//! 2. High-stake minority signers meeting threshold → cert valid
//! 3. Low-stake majority signers below threshold → cert invalid
//! 4. Epoch advance → new committee stake-weighted quorum correct
//! 5. Cold restart → restored quorum matches persisted stake

use nexus_consensus::certificate::{cert_signing_payload, CertificateBuilder, CertificateVerifier};
use nexus_consensus::types::{ReputationScore, ValidatorBitset, ValidatorInfo, CERT_DOMAIN};
use nexus_consensus::validator::Committee;
use nexus_consensus::ValidatorRegistry;
use nexus_crypto::{FalconSigner, FalconSigningKey, Signer};
use nexus_primitives::{Amount, Blake3Digest, EpochNumber, RoundNumber, ValidatorIndex};

use crate::fixtures::consensus::TestCommittee;

/// Build a committee + keypairs from heterogeneous stakes.
fn make_hetero_committee(stakes: &[u64], epoch: EpochNumber) -> (Committee, Vec<FalconSigningKey>) {
    let mut keys = Vec::with_capacity(stakes.len());
    let mut validators = Vec::with_capacity(stakes.len());
    for (i, &s) in stakes.iter().enumerate() {
        let (sk, vk) = FalconSigner::generate_keypair();
        validators.push(ValidatorInfo {
            index: ValidatorIndex(i as u32),
            falcon_pub_key: vk,
            stake: Amount(s),
            reputation: ReputationScore::MAX,
            is_slashed: false,
            shard_id: None,
        });
        keys.push(sk);
    }
    let committee = Committee::new(epoch, validators).expect("committee");
    (committee, keys)
}

/// Sign and add a subset of validators (by index) to a builder.
#[allow(clippy::too_many_arguments)]
fn sign_with(
    builder: &mut CertificateBuilder,
    keys: &[FalconSigningKey],
    indices: &[u32],
    epoch: EpochNumber,
    batch_digest: &Blake3Digest,
    origin: ValidatorIndex,
    round: RoundNumber,
    parents: &[nexus_primitives::CertDigest],
) {
    let payload = cert_signing_payload(epoch, batch_digest, origin, round, parents).unwrap();
    for &i in indices {
        let sig = FalconSigner::sign(&keys[i as usize], CERT_DOMAIN, &payload);
        builder.add_signature(ValidatorIndex(i), sig);
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// J-9 Test 1: 7 validators heterogeneous stake → build cert → verify
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn j9_heterogeneous_7_validators_build_and_verify() {
    let stakes = [100, 200, 300, 100, 200, 50, 50]; // total=1000
    let epoch = EpochNumber(1);
    let (committee, keys) = make_hetero_committee(&stakes, epoch);

    // quorum = 1000*2/3+1 = 667
    assert_eq!(committee.quorum_threshold(), Amount(667));

    let batch = Blake3Digest([7u8; 32]);
    let origin = ValidatorIndex(0);
    let round = RoundNumber(1);
    let parents = vec![];

    let mut builder = CertificateBuilder::new(
        epoch,
        batch,
        origin,
        round,
        parents.clone(),
        stakes.len() as u32,
    );

    // Sign with validators 1(200), 2(300), 4(200) = 700 >= 667 ✓
    sign_with(
        &mut builder,
        &keys,
        &[1, 2, 4],
        epoch,
        &batch,
        origin,
        round,
        &parents,
    );

    let cert = builder.build(&committee).unwrap();
    CertificateVerifier::verify(&cert, &committee, epoch).unwrap();
}

// ═════════════════════════════════════════════════════════════════════════════
// J-9 Test 2: High-stake minority signers meeting threshold → cert valid
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn j9_high_stake_minority_meets_threshold() {
    // 5 validators: one whale (700) + four smalls (100 each)
    // total = 1100, quorum = 1100*2/3+1 = 734
    let stakes = [700, 100, 100, 100, 100];
    let epoch = EpochNumber(1);
    let (committee, keys) = make_hetero_committee(&stakes, epoch);
    assert_eq!(committee.quorum_threshold(), Amount(734));

    let batch = Blake3Digest([2u8; 32]);
    let origin = ValidatorIndex(0);
    let round = RoundNumber(1);
    let parents = vec![];

    let mut builder = CertificateBuilder::new(
        epoch,
        batch,
        origin,
        round,
        parents.clone(),
        stakes.len() as u32,
    );

    // Whale (700) + one small (100) = 800 >= 734 ✓  (only 2 of 5 = minority)
    sign_with(
        &mut builder,
        &keys,
        &[0, 1],
        epoch,
        &batch,
        origin,
        round,
        &parents,
    );

    let cert = builder.build(&committee).unwrap();
    CertificateVerifier::verify(&cert, &committee, epoch).unwrap();
}

// ═════════════════════════════════════════════════════════════════════════════
// J-9 Test 3: Low-stake majority signers below threshold → cert invalid
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn j9_low_stake_majority_below_threshold() {
    // Same distribution: whale (700) + four smalls (100 each)
    // total=1100, quorum=734
    let stakes = [700, 100, 100, 100, 100];
    let epoch = EpochNumber(1);
    let (committee, keys) = make_hetero_committee(&stakes, epoch);
    assert_eq!(committee.quorum_threshold(), Amount(734));

    let batch = Blake3Digest([3u8; 32]);
    let origin = ValidatorIndex(0);
    let round = RoundNumber(1);
    let parents = vec![];

    let mut builder = CertificateBuilder::new(
        epoch,
        batch,
        origin,
        round,
        parents.clone(),
        stakes.len() as u32,
    );

    // Four small validators (4 of 5 = majority by count) = 400 < 734 ✗
    sign_with(
        &mut builder,
        &keys,
        &[1, 2, 3, 4],
        epoch,
        &batch,
        origin,
        round,
        &parents,
    );

    let result = builder.build(&committee);
    assert!(
        result.is_err(),
        "4-of-5 signers with only 400/1100 stake must fail"
    );
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("insufficient signer stake"),
        "expected InsufficientSignatures, got: {err}"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// J-9 Test 4: Epoch advance → new committee stake-weighted quorum correct
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn j9_epoch_advance_quorum_correct() {
    // Epoch 0: 4 equal-stake validators
    let tc0 = TestCommittee::new(4, EpochNumber(0));
    // total=4000, quorum=2667
    assert_eq!(tc0.committee.quorum_threshold(), Amount(2667));

    // Epoch 1: heterogeneous stakes — simulate governance rebalance
    let tc1 = TestCommittee::new_heterogeneous(&[500, 300, 100, 100], EpochNumber(1));
    // total=1000, quorum=667
    assert_eq!(tc1.committee.quorum_threshold(), Amount(667));

    // Build a cert in epoch 1 using the new heterogeneous committee.
    let batch = Blake3Digest([4u8; 32]);
    let origin = ValidatorIndex(0);
    let round = RoundNumber(0);
    let parents = vec![];

    let cert = tc1.build_cert(batch, origin, round, parents);
    CertificateVerifier::verify(&cert, &tc1.committee, EpochNumber(1)).unwrap();

    // Cross-epoch: verifying epoch-1 cert against epoch-0 committee must fail.
    let result = CertificateVerifier::verify(&cert, &tc0.committee, EpochNumber(0));
    assert!(result.is_err(), "epoch mismatch must be detected");
}

// ═════════════════════════════════════════════════════════════════════════════
// J-9 Test 5: Cold restart → restored quorum matches persisted stake
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn j9_cold_restart_quorum_matches_persisted() {
    let stakes = [500, 300, 100, 100];
    let epoch = EpochNumber(2);
    let tc = TestCommittee::new_heterogeneous(&stakes, epoch);

    let original_quorum = tc.committee.quorum_threshold();
    let original_total = tc.committee.total_stake();

    // Persist the committee.
    let snapshot = tc.committee.to_persistent();

    // "Crash" — drop original committee.
    drop(tc);

    // Reconstruct from snapshot (simulates cold restart).
    let restored = Committee::from_persistent(snapshot).expect("restore");

    assert_eq!(restored.quorum_threshold(), original_quorum);
    assert_eq!(restored.total_stake(), original_total);
    assert_eq!(restored.epoch(), epoch);

    // Verify the restored committee can correctly evaluate quorum.
    // 500 + 300 = 800 >= 667  (total=1000, quorum=667)
    let mut bs = ValidatorBitset::new(stakes.len() as u32);
    bs.set(ValidatorIndex(0)); // 500
    bs.set(ValidatorIndex(1)); // 300
    assert!(restored.is_quorum(&bs));

    // 300 + 100 + 100 = 500 < 667
    let mut bs2 = ValidatorBitset::new(stakes.len() as u32);
    bs2.set(ValidatorIndex(1)); // 300
    bs2.set(ValidatorIndex(2)); // 100
    bs2.set(ValidatorIndex(3)); // 100
    assert!(!restored.is_quorum(&bs2));
}
