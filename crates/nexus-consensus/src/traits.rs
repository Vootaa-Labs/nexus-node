// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Consensus trait contracts.
//!
//! These traits define the sealed interfaces for the consensus layer.
//! Implementations are in the `narwhal/`, `shoal/`, and `validator` modules.
//!
//! Trait stability levels (per ALADF):
//! - `BatchProposer`    — **SEALED** (FROZEN-2)
//! - `CertificateDag`   — **SEALED** (FROZEN-2)
//! - `BftOrderer`       — **SEALED** (FROZEN-2)
//! - `ValidatorRegistry` — **STABLE** (FROZEN-1)

use crate::error::ConsensusError;
use crate::types::{
    BatchStatus, CommittedBatch, NarwhalCertificate, ReputationScore, ValidatorBitset,
    ValidatorInfo,
};
use nexus_primitives::{BatchDigest, CertDigest, RoundNumber, ValidatorIndex};

// ── BatchProposer (SEALED) ───────────────────────────────────────────────────

/// Proposes transaction batches to the Narwhal DAG mempool.
///
/// Workers collect incoming transactions, assemble them into `NarwhalBatch`
/// payloads (≤ 512 KiB), and broadcast them for certification.
///
/// # Stability: SEALED (FROZEN-2)
/// Modifying this trait's signature requires a formal architecture review.
pub trait BatchProposer: Send + Sync + 'static {
    /// Submit a batch of raw transactions for inclusion in the DAG.
    ///
    /// Returns the `BatchDigest` of the assembled batch on success.
    /// The caller can then poll `batch_status` to track progress.
    fn propose_batch(
        &self,
        transactions: Vec<Vec<u8>>,
    ) -> impl std::future::Future<Output = Result<BatchDigest, ConsensusError>> + Send;

    /// Query the current status of a previously proposed batch.
    fn batch_status(
        &self,
        digest: &BatchDigest,
    ) -> impl std::future::Future<Output = Result<BatchStatus, ConsensusError>> + Send;
}

// ── CertificateDag (SEALED) ──────────────────────────────────────────────────

/// DAG data structure for Narwhal certificate storage and traversal.
///
/// Each round of the DAG contains one certificate per honest validator.
/// Certificates reference their parents from the previous round,
/// forming a directed acyclic graph with causal ordering guarantees.
///
/// # Stability: SEALED (FROZEN-2)
pub trait CertificateDag: Send + Sync + 'static {
    /// Insert a verified certificate into the DAG.
    ///
    /// Returns `ConsensusError::DuplicateCertificate` if already present.
    /// Returns `ConsensusError::MissingParent` if any parent is not in the DAG.
    /// Returns `ConsensusError::CausalityViolation` if round constraints are broken.
    fn insert_certificate(&mut self, cert: NarwhalCertificate) -> Result<(), ConsensusError>;

    /// Retrieve a certificate by its origin validator and round.
    fn get_certificate(
        &self,
        origin: ValidatorIndex,
        round: RoundNumber,
    ) -> Option<&NarwhalCertificate>;

    /// Retrieve all certificates in a given round.
    fn round_certificates(&self, round: RoundNumber) -> Vec<&NarwhalCertificate>;

    /// The highest round with at least one certificate.
    fn current_round(&self) -> RoundNumber;

    /// Compute the full causal history of a certificate (topological order).
    ///
    /// Returns all certificate digests reachable from the given cert,
    /// sorted in causal (topological) order.
    fn causal_history(&self, cert: &CertDigest) -> Vec<CertDigest>;
}

// ── BftOrderer (SEALED) ──────────────────────────────────────────────────────

