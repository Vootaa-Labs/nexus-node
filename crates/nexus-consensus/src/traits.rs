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
        struct EmptyRegistry;
        impl ValidatorRegistry for EmptyRegistry {
            fn active_validators(&self) -> Vec<ValidatorInfo> {
                vec![]
            }
            fn validator_info(&self, _: ValidatorIndex) -> Option<ValidatorInfo> {
                None
            }
            fn quorum_threshold(&self) -> nexus_primitives::Amount {
                nexus_primitives::Amount(1)
            }
            fn total_stake(&self) -> nexus_primitives::Amount {
                nexus_primitives::Amount(0)
            }
            fn is_quorum(&self, _: &ValidatorBitset) -> bool {
                false
            }
        }

        let reg = EmptyRegistry;
        assert!(!reg.is_active(ValidatorIndex(0)));
        assert_eq!(reg.reputation(ValidatorIndex(0)), ReputationScore::ZERO);
    }
}
