//! Core cryptographic trait definitions.
//!
//! These traits define the **FROZEN-2** interface contracts for all
//! cryptographic operations in the Nexus protocol. Implementations
//! live in sibling modules (`falcon`, `mldsa`, `mlkem`, `hasher`).

use serde::{de::DeserializeOwned, Serialize};
use std::fmt;
use zeroize::ZeroizeOnDrop;

use crate::error::NexusCryptoError;

// ── Signer [SEALED] ──────────────────────────────────────────────────────────

/// Digital signature scheme.
///
/// # Security invariants
/// - `SigningKey` **must** implement `ZeroizeOnDrop` and **must not** implement `Clone`.
/// - `sign` **must** prepend the provided `domain` separator before hashing.
/// - `verify` **must** return `Result`, never a bare `bool`.
pub trait Signer: Send + Sync + 'static {
    /// Private signing key — zeroized on drop, never cloneable.
    type SigningKey: ZeroizeOnDrop + Send + Sync + fmt::Debug;

    /// Public verification key — freely clonable and serializable.
    type VerifyKey: Clone + Send + Sync + Serialize + DeserializeOwned + fmt::Debug;

    /// Produced signature — clonable and serializable.
    type Signature: Clone + Send + Sync + Serialize + DeserializeOwned + fmt::Debug;

    /// Generate a fresh keypair using the OS CSPRNG.
    fn generate_keypair() -> (Self::SigningKey, Self::VerifyKey);

    /// Sign `message` under `domain` separator.
    fn sign(sk: &Self::SigningKey, domain: &[u8], message: &[u8]) -> Self::Signature;

    /// Verify `sig` over `message` with the given `domain` separator.
    ///
    /// # Errors
    /// Returns [`NexusCryptoError::VerificationFailed`] if the signature is invalid.
    fn verify(
        vk: &Self::VerifyKey,
        domain: &[u8],
        message: &[u8],
        sig: &Self::Signature,
    ) -> Result<(), NexusCryptoError>;

    /// Byte size of a single signature.
    fn signature_size() -> usize;

    /// Byte size of a public verification key.
    fn verify_key_size() -> usize;
}

// ── BatchVerifier [STABLE] ────────────────────────────────────────────────────

/// Batch signature verification — parallel verification using `rayon`.
///
/// A default implementation is provided that delegates to
/// [`Signer::verify`] in parallel.
pub trait BatchVerifier: Signer {
    /// Verify multiple `(verify_key, domain, message, signature)` tuples.
    ///
    /// # Errors
    /// Returns [`NexusCryptoError::BatchVerificationFailed`] if one or more
    /// signatures are invalid, listing the indices that failed.
    #[allow(clippy::type_complexity)]
    fn batch_verify(
        pairs: &[(Self::VerifyKey, Vec<u8>, Vec<u8>, Self::Signature)],
    ) -> Result<(), NexusCryptoError>;
}

// ── KeyEncapsulationMechanism [STABLE] ────────────────────────────────────────

/// Post-quantum key encapsulation mechanism (KEM).
///
/// # Security invariants
/// - `DecapsKey` **must** implement `ZeroizeOnDrop` and **must not** implement `Clone`.
/// - `SharedSecret` **must** implement `ZeroizeOnDrop`.
pub trait KeyEncapsulationMechanism: Send + Sync + 'static {
    /// Public encapsulation key.
    type EncapsKey: Clone + Send + Sync + Serialize + DeserializeOwned + fmt::Debug;

    /// Private decapsulation key — zeroized on drop.
    type DecapsKey: ZeroizeOnDrop + Send + Sync + fmt::Debug;

    /// Ciphertext produced by encapsulation.
    type Ciphertext: Clone + Send + Sync + Serialize + DeserializeOwned + fmt::Debug;

    /// Shared secret — zeroized on drop.
    type SharedSecret: ZeroizeOnDrop + AsRef<[u8]> + fmt::Debug;

    /// Generate a fresh encapsulation/decapsulation keypair.
    fn generate_keypair() -> (Self::EncapsKey, Self::DecapsKey);

    /// Encapsulate: produce a shared secret and the corresponding ciphertext.
    fn encapsulate(ek: &Self::EncapsKey) -> (Self::SharedSecret, Self::Ciphertext);

    /// Decapsulate: recover the shared secret from a ciphertext.
    ///
    /// # Errors
    /// Returns [`NexusCryptoError::DecapsulationFailed`] on invalid ciphertext.
    fn decapsulate(
        dk: &Self::DecapsKey,
        ct: &Self::Ciphertext,
    ) -> Result<Self::SharedSecret, NexusCryptoError>;

    /// Byte size of a ciphertext.
    fn ciphertext_size() -> usize;

    /// Byte size of the shared secret.
    fn shared_secret_size() -> usize;
}

// ── CryptoHasher [SEALED] ────────────────────────────────────────────────────

/// Incremental cryptographic hasher with mandatory domain separation.
pub trait CryptoHasher: Send + Sync + Clone + 'static {
    /// Digest output type.
    type Output: Clone
        + Copy
        + Eq
        + std::hash::Hash
        + AsRef<[u8]>
        + Send
        + Sync
        + fmt::Debug
        + fmt::Display;

    /// One-shot: hash `data` under `domain` separator.
    fn hash(domain: &[u8], data: &[u8]) -> Self::Output;

    /// Create a new incremental hasher pre-loaded with a domain separator.
    fn new_with_domain(domain: &[u8]) -> Self;

    /// Feed more data into the hasher.
    fn update(&mut self, data: &[u8]) -> &mut Self;

    /// Consume the hasher and produce the final digest.
    fn finalize(self) -> Self::Output;

    /// Compute a binary Merkle tree root over `leaves`.
    ///
    /// Uses the convention: empty → `Output::default()-equivalent zero hash`,
    /// single leaf → the leaf itself, otherwise pairwise hashing upward.
    fn merkle_root(leaves: &[Self::Output]) -> Self::Output;
}
