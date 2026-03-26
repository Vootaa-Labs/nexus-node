//! Unified error types for the `nexus-crypto` crate.
//!
//! Every cryptographic operation returns `Result<T, NexusCryptoError>` —
//! no boolean success/failure, no panics.

/// Top-level error type for all cryptographic operations.
#[derive(Debug, thiserror::Error)]
pub enum NexusCryptoError {
    /// Signature verification failed.
    #[error("signature verification failed: {reason}")]
    VerificationFailed {
        /// Human-readable description of the failure.
        reason: String,
    },

    /// Key decoding or parsing error.
    #[error("invalid key material: {reason}")]
    InvalidKey {
        /// What was wrong with the key bytes.
        reason: String,
    },

    /// Signature bytes could not be decoded.
    #[error("invalid signature encoding: {reason}")]
    InvalidSignature {
        /// What was wrong with the signature bytes.
        reason: String,
    },

    /// KEM decapsulation failed (ciphertext invalid or tampered).
    #[error("decapsulation failed: {reason}")]
    DecapsulationFailed {
        /// Description of the decapsulation error.
        reason: String,
    },

    /// Batch verification found one or more invalid signatures.
    #[error("batch verification failed: {count} of {total} signatures invalid")]
    BatchVerificationFailed {
        /// Number of invalid signatures.
        count: usize,
        /// Total number of signatures checked.
        total: usize,
        /// Indices of the failed verifications.
        failed_indices: Vec<usize>,
    },
}

/// Result alias for cryptographic operations.
pub type CryptoResult<T> = Result<T, NexusCryptoError>;
