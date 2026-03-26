// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Crate-level error types for `nexus-primitives`.

use thiserror::Error;

/// Top-level error type for the `nexus-primitives` crate.
#[derive(Debug, Error)]
pub enum NexusPrimitivesError {
    /// A cryptographic digest could not be decoded.
    #[error(transparent)]
    DigestDecode(#[from] DigestDecodeError),
}

/// Error returned when decoding a cryptographic digest from bytes or hex fails.
#[derive(Debug, Error, PartialEq, Eq, Clone)]
pub enum DigestDecodeError {
    /// Byte slice was not the expected length.
    #[error("expected {expected} bytes, got {got}")]
    WrongLength {
        /// Number of bytes expected.
        expected: usize,
        /// Number of bytes actually provided.
        got: usize,
    },
    /// Hex string contained invalid characters or had the wrong length.
    #[error("invalid hex string: {reason}")]
    InvalidHex {
        /// Human-readable description of what was wrong.
        reason: String,
    },
}
