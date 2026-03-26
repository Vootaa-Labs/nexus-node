// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Consensus test fixtures.
//!
//! Provides helpers for building committees, certificates, and DAGs
//! for integration tests.

use nexus_consensus::types::{NarwhalCertificate, ReputationScore, ValidatorBitset, ValidatorInfo};
use nexus_consensus::{Committee, ConsensusEngine};
use nexus_crypto::{FalconSigner, FalconSigningKey, FalconVerifyKey, Signer};
use nexus_primitives::{
    Amount, BatchDigest, Blake3Digest, CertDigest, EpochNumber, RoundNumber, ShardId,
    ValidatorIndex,
};

/// A test committee with pre-generated signing keys.
pub struct TestCommittee {
    /// Signing keys for each validator (index-aligned).
    pub signing_keys: Vec<FalconSigningKey>,
    /// Verify keys for each validator (index-aligned).
    pub verify_keys: Vec<FalconVerifyKey>,
    /// The committee instance.
    pub committee: Committee,
    /// Epoch number.
    pub epoch: EpochNumber,
}

impl TestCommittee {
    /// Create a test committee with `n` validators, each with equal stake.
    pub fn new(n: usize, epoch: EpochNumber) -> Self {
        let mut signing_keys = Vec::with_capacity(n);
        let mut verify_keys = Vec::with_capacity(n);
        let mut validators = Vec::with_capacity(n);

        for i in 0..n {
            let (sk, vk) = FalconSigner::generate_keypair();
            validators.push(ValidatorInfo {
                index: ValidatorIndex(i as u32),
                falcon_pub_key: vk.clone(),
                stake: Amount(1_000),
                reputation: ReputationScore::default(),
                is_slashed: false,
                shard_id: Some(ShardId(0)),
            });
            signing_keys.push(sk);
            verify_keys.push(vk);
        }

        let committee = Committee::new(epoch, validators).expect("test committee");
        Self {
            signing_keys,
            verify_keys,
            committee,
            epoch,
        }
    }

    /// Build a properly signed certificate for a given batch/round/origin.
    ///
    /// Signs with all validators (i.e., full quorum).
    pub fn build_cert(
        &self,
        batch_digest: BatchDigest,
        origin: ValidatorIndex,
        round: RoundNumber,
        parents: Vec<CertDigest>,
    ) -> NarwhalCertificate {
        let cert_digest = nexus_consensus::compute_cert_digest(
            self.epoch,
            &batch_digest,
            origin,
            round,
            &parents,
        )
        .expect("cert digest computation should not fail");

        let signing_payload = nexus_consensus::certificate::cert_signing_payload(
            self.epoch,
            &batch_digest,
            origin,
            round,
            &parents,
        )
        .expect("cert signing payload computation should not fail");

        let n = self.signing_keys.len();
        let mut signatures = Vec::with_capacity(n);
        let mut signers = ValidatorBitset::new(n as u32);

        for (i, sk) in self.signing_keys.iter().enumerate() {
            let sig = FalconSigner::sign(sk, nexus_consensus::types::CERT_DOMAIN, &signing_payload);
            signatures.push((ValidatorIndex(i as u32), sig));
            signers.set(nexus_primitives::ValidatorIndex(i as u32));
        }

        NarwhalCertificate {
            epoch: self.epoch,
            batch_digest,
            origin,
            round,
            parents,
            signatures,
            signers,
            cert_digest,
        }
    }

    /// Build a genesis certificate (round 0, no parents) for a validator.
    pub fn genesis_cert(&self, origin: ValidatorIndex) -> NarwhalCertificate {
        self.build_cert(
            Blake3Digest([origin.0 as u8; 32]),
            origin,
            RoundNumber(0),
            vec![],
        )
    }

    /// Create a test committee with heterogeneous (non-equal) stakes.
    ///
    /// `stakes` provides the stake value for each validator (one entry per validator).
    pub fn new_heterogeneous(stakes: &[u64], epoch: EpochNumber) -> Self {
        let n = stakes.len();
        let mut signing_keys = Vec::with_capacity(n);
        let mut verify_keys = Vec::with_capacity(n);
        let mut validators = Vec::with_capacity(n);

        for (i, &stake) in stakes.iter().enumerate() {
            let (sk, vk) = FalconSigner::generate_keypair();
            validators.push(ValidatorInfo {
                index: ValidatorIndex(i as u32),
                falcon_pub_key: vk.clone(),
                stake: Amount(stake),
                reputation: ReputationScore::default(),
                is_slashed: false,
                shard_id: Some(ShardId(0)),
            });
            signing_keys.push(sk);
            verify_keys.push(vk);
        }

        let committee = Committee::new(epoch, validators).expect("test committee");
        Self {
            signing_keys,
            verify_keys,
            committee,
            epoch,
        }
    }

    /// Create a `ConsensusEngine` from this test committee.
    pub fn into_engine(self) -> (ConsensusEngine, Vec<FalconSigningKey>, Vec<FalconVerifyKey>) {
        let engine = ConsensusEngine::new(self.epoch, self.committee);
        (engine, self.signing_keys, self.verify_keys)
    }
}
