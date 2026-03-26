//! `nexus-primitives` — Core primitive types for the Nexus blockchain.
//!
//! This crate has **no internal crate dependencies** and only depends on
//! standard-library-adjacent crates (`serde`, `thiserror`, `hex`, `blake3`, `zeroize`).
//! All other Nexus crates may depend on this crate.
//!
//! # Modules
//! - [`ids`]     — newtype wrappers for protocol identifiers
//! - [`digest`]  — BLAKE3-256 digest type and semantic aliases
//! - [`address`] — account/contract addresses, token IDs, and amounts
//! - [`error`]   — crate-level error types
//! - [`traits`]  — [`ProtocolId`] compile-time constraint trait
//!
//! # Quick import
//! ```
//! use nexus_primitives::{
//!     ValidatorIndex, EpochNumber, RoundNumber, ShardId, CommitSequence, TimestampMs,
//!     Blake3Digest, BatchDigest, CertDigest, BlockDigest, StateRoot, TxDigest, IntentId,
//!     AccountAddress, ContractAddress, TokenId, Amount,
//!     DigestDecodeError, NexusPrimitivesError,
//!     ProtocolId,
//! };
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod address;
pub mod digest;
pub mod error;
pub mod ids;
pub mod traits;

// ── Convenience re-exports at crate root ─────────────────────────────────────

pub use address::{AccountAddress, Amount, ContractAddress, TokenId};
pub use digest::{
    BatchDigest, Blake3Digest, BlockDigest, CertDigest, IntentId, StateRoot, TxDigest,
};
pub use error::{DigestDecodeError, NexusPrimitivesError};
pub use ids::{CommitSequence, EpochNumber, RoundNumber, ShardId, TimestampMs, ValidatorIndex};
pub use traits::ProtocolId;
