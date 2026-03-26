// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! In-memory DAG data structure for Narwhal certificate storage.
//!
//! [`InMemoryDag`] implements [`CertificateDag`] with:
//! - O(1) certificate lookup by `(origin, round)` or `CertDigest`
//! - O(1) round certificate listing
//! - Causality validation on insertion (parents must exist, round must increase)
//! - BFS-based causal history traversal in topological order

use std::collections::{HashMap, VecDeque};

use crate::error::ConsensusError;
use crate::traits::CertificateDag;
use crate::types::NarwhalCertificate;
use nexus_primitives::{CertDigest, RoundNumber, ValidatorIndex};

/// In-memory directed acyclic graph for Narwhal certificates.
///
/// Certificates are indexed by:
/// - `CertDigest` → full certificate (primary store)
/// - `(ValidatorIndex, RoundNumber)` → digest (origin index)
/// - `RoundNumber` → list of digests (round index)
#[derive(Debug, Clone)]
pub struct InMemoryDag {
    /// Primary store: digest → certificate.
    certs: HashMap<CertDigest, NarwhalCertificate>,
    /// Index: (origin, round) → digest.
    by_origin: HashMap<(ValidatorIndex, RoundNumber), CertDigest>,
    /// Index: round → ordered list of digests.
    by_round: HashMap<RoundNumber, Vec<CertDigest>>,
    /// Highest round with at least one certificate.
    highest_round: RoundNumber,
}

impl InMemoryDag {
    /// Create an empty DAG.
    pub fn new() -> Self {
        Self {
            certs: HashMap::new(),
            by_origin: HashMap::new(),
            by_round: HashMap::new(),
            highest_round: RoundNumber(0),
        }
    }

    /// Number of certificates in the DAG.
    pub fn len(&self) -> usize {
        self.certs.len()
    }

    /// Whether the DAG is empty.
    pub fn is_empty(&self) -> bool {
        self.certs.is_empty()
    }

    /// Prune all certificates below the given round.
    ///
    /// This removes certificates from `certs`, `by_origin`, and `by_round`
    /// for all rounds strictly less than `min_round`.  Call this after a
    /// commit to bound memory growth.
    pub fn prune_below_round(&mut self, min_round: RoundNumber) {
        let stale_rounds: Vec<RoundNumber> = self
            .by_round
            .keys()
            .filter(|r| r.0 < min_round.0)
            .copied()
            .collect();

        for round in stale_rounds {
            if let Some(digests) = self.by_round.remove(&round) {
                for digest in &digests {
                    if let Some(cert) = self.certs.remove(digest) {
                        self.by_origin.remove(&(cert.origin, cert.round));
                    }
                }
            }
        }
    }

    /// Check if a certificate with the given digest exists.
    pub fn contains(&self, digest: &CertDigest) -> bool {
        self.certs.contains_key(digest)
    }

    /// Retrieve a certificate by its digest.
    pub fn get_by_digest(&self, digest: &CertDigest) -> Option<&NarwhalCertificate> {
        self.certs.get(digest)
    }

    /// Return all certificate digests currently in the DAG.
    pub fn all_digests(&self) -> Vec<CertDigest> {
        self.certs.keys().copied().collect()
    }
}

impl Default for InMemoryDag {
    fn default() -> Self {
        Self::new()
    }
}

