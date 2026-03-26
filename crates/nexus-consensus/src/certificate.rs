// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Narwhal certificate construction and verification.
//!
//! [`CertificateBuilder`] accumulates validator signatures and produces a
//! [`NarwhalCertificate`] once a quorum (2f+1) is reached.
//!
//! [`CertificateVerifier`] checks that an existing certificate has valid
//! signatures from a quorum of the committee.

use crate::error::{ConsensusError, ConsensusResult};
use crate::traits::ValidatorRegistry;
use crate::types::{NarwhalCertificate, ValidatorBitset, CERT_DOMAIN};
use nexus_crypto::{Blake3Hasher, FalconSignature, FalconSigner, Signer};
use nexus_primitives::{BatchDigest, CertDigest, EpochNumber, RoundNumber, ValidatorIndex};

/// Compute the certificate digest over its header fields.
///
/// `BLAKE3( CERT_DOMAIN ‖ BCS(epoch, batch_digest, origin, round, parents) )`
pub fn compute_cert_digest(
    epoch: EpochNumber,
    batch_digest: &BatchDigest,
    origin: ValidatorIndex,
    round: RoundNumber,
    parents: &[CertDigest],
) -> ConsensusResult<CertDigest> {
    let header = bcs::to_bytes(&(epoch, batch_digest, origin, round, parents))
        .map_err(|e| ConsensusError::Codec(e.to_string()))?;
    Ok(Blake3Hasher::digest(CERT_DOMAIN, &header))
}

/// Compute the signing payload for a certificate (what validators sign).
///
/// Returns the BCS-serialized header bytes used for signing and verification.
pub fn cert_signing_payload(
    epoch: EpochNumber,
    batch_digest: &BatchDigest,
    origin: ValidatorIndex,
    round: RoundNumber,
    parents: &[CertDigest],
) -> ConsensusResult<Vec<u8>> {
    bcs::to_bytes(&(epoch, batch_digest, origin, round, parents))
        .map_err(|e| ConsensusError::Codec(e.to_string()))
}

/// Builder for accumulating signatures into a Narwhal certificate.
///
/// Collects individual validator signatures over a batch header.
/// Once the quorum threshold is met, [`build`](CertificateBuilder::build)
/// produces the final [`NarwhalCertificate`].
#[derive(Debug)]
pub struct CertificateBuilder {
    epoch: EpochNumber,
    batch_digest: BatchDigest,
    origin: ValidatorIndex,
    round: RoundNumber,
    parents: Vec<CertDigest>,
    signatures: Vec<(ValidatorIndex, FalconSignature)>,
    signers: ValidatorBitset,
}

impl CertificateBuilder {
    /// Start building a certificate for the given batch header.
    pub fn new(
        epoch: EpochNumber,
        batch_digest: BatchDigest,
        origin: ValidatorIndex,
        round: RoundNumber,
        parents: Vec<CertDigest>,
        num_validators: u32,
    ) -> Self {
        Self {
            epoch,
            batch_digest,
            origin,
            round,
            parents,
            signatures: Vec::new(),
            signers: ValidatorBitset::new(num_validators),
        }
    }

    /// Add a validator's signature.
    ///
    /// Returns `true` if the signature was new, `false` if the validator
    /// already signed (duplicate is silently ignored).
    pub fn add_signature(&mut self, validator: ValidatorIndex, signature: FalconSignature) -> bool {
        if self.signers.is_set(validator) {
            return false;
        }
        self.signers.set(validator);
        self.signatures.push((validator, signature));
        true
    }

    /// Number of unique signatures collected so far.
    pub fn signature_count(&self) -> u32 {
        self.signers.count()
    }

    /// Read-only access to the signer bitset (for external quorum checks).
    pub fn signers(&self) -> &ValidatorBitset {
        &self.signers
    }

