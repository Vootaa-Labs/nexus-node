//! Consensus-layer error types.

use nexus_primitives::{CertDigest, RoundNumber, ValidatorIndex};
use thiserror::Error;

/// Errors produced by the consensus layer.
#[derive(Debug, Error)]
pub enum ConsensusError {
    /// Certificate signers do not meet the stake-weighted quorum threshold.
    #[error("insufficient signer stake: need {required}, got {got}")]
    InsufficientSignatures {
        /// Minimum required stake (⌊2/3 × total_stake⌋ + 1).
        required: u64,
        /// Accumulated stake of collected signers.
        got: u64,
    },

    /// A signature on a certificate failed verification.
    #[error("invalid signature from validator {validator}")]
    InvalidSignature {
        /// The validator whose signature is invalid.
        validator: ValidatorIndex,
        /// The underlying crypto error message.
        source: nexus_crypto::NexusCryptoError,
    },

    /// Attempted to insert a certificate that already exists in the DAG.
    #[error("duplicate certificate: {digest}")]
    DuplicateCertificate {
        /// The digest of the duplicate certificate.
        digest: CertDigest,
    },

    /// Certificate references a parent that is not in the DAG.
    #[error("missing parent certificate: {digest}")]
    MissingParent {
        /// The digest of the missing parent.
        digest: CertDigest,
    },

    /// Same validator produced two different certificates in the same round (equivocation).
    #[error("equivocating certificate from validator {origin} at round {round}: existing {existing}, new {new}")]
    EquivocatingCertificate {
        /// The validator that equivocated.
        origin: ValidatorIndex,
        /// The round in which equivocation occurred.
        round: RoundNumber,
        /// The digest of the certificate already in the DAG.
        existing: CertDigest,
        /// The digest of the conflicting new certificate.
        new: CertDigest,
    },

    /// Certificate is from a round that violates DAG causality.
    #[error("causality violation: cert round {cert_round} but parent in round {parent_round}")]
    CausalityViolation {
        /// The certificate's round.
        cert_round: RoundNumber,
        /// The parent's round (should be cert_round - 1).
        parent_round: RoundNumber,
    },

    /// Validator is not in the active committee for this epoch.
    #[error("unknown validator: {0}")]
    UnknownValidator(ValidatorIndex),

    /// Validator has been slashed and cannot participate.
    #[error("slashed validator: {0}")]
    SlashedValidator(ValidatorIndex),

    /// Batch exceeds the maximum allowed size.
    #[error("batch too large: {size} bytes (max {max} bytes)")]
    BatchTooLarge {
        /// Actual batch size in bytes.
        size: usize,
        /// Maximum allowed size.
        max: usize,
    },

    /// Epoch has changed; operation no longer valid.
    #[error("epoch mismatch: expected {expected}, got {got}")]
    EpochMismatch {
        /// The expected epoch.
        expected: nexus_primitives::EpochNumber,
        /// The epoch received.
        got: nexus_primitives::EpochNumber,
    },

    /// Internal storage error during DAG persistence.
    #[error("storage error: {0}")]
    Storage(String),

    /// Serialization / deserialization failure.
    #[error("codec error: {0}")]
    Codec(String),

    /// Total stake overflowed `u64` during accumulation.
    #[error("stake overflow: total stake exceeds u64::MAX")]
    StakeOverflow,

    /// The committed sub-DAG buffer is full (execution layer is not draining fast enough).
    #[error("committed buffer full: {len} pending (max {max})")]
    CommittedBufferFull {
        /// Current number of pending committed batches.
        len: usize,
        /// Maximum allowed.
        max: usize,
    },
}

/// Convenience alias.
pub type ConsensusResult<T> = Result<T, ConsensusError>;