impl CertificateDag for InMemoryDag {
    fn insert_certificate(&mut self, cert: NarwhalCertificate) -> Result<(), ConsensusError> {
        // Check for duplicate.
        if self.certs.contains_key(&cert.cert_digest) {
            return Err(ConsensusError::DuplicateCertificate {
                digest: cert.cert_digest,
            });
        }

        // Check for equivocation: same (origin, round) but different digest.
        if let Some(&existing_digest) = self.by_origin.get(&(cert.origin, cert.round)) {
            if existing_digest != cert.cert_digest {
                return Err(ConsensusError::EquivocatingCertificate {
                    origin: cert.origin,
                    round: cert.round,
                    existing: existing_digest,
                    new: cert.cert_digest,
                });
            }
        }

        // Genesis certificates (round 0) have no parents to validate.
        if cert.round.0 > 0 {
            // Verify all parents exist and enforce strict Narwhal causality:
            // each parent must be from exactly round - 1 (TLD-04 §4).
            let expected_parent_round = cert.round.0 - 1;
            for parent_digest in &cert.parents {
                let parent =
                    self.certs
                        .get(parent_digest)
                        .ok_or(ConsensusError::MissingParent {
                            digest: *parent_digest,
                        })?;
                if parent.round.0 != expected_parent_round {
                    return Err(ConsensusError::CausalityViolation {
                        cert_round: cert.round,
                        parent_round: parent.round,
                    });
                }
            }
        }

        // Update indices.
        let digest = cert.cert_digest;
        let origin = cert.origin;
        let round = cert.round;

        self.by_origin.insert((origin, round), digest);
        self.by_round.entry(round).or_default().push(digest);
        if round.0 > self.highest_round.0 {
            self.highest_round = round;
        }

        self.certs.insert(digest, cert);

        Ok(())
    }

    fn get_certificate(
        &self,
        origin: ValidatorIndex,
        round: RoundNumber,
    ) -> Option<&NarwhalCertificate> {
        self.by_origin
            .get(&(origin, round))
            .and_then(|d| self.certs.get(d))
    }

    fn round_certificates(&self, round: RoundNumber) -> Vec<&NarwhalCertificate> {
        self.by_round
            .get(&round)
            .map(|digests| digests.iter().filter_map(|d| self.certs.get(d)).collect())
            .unwrap_or_default()
    }

    fn current_round(&self) -> RoundNumber {
        self.highest_round
    }