    /// Finalize the certificate if the stake-weighted quorum threshold is met.
    ///
    /// # Errors
    ///
    /// Returns [`ConsensusError::InsufficientSignatures`] if the accumulated
    /// signer stake is below the committee's quorum threshold.
    pub fn build<R: ValidatorRegistry>(self, registry: &R) -> ConsensusResult<NarwhalCertificate> {
        if !registry.is_quorum(&self.signers) {
            let signer_stake: u64 = self
                .signatures
                .iter()
                .filter_map(|(idx, _)| registry.validator_info(*idx))
                .filter(|v| !v.is_slashed)
                .map(|v| v.stake.0)
                .sum();
            return Err(ConsensusError::InsufficientSignatures {
                required: registry.quorum_threshold().0,
                got: signer_stake,
            });
        }

        let cert_digest = compute_cert_digest(
            self.epoch,
            &self.batch_digest,
            self.origin,
            self.round,
            &self.parents,
        )?;

        Ok(NarwhalCertificate {
            epoch: self.epoch,
            batch_digest: self.batch_digest,
            origin: self.origin,
            round: self.round,
            parents: self.parents,
            signatures: self.signatures,
            signers: self.signers,
            cert_digest,
        })
    }
}

/// Verify that a certificate has valid stake-weighted quorum signatures from the committee.
pub struct CertificateVerifier;

