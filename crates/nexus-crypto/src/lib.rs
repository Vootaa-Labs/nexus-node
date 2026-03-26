//! `nexus-crypto` — Safe high-level cryptographic API for Nexus.
//!
//! Wraps RustCrypto `ml-dsa`/`ml-kem` (pure Rust) and `pqcrypto-falcon`
//! (C FFI) bindings with an ergonomic, safe interface.
//! Sensitive key material is protected with [`zeroize`] on drop.
//!
//! # Cryptographic scheme map
//! | Scheme | Module | Use case |
//! |--------|--------|---------|
//! | FALCON-512 | [`falcon`] | Consensus signatures (Narwhal, Shoal++) |
//! | ML-DSA-65 | [`mldsa`] | User transaction signatures (FIPS 204) |
//! | ML-KEM-768 | [`mlkem`] | Key encapsulation (FIPS 203) |
//! | BLAKE3-256 | [`hasher`] | All digest / hash computations |
//!
//! # Quick import
//! ```no_run
//! use nexus_crypto::{
//!     // Traits
//!     Signer, BatchVerifier, KeyEncapsulationMechanism, CryptoHasher,
//!     // Implementations
//!     FalconSigner, DilithiumSigner, KyberKem, Blake3Hasher,
//!     // Error
//!     NexusCryptoError,
//!     // Domain separators
//!     domains,
//! };
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub(crate) mod csprng;
pub mod domains;
pub mod error;
pub mod falcon;
pub mod hasher;
pub mod mldsa;
pub mod mlkem;
pub mod traits;

// ── Convenience re-exports at crate root ─────────────────────────────────────

// Traits
pub use traits::{BatchVerifier, CryptoHasher, KeyEncapsulationMechanism, Signer};

// Error
pub use error::{CryptoResult, NexusCryptoError};

// FALCON-512
pub use falcon::{FalconSignature, FalconSigner, FalconSigningKey, FalconVerifyKey};

// ML-DSA-65 (FIPS 204)
pub use mldsa::{DilithiumSignature, DilithiumSigner, DilithiumSigningKey, DilithiumVerifyKey};

// ML-KEM-768 (FIPS 203)
pub use mlkem::{KyberCiphertext, KyberDecapsKey, KyberEncapsKey, KyberKem, KyberSharedSecret};

// Blake3 Hasher
pub use hasher::Blake3Hasher;