    fn causal_history(&self, cert_digest: &CertDigest) -> Vec<CertDigest> {
        // BFS from the given cert, collecting all ancestors.
        // Returns topological order (ancestors first, cert last).
        let mut visited = HashMap::new();
        let mut queue = VecDeque::new();

        if let Some(cert) = self.certs.get(cert_digest) {
            queue.push_back(*cert_digest);
            visited.insert(*cert_digest, cert.round);
        }

        while let Some(current) = queue.pop_front() {
            if let Some(c) = self.certs.get(&current) {
                for parent in &c.parents {
                    if !visited.contains_key(parent) {
                        if let Some(p) = self.certs.get(parent) {
                            visited.insert(*parent, p.round);
                            queue.push_back(*parent);
                        }
                    }
                }
            }
        }

        // Sort by round ascending (topological order), then by digest for determinism.
        let mut result: Vec<(CertDigest, RoundNumber)> = visited.into_iter().collect();
        result.sort_by(|a, b| a.1 .0.cmp(&b.1 .0).then_with(|| a.0 .0.cmp(&b.0 .0)));
        result.into_iter().map(|(d, _)| d).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ValidatorBitset;
    use nexus_primitives::{Blake3Digest, EpochNumber};
    use nexus_test_utils::fixtures::crypto::make_falcon_keypair;

    /// Make a test certificate with the given parameters.
    fn make_cert(
        origin: u32,
        round: u64,
        parents: Vec<CertDigest>,
        digest_seed: u8,
    ) -> NarwhalCertificate {
        let (_sk, _vk) = make_falcon_keypair();
        NarwhalCertificate {
            epoch: EpochNumber(1),
            batch_digest: Blake3Digest([digest_seed; 32]),
            origin: ValidatorIndex(origin),
            round: RoundNumber(round),
            parents,
            signatures: vec![],
            signers: ValidatorBitset::new(4),
            cert_digest: Blake3Digest([digest_seed; 32]),
        }
    }

    // ── Basic insertion and lookup ──────────────────────────────────────

    #[test]
    fn insert_and_get_genesis_cert() {
        let mut dag = InMemoryDag::new();
        let cert = make_cert(0, 0, vec![], 1);
        let digest = cert.cert_digest;

        dag.insert_certificate(cert).unwrap();

        assert_eq!(dag.len(), 1);
        assert!(!dag.is_empty());
        assert!(dag.contains(&digest));
    }

    #[test]
    fn get_certificate_by_origin_and_round() {
        let mut dag = InMemoryDag::new();
        let cert = make_cert(0, 0, vec![], 1);
        dag.insert_certificate(cert).unwrap();

        let found = dag.get_certificate(ValidatorIndex(0), RoundNumber(0));
        assert!(found.is_some());
        assert_eq!(found.unwrap().origin, ValidatorIndex(0));

        // Not found.
        assert!(dag
            .get_certificate(ValidatorIndex(1), RoundNumber(0))
            .is_none());
    }

    #[test]
    fn round_certificates_returns_all_for_round() {
        let mut dag = InMemoryDag::new();
        let c0 = make_cert(0, 0, vec![], 1);
        let c1 = make_cert(1, 0, vec![], 2);
        dag.insert_certificate(c0).unwrap();
        dag.insert_certificate(c1).unwrap();

        let round_certs = dag.round_certificates(RoundNumber(0));
        assert_eq!(round_certs.len(), 2);

        // Empty round.
        assert!(dag.round_certificates(RoundNumber(99)).is_empty());
    }

    // ── Current round tracking ──────────────────────────────────────────

    #[test]
    fn current_round_tracks_highest() {
        let mut dag = InMemoryDag::new();
        assert_eq!(dag.current_round(), RoundNumber(0));

        let c0 = make_cert(0, 0, vec![], 1);
        let d0 = c0.cert_digest;
        dag.insert_certificate(c0).unwrap();
        assert_eq!(dag.current_round(), RoundNumber(0));

        let c1 = make_cert(0, 1, vec![d0], 2);
        dag.insert_certificate(c1).unwrap();
        assert_eq!(dag.current_round(), RoundNumber(1));
    }

    // ── Error cases ─────────────────────────────────────────────────────

    #[test]
    fn duplicate_certificate_rejected() {
        let mut dag = InMemoryDag::new();
        let cert = make_cert(0, 0, vec![], 1);
        dag.insert_certificate(cert.clone()).unwrap();

        let result = dag.insert_certificate(cert);
        assert!(matches!(
            result,
            Err(ConsensusError::DuplicateCertificate { .. })
        ));
    }

    #[test]
    fn missing_parent_rejected() {
        let mut dag = InMemoryDag::new();
        let fake_parent = Blake3Digest([99; 32]);
        let cert = make_cert(0, 1, vec![fake_parent], 1);

        let result = dag.insert_certificate(cert);
        assert!(matches!(result, Err(ConsensusError::MissingParent { .. })));
    }

    #[test]
    fn causality_violation_rejected() {
        let mut dag = InMemoryDag::new();
        // Insert a cert at round 2.
        let c0 = make_cert(0, 2, vec![], 1);
        let d0 = c0.cert_digest;
        dag.insert_certificate(c0).unwrap();

        // Try to insert at round 2 referencing d0 as parent → violation (parent.round >= cert.round).
        let c1 = make_cert(1, 2, vec![d0], 2);
        let result = dag.insert_certificate(c1);
        assert!(matches!(
            result,
            Err(ConsensusError::CausalityViolation { .. })
        ));
    }

    // ── Causal history ──────────────────────────────────────────────────

    #[test]
    fn causal_history_returns_ancestors_in_topological_order() {
        let mut dag = InMemoryDag::new();

        // Round 0: two genesis certs.
        let g0 = make_cert(0, 0, vec![], 10);
        let g1 = make_cert(1, 0, vec![], 11);
        let d0 = g0.cert_digest;
        let d1 = g1.cert_digest;
        dag.insert_certificate(g0).unwrap();
        dag.insert_certificate(g1).unwrap();

        // Round 1: one cert referencing both.
        let c1 = make_cert(0, 1, vec![d0, d1], 20);
        let d_c1 = c1.cert_digest;
        dag.insert_certificate(c1).unwrap();

        let history = dag.causal_history(&d_c1);
        // Should contain d0, d1, d_c1 in topological order
        // (round 0 before round 1).
        assert_eq!(history.len(), 3);
        assert!(history[..2].contains(&d0));
        assert!(history[..2].contains(&d1));
        assert_eq!(history[2], d_c1);
    }

    #[test]
    fn causal_history_single_cert_no_parents() {
        let mut dag = InMemoryDag::new();
        let c = make_cert(0, 0, vec![], 1);
        let d = c.cert_digest;
        dag.insert_certificate(c).unwrap();

        let history = dag.causal_history(&d);
        assert_eq!(history, vec![d]);
    }

    #[test]
    fn causal_history_unknown_cert_returns_empty() {
        let dag = InMemoryDag::new();
        let unknown = Blake3Digest([42; 32]);
        assert!(dag.causal_history(&unknown).is_empty());
    }

    // ── Multi-round DAG ─────────────────────────────────────────────────

    #[test]
    fn three_round_diamond_dag() {
        let mut dag = InMemoryDag::new();

        // Round 0: 2 genesis certs.
        let g0 = make_cert(0, 0, vec![], 10);
        let g1 = make_cert(1, 0, vec![], 11);
        let d_g0 = g0.cert_digest;
        let d_g1 = g1.cert_digest;
        dag.insert_certificate(g0).unwrap();
        dag.insert_certificate(g1).unwrap();

        // Round 1: 2 certs each referencing both genesis.
        let r1_a = make_cert(0, 1, vec![d_g0, d_g1], 20);
        let r1_b = make_cert(1, 1, vec![d_g0, d_g1], 21);
        let d_r1a = r1_a.cert_digest;
        let d_r1b = r1_b.cert_digest;
        dag.insert_certificate(r1_a).unwrap();
        dag.insert_certificate(r1_b).unwrap();

        // Round 2: 1 cert referencing both round-1 certs.
        let r2 = make_cert(0, 2, vec![d_r1a, d_r1b], 30);
        let d_r2 = r2.cert_digest;
        dag.insert_certificate(r2).unwrap();

        assert_eq!(dag.len(), 5);
        assert_eq!(dag.current_round(), RoundNumber(2));
        assert_eq!(dag.round_certificates(RoundNumber(0)).len(), 2);
        assert_eq!(dag.round_certificates(RoundNumber(1)).len(), 2);
        assert_eq!(dag.round_certificates(RoundNumber(2)).len(), 1);

        // Full causal history from r2 should be all 5 certs.
        let history = dag.causal_history(&d_r2);
        assert_eq!(history.len(), 5);
        // Round 0 certs first, then round 1, then round 2.
        assert!(history[0].0 < history[2].0 || history[1].0 < history[2].0);
        assert_eq!(history[4], d_r2);
    }

    // ── get_by_digest ───────────────────────────────────────────────────

    #[test]
    fn get_by_digest_works() {
        let mut dag = InMemoryDag::new();
        let cert = make_cert(0, 0, vec![], 1);
        let digest = cert.cert_digest;
        dag.insert_certificate(cert).unwrap();

        assert!(dag.get_by_digest(&digest).is_some());
        assert!(dag.get_by_digest(&Blake3Digest([99; 32])).is_none());
    }

    // ── Equivocation detection (C-1 / SEC-H7) ──────────────────────────

    #[test]
    fn dag_should_reject_equivocating_certificate_same_origin_round() {
        let mut dag = InMemoryDag::new();

        // Insert a valid genesis cert from validator 0.
        let c1 = make_cert(0, 0, vec![], 10);
        dag.insert_certificate(c1).unwrap();

        // Try to insert a different cert from the same validator at the same round.
        let c2 = make_cert(0, 0, vec![], 99); // different digest_seed → different digest
        let result = dag.insert_certificate(c2);
        assert!(
            matches!(result, Err(ConsensusError::EquivocatingCertificate { origin, round, .. })
                if origin == ValidatorIndex(0) && round == RoundNumber(0)),
            "expected EquivocatingCertificate, got: {result:?}"
        );

        // DAG should still contain exactly 1 cert—the original.
        assert_eq!(dag.len(), 1);
    }
}