impl CertificateVerifier {
    /// Verify a certificate against the given committee.
    ///
    /// Checks:
    /// 1. The certificate epoch matches `expected_epoch`.
    /// 2. The cert digest matches the header fields.
    /// 3. Active signers' combined stake meets the quorum threshold.
    /// 4. Each signature is valid under the signer's FALCON-512 public key.
    ///
    /// # Errors
    ///
    /// - [`ConsensusError::EpochMismatch`] if cert epoch ≠ expected epoch.
    /// - [`ConsensusError::Codec`] if digest re-computation fails.
    /// - [`ConsensusError::InsufficientSignatures`] if signer stake below quorum.
    /// - [`ConsensusError::InvalidSignature`] if any signature fails verification.
    /// - [`ConsensusError::UnknownValidator`] if a signer is not in the committee.
    pub fn verify<R: ValidatorRegistry>(
        cert: &NarwhalCertificate,
        registry: &R,
        expected_epoch: EpochNumber,
    ) -> ConsensusResult<()> {
        // 1. Verify epoch.
        if cert.epoch != expected_epoch {
            return Err(ConsensusError::EpochMismatch {
                expected: expected_epoch,
                got: cert.epoch,
            });
        }

        // 2. Verify cert digest integrity.
        let expected = compute_cert_digest(
            cert.epoch,
            &cert.batch_digest,
            cert.origin,
            cert.round,
            &cert.parents,
        )?;
        if expected != cert.cert_digest {
            return Err(ConsensusError::Codec(
                "certificate digest mismatch".to_string(),
            ));
        }

        // 2. Check stake-weighted quorum.
        if !registry.is_quorum(&cert.signers) {
            let signer_stake: u64 = cert
                .signatures
                .iter()
                .filter_map(|(idx, _)| registry.validator_info(*idx))
                .filter(|v| !v.is_slashed)
                .map(|v| v.stake.0)
                .sum();
            return Err(ConsensusError::InsufficientSignatures {
                required: registry.quorum_threshold().0,
                got: signer_stake,
            });
        }

        // 3. Verify each signature.
        let payload = cert_signing_payload(
            cert.epoch,
            &cert.batch_digest,
            cert.origin,
            cert.round,
            &cert.parents,
        )?;

        for (validator_idx, sig) in &cert.signatures {
            let info = registry
                .validator_info(*validator_idx)
                .ok_or(ConsensusError::UnknownValidator(*validator_idx))?;

            FalconSigner::verify(&info.falcon_pub_key, CERT_DOMAIN, &payload, sig).map_err(
                |source| ConsensusError::InvalidSignature {
                    validator: *validator_idx,
                    source,
                },
            )?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ReputationScore;
    use crate::types::ValidatorInfo;
    use crate::validator::Committee;
    use nexus_crypto::{FalconVerifyKey, Signer};
    use nexus_primitives::{Amount, Blake3Digest};

    /// Create a committee with keypairs for testing.
    fn make_test_committee(
        n: u32,
    ) -> (
        Committee,
        Vec<(nexus_crypto::FalconSigningKey, FalconVerifyKey)>,
    ) {
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
        (
            Committee::new(EpochNumber(1), validators).expect("test committee"),
            keys,
        )
    }

    fn sign_cert_header(
        sk: &nexus_crypto::FalconSigningKey,
        epoch: EpochNumber,
        batch_digest: &BatchDigest,
        origin: ValidatorIndex,
        round: RoundNumber,
        parents: &[CertDigest],
    ) -> FalconSignature {
        let payload = cert_signing_payload(epoch, batch_digest, origin, round, parents).unwrap();
        FalconSigner::sign(sk, CERT_DOMAIN, &payload)
    }

    // ── CertificateBuilder tests ────────────────────────────────────────

    #[test]
    fn builder_collects_signatures() {
        let (committee, keys) = make_test_committee(4);
        let batch_digest = Blake3Digest([1u8; 32]);
        let epoch = EpochNumber(1);
        let origin = ValidatorIndex(0);
        let round = RoundNumber(1);
        let parents = vec![];

        let mut builder =
            CertificateBuilder::new(epoch, batch_digest, origin, round, parents.clone(), 4);

        for (i, (sk, _vk)) in keys.iter().enumerate().take(3) {
            let sig = sign_cert_header(sk, epoch, &batch_digest, origin, round, &parents);
            assert!(builder.add_signature(ValidatorIndex(i as u32), sig));
        }

        assert_eq!(builder.signature_count(), 3);
        let cert = builder.build(&committee).unwrap();
        assert_eq!(cert.signatures.len(), 3);
        assert_eq!(cert.origin, origin);
        assert_eq!(cert.round, round);
    }

    #[test]
    fn builder_rejects_duplicate_signer() {
        let (_committee, keys) = make_test_committee(4);
        let batch_digest = Blake3Digest([1u8; 32]);
        let epoch = EpochNumber(1);
        let origin = ValidatorIndex(0);
        let round = RoundNumber(0);
        let parents = vec![];

        let mut builder =
            CertificateBuilder::new(epoch, batch_digest, origin, round, parents.clone(), 4);
        let sig = sign_cert_header(&keys[0].0, epoch, &batch_digest, origin, round, &parents);
        assert!(builder.add_signature(ValidatorIndex(0), sig.clone()));
        assert!(!builder.add_signature(ValidatorIndex(0), sig));
        assert_eq!(builder.signature_count(), 1);
    }

    #[test]
    fn builder_fails_below_quorum() {
        let (committee, keys) = make_test_committee(4);
        // total_stake=400, quorum=267. Each validator stakes 100.
        let batch_digest = Blake3Digest([1u8; 32]);
        let epoch = EpochNumber(1);
        let origin = ValidatorIndex(0);
        let round = RoundNumber(0);
        let parents = vec![];

        let mut builder =
            CertificateBuilder::new(epoch, batch_digest, origin, round, parents.clone(), 4);
        // Only 2 signatures → signer stake = 200 < 267.
        for (i, (sk, _)) in keys.iter().enumerate().take(2) {
            let sig = sign_cert_header(sk, epoch, &batch_digest, origin, round, &parents);
            builder.add_signature(ValidatorIndex(i as u32), sig);
        }

        let result = builder.build(&committee);
        assert!(matches!(
            result,
            Err(ConsensusError::InsufficientSignatures {
                required: 267,
                got: 200
            })
        ));
    }

    // ── CertificateVerifier tests ───────────────────────────────────────

    #[test]
    fn verify_valid_certificate() {
        let (committee, keys) = make_test_committee(4);
        let batch_digest = Blake3Digest([1u8; 32]);
        let epoch = EpochNumber(1);
        let origin = ValidatorIndex(0);
        let round = RoundNumber(0);
        let parents = vec![];

        let mut builder =
            CertificateBuilder::new(epoch, batch_digest, origin, round, parents.clone(), 4);
        for (i, (sk, _)) in keys.iter().enumerate().take(3) {
            let sig = sign_cert_header(sk, epoch, &batch_digest, origin, round, &parents);
            builder.add_signature(ValidatorIndex(i as u32), sig);
        }
        let cert = builder.build(&committee).unwrap();

        CertificateVerifier::verify(&cert, &committee, EpochNumber(1)).unwrap();
    }

    #[test]
    fn verify_rejects_tampered_digest() {
        let (committee, keys) = make_test_committee(4);
        let batch_digest = Blake3Digest([1u8; 32]);
        let epoch = EpochNumber(1);
        let origin = ValidatorIndex(0);
        let round = RoundNumber(0);
        let parents = vec![];

        let mut builder =
            CertificateBuilder::new(epoch, batch_digest, origin, round, parents.clone(), 4);
        for (i, (sk, _)) in keys.iter().enumerate().take(3) {
            let sig = sign_cert_header(sk, epoch, &batch_digest, origin, round, &parents);
            builder.add_signature(ValidatorIndex(i as u32), sig);
        }
        let mut cert = builder.build(&committee).unwrap();
        // Tamper with the digest.
        cert.cert_digest = Blake3Digest([99u8; 32]);

        let result = CertificateVerifier::verify(&cert, &committee, EpochNumber(1));
        assert!(matches!(result, Err(ConsensusError::Codec(_))));
    }

    #[test]
    fn verify_rejects_wrong_signature() {
        let (committee, keys) = make_test_committee(4);
        let batch_digest = Blake3Digest([1u8; 32]);
        let epoch = EpochNumber(1);
        let origin = ValidatorIndex(0);
        let round = RoundNumber(0);
        let parents = vec![];

        let mut builder =
            CertificateBuilder::new(epoch, batch_digest, origin, round, parents.clone(), 4);

        // First two sign correctly.
        for (i, (sk, _)) in keys.iter().enumerate().take(2) {
            let sig = sign_cert_header(sk, epoch, &batch_digest, origin, round, &parents);
            builder.add_signature(ValidatorIndex(i as u32), sig);
        }

        // Third uses wrong key (sign with key[3] but claim to be validator 2).
        let wrong_sig = sign_cert_header(&keys[3].0, epoch, &batch_digest, origin, round, &parents);
        builder.add_signature(ValidatorIndex(2), wrong_sig);

        let cert = builder.build(&committee).unwrap();
        let result = CertificateVerifier::verify(&cert, &committee, EpochNumber(1));
        assert!(matches!(
            result,
            Err(ConsensusError::InvalidSignature { .. })
        ));
    }

    // ── Digest computation tests ────────────────────────────────────────

    #[test]
    fn cert_digest_is_deterministic() {
        let d1 = compute_cert_digest(
            EpochNumber(1),
            &Blake3Digest([1; 32]),
            ValidatorIndex(0),
            RoundNumber(1),
            &[],
        )
        .unwrap();
        let d2 = compute_cert_digest(
            EpochNumber(1),
            &Blake3Digest([1; 32]),
            ValidatorIndex(0),
            RoundNumber(1),
            &[],
        )
        .unwrap();
        assert_eq!(d1, d2);
    }

    #[test]
    fn cert_digest_changes_with_different_inputs() {
        let d1 = compute_cert_digest(
            EpochNumber(1),
            &Blake3Digest([1; 32]),
            ValidatorIndex(0),
            RoundNumber(1),
            &[],
        )
        .unwrap();
        let d2 = compute_cert_digest(
            EpochNumber(2), // Different epoch.
            &Blake3Digest([1; 32]),
            ValidatorIndex(0),
            RoundNumber(1),
            &[],
        )
        .unwrap();
        assert_ne!(d1, d2);
    }

    // ── Epoch verification (C-2 / SEC-H8) ──────────────────────────────

    #[test]
    fn certificate_verifier_should_reject_wrong_epoch() {
        let (committee, keys) = make_test_committee(4);
        let batch_digest = Blake3Digest([1u8; 32]);
        let epoch = EpochNumber(1);
        let origin = ValidatorIndex(0);
        let round = RoundNumber(0);
        let parents = vec![];

        let mut builder =
            CertificateBuilder::new(epoch, batch_digest, origin, round, parents.clone(), 4);
        for (i, (sk, _)) in keys.iter().enumerate().take(3) {
            let sig = sign_cert_header(sk, epoch, &batch_digest, origin, round, &parents);
            builder.add_signature(ValidatorIndex(i as u32), sig);
        }
        let cert = builder.build(&committee).unwrap();

        // Verify with correct epoch → should pass.
        CertificateVerifier::verify(&cert, &committee, EpochNumber(1)).unwrap();

        // Verify with wrong epoch → should fail.
        let result = CertificateVerifier::verify(&cert, &committee, EpochNumber(2));
        assert!(
            matches!(result, Err(ConsensusError::EpochMismatch { expected, got })
                if expected == EpochNumber(2) && got == EpochNumber(1)),
            "expected EpochMismatch, got: {result:?}"
        );
    }
}