/// Shoal++ BFT total-order finalization.
///
/// Operates on top of the `CertificateDag`, selecting anchors every 2 rounds
/// and committing sub-DAGs when the commit rule is satisfied.
///
/// # Stability: SEALED (FROZEN-2)
pub trait BftOrderer: Send + Sync + 'static {
    /// Attempt to commit the next sub-DAG based on the current DAG state.
    ///
    /// Returns `Some(CommittedBatch)` if a new anchor was committed,
    /// `None` if the commit rule is not yet satisfied.
    fn try_commit(
        &mut self,
        dag: &dyn CertificateDag,
    ) -> Result<Option<CommittedBatch>, ConsensusError>;

    /// Update a validator's reputation based on observed latency.
    ///
    /// Lower latency → higher reputation → higher anchor election probability.
    fn update_reputation(&mut self, validator: ValidatorIndex, latency_ms: u32);

    /// Return the current anchor candidates ordered by reputation (descending).
    fn current_anchor_candidates(&self) -> Vec<ValidatorIndex>;
}

// ── ValidatorRegistry (STABLE) ───────────────────────────────────────────────

/// Committee management and quorum computation.
///
/// Tracks the active validator set for the current epoch, including
/// stake weights, public keys, reputation scores, and slashing status.
///
/// # Stability: STABLE (FROZEN-1)
pub trait ValidatorRegistry: Send + Sync + 'static {
    /// Return all active (non-slashed) validators in the current committee.
    fn active_validators(&self) -> Vec<ValidatorInfo>;

    /// Look up a specific validator by index.
    fn validator_info(&self, index: ValidatorIndex) -> Option<ValidatorInfo>;

    /// The stake-weighted quorum threshold: ⌊total_stake × 2/3⌋ + 1.
    fn quorum_threshold(&self) -> nexus_primitives::Amount;

    /// Total stake across all active validators.
    fn total_stake(&self) -> nexus_primitives::Amount;

    /// Check whether the given set of signers constitutes a quorum.
    ///
    /// Returns `true` if the combined stake of active set validators
    /// meets or exceeds the stake-weighted quorum threshold.
    fn is_quorum(&self, signers: &ValidatorBitset) -> bool;

    /// Check whether a specific validator is active and not slashed.
    fn is_active(&self, index: ValidatorIndex) -> bool {
        self.validator_info(index)
            .map(|v| !v.is_slashed)
            .unwrap_or(false)
    }

    /// Compute the reputation score for a validator.
    fn reputation(&self, index: ValidatorIndex) -> ReputationScore {
        self.validator_info(index)
            .map(|v| v.reputation)
            .unwrap_or(ReputationScore::ZERO)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_test_utils::fixtures::crypto::make_falcon_keypair;

    struct MockRegistry {
        validators: Vec<ValidatorInfo>,
    }

    impl ValidatorRegistry for MockRegistry {
        fn active_validators(&self) -> Vec<ValidatorInfo> {
            self.validators
                .iter()
                .filter(|v| !v.is_slashed)
                .cloned()
                .collect()
        }

        fn validator_info(&self, index: ValidatorIndex) -> Option<ValidatorInfo> {
            self.validators.iter().find(|v| v.index == index).cloned()
        }

        fn quorum_threshold(&self) -> nexus_primitives::Amount {
            nexus_primitives::Amount(1)
        }

        fn total_stake(&self) -> nexus_primitives::Amount {
            nexus_primitives::Amount(self.validators.len() as u64)
        }

        fn is_quorum(&self, _: &ValidatorBitset) -> bool {
            false
        }
    }

    // Verify traits are object-safe where needed (BftOrderer uses dyn CertificateDag)
    // CertificateDag cannot be object-safe due to &NarwhalCertificate return types,
    // but BftOrderer's dyn usage works because it's a parameter trait object.

    // Verify trait bounds are Send + Sync + 'static (compile-time check)
    fn _assert_batch_proposer_bounds<T: BatchProposer>() {}
    fn _assert_certificate_dag_bounds<T: CertificateDag>() {}
    fn _assert_bft_orderer_bounds<T: BftOrderer>() {}
    fn _assert_validator_registry_bounds<T: ValidatorRegistry>() {}

    #[test]
    fn trait_object_safety_bft_orderer_param() {
        // BftOrderer::try_commit takes &dyn CertificateDag as parameter.
        // This test verifies the trait compiles with the dyn dispatch pattern.
        // Actual implementation tests will be in T-1003+.
    }

    /// Verify the default is_active implementation logic.
    #[test]
    fn default_is_active_returns_false_for_unknown() {
        let reg = MockRegistry { validators: vec![] };
        assert!(!reg.is_active(ValidatorIndex(0)));
        assert_eq!(reg.reputation(ValidatorIndex(0)), ReputationScore::ZERO);
    }

    #[test]
    fn default_is_active_returns_true_for_known_unslashed_validator() {
        let (_sk, vk) = make_falcon_keypair();
        let reg = MockRegistry {
            validators: vec![ValidatorInfo {
                index: ValidatorIndex(7),
                falcon_pub_key: vk,
                stake: nexus_primitives::Amount(10),
                reputation: ReputationScore::from_f32(0.8),
                is_slashed: false,
                shard_id: None,
            }],
        };

        assert!(reg.is_active(ValidatorIndex(7)));
    }

    #[test]
    fn default_methods_respect_slashed_and_reputation_values() {
        let (_sk, vk) = make_falcon_keypair();
        let rep = ReputationScore::from_f32(0.42);
        let reg = MockRegistry {
            validators: vec![ValidatorInfo {
                index: ValidatorIndex(3),
                falcon_pub_key: vk,
                stake: nexus_primitives::Amount(10),
                reputation: rep,
                is_slashed: true,
                shard_id: None,
            }],
        };

        assert!(!reg.is_active(ValidatorIndex(3)));
        assert_eq!(reg.reputation(ValidatorIndex(3)), rep);
    }

    #[test]
    fn active_validators_filters_slashed_out() {
        let (_sk1, vk1) = make_falcon_keypair();
        let (_sk2, vk2) = make_falcon_keypair();
        let reg = MockRegistry {
            validators: vec![
                ValidatorInfo {
                    index: ValidatorIndex(0),
                    falcon_pub_key: vk1,
                    stake: nexus_primitives::Amount(10),
                    reputation: ReputationScore::ZERO,
                    is_slashed: false,
                    shard_id: None,
                },
                ValidatorInfo {
                    index: ValidatorIndex(1),
                    falcon_pub_key: vk2,
                    stake: nexus_primitives::Amount(10),
                    reputation: ReputationScore::ZERO,
                    is_slashed: true,
                    shard_id: None,
                },
            ],
        };

        let active = reg.active_validators();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].index, ValidatorIndex(0));
    }

    #[test]
    fn quorum_threshold_and_total_stake_reflect_validator_count() {
        let mut validators = Vec::new();
        for i in 0..3u64 {
            let (_sk, vk) = make_falcon_keypair();
            validators.push(ValidatorInfo {
                index: ValidatorIndex(i as u32),
                falcon_pub_key: vk,
                stake: nexus_primitives::Amount(i + 1),
                reputation: ReputationScore::ZERO,
                is_slashed: false,
                shard_id: None,
            });
        }
        let reg = MockRegistry {
            validators: validators.clone(),
        };

        assert_eq!(reg.quorum_threshold(), nexus_primitives::Amount(1));
        assert_eq!(reg.total_stake(), nexus_primitives::Amount(3));
    }

    #[test]
    fn is_quorum_mock_always_returns_false() {
        let reg = MockRegistry { validators: vec![] };
        let bitset = ValidatorBitset::new(4);
        assert!(!reg.is_quorum(&bitset));
    }

    #[test]
    fn validator_info_returns_none_for_missing_index() {
        let reg = MockRegistry { validators: vec![] };
        assert!(reg.validator_info(ValidatorIndex(99)).is_none());
    }
}
