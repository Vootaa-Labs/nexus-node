// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! BLAKE3-256 digest type and semantic type aliases.
//!
//! All digest / hash computations in the Nexus protocol use BLAKE3-256 (32 bytes).
//! Semantic aliases (`BatchDigest`, `CertDigest`, …) are zero-cost: they are the
//! same type at runtime and interchange freely without conversion.

use std::fmt;

use crate::error::DigestDecodeError;
use serde::{Deserialize, Serialize};

/// BLAKE3-256 digest — a fixed 32-byte cryptographic hash output.
///
/// BCS serialization: exactly 32 bytes with no length prefix.
/// JSON serialization: lowercase hex string (64 characters).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Blake3Digest(
    /// Raw 32-byte digest value.
    pub [u8; 32],
);

impl Serialize for Blake3Digest {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if serializer.is_human_readable() {
            serializer.serialize_str(&self.to_hex())
        } else {
            self.0.serialize(serializer)
        }
    }
}

impl<'de> Deserialize<'de> for Blake3Digest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if deserializer.is_human_readable() {
            let s = String::deserialize(deserializer)?;
            Self::from_hex(&s).map_err(serde::de::Error::custom)
        } else {
            let bytes = <[u8; 32]>::deserialize(deserializer)?;
            Ok(Self(bytes))
        }
    }
}

impl Blake3Digest {
    /// All-zero digest. **Must not** appear on production code paths; present
    /// only to simplify test fixture construction.
    pub const ZERO: Self = Self([0u8; 32]);

    /// Byte length of every BLAKE3-256 digest.
    pub const BYTE_LEN: usize = 32;

    /// Wrap raw bytes as a digest.
    #[inline]
    pub fn from_bytes(b: [u8; 32]) -> Self {
        Self(b)
    }

    /// Return a reference to the underlying byte array.
    #[inline]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Encode as a lowercase hex string (64 characters).
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Decode from a lowercase hex string.
    ///
    /// # Errors
    /// Returns [`DigestDecodeError::InvalidHex`] if the string contains
    /// non-hex characters or is not exactly 64 characters long.
    pub fn from_hex(s: &str) -> Result<Self, DigestDecodeError> {
        let mut bytes = [0u8; 32];
        hex::decode_to_slice(s, &mut bytes).map_err(|e| DigestDecodeError::InvalidHex {
            reason: e.to_string(),
        })?;
        Ok(Self(bytes))
    }
}

/// Construct from a byte slice of exactly 32 bytes.
///
/// # Errors
/// Returns [`DigestDecodeError::WrongLength`] if `value.len() != 32`.
impl TryFrom<&[u8]> for Blake3Digest {
    type Error = DigestDecodeError;

    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        if value.len() != 32 {
            return Err(DigestDecodeError::WrongLength {
                expected: 32,
                got: value.len(),
            });
        }
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(value);
        Ok(Self(bytes))
    }
}

impl AsRef<[u8]> for Blake3Digest {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for Blake3Digest {
    /// Abbreviated representation showing only the first and last 4 bytes
    /// (8 hex characters each) to keep log output readable.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hex = self.to_hex();
        write!(f, "Blake3Digest({}…{})", &hex[..8], &hex[56..])
    }
}

impl fmt::Display for Blake3Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

impl fmt::LowerHex for Blake3Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

// ── Semantic type aliases (zero runtime overhead) ─────────────────────────────

/// Narwhal batch content digest.
pub type BatchDigest = Blake3Digest;
/// Narwhal certificate digest.
pub type CertDigest = Blake3Digest;
/// Execution block digest.
pub type BlockDigest = Blake3Digest;
/// State commitment (sorted Merkle) root.
pub type StateRoot = Blake3Digest;
/// Single transaction digest.
pub type TxDigest = Blake3Digest;
/// User intent unique identifier.
pub type IntentId = Blake3Digest;

// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // DG-01: ZERO constant
    #[test]
    fn zero_is_all_zeros() {
        assert_eq!(Blake3Digest::ZERO.0, [0u8; 32]);
    }

    // DG-02: from_bytes / as_bytes roundtrip
    #[test]
    fn from_bytes_as_bytes_roundtrip() {
        let raw = [0xab_u8; 32];
        let d = Blake3Digest::from_bytes(raw);
        assert_eq!(d.as_bytes(), &raw);
    }

    // DG-03: to_hex / from_hex roundtrip
    #[test]
    fn hex_roundtrip() {
        let raw = [0xde_u8; 32];
        let d = Blake3Digest::from_bytes(raw);
        let hex = d.to_hex();
        assert_eq!(hex.len(), 64, "hex string must be 64 characters");
        let d2 = Blake3Digest::from_hex(&hex).expect("valid hex");
        assert_eq!(d, d2);
    }

    // DG-04: from_hex rejects invalid input
    #[test]
    fn from_hex_rejects_non_hex_chars() {
        assert!(Blake3Digest::from_hex(
            "not-valid-hex-at-all-!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!"
        )
        .is_err());
    }

    #[test]
    fn from_hex_rejects_too_short() {
        // Only 8 hex chars (4 bytes), need 64
        assert!(Blake3Digest::from_hex("deadbeef").is_err());
    }

    #[test]
    fn from_hex_rejects_too_long() {
        // 66 hex chars instead of 64
        assert!(Blake3Digest::from_hex(&"aa".repeat(33)).is_err());
    }

    // DG-05: Debug output is abbreviated (does not expose full 64-char hex)
    #[test]
    fn debug_output_is_abbreviated() {
        let d = Blake3Digest::ZERO;
        let s = format!("{d:?}");
        assert!(
            s.starts_with("Blake3Digest("),
            "should start with Blake3Digest("
        );
        // Full hex would be 64 chars; abbreviated shows only 8+8 = 16 hex chars
        assert!(s.len() < 50, "debug output should be short, got: {s}");
    }

    // DG-06: BCS serialization roundtrip
    #[test]
    fn bcs_serde_roundtrip() {
        let d = Blake3Digest::from_bytes([0x42; 32]);
        let bytes = bcs::to_bytes(&d).expect("bcs serialize");
        let d2: Blake3Digest = bcs::from_bytes(&bytes).expect("bcs deserialize");
        assert_eq!(d, d2);
    }

    // DG-07: TryFrom<&[u8]> — correct length succeeds
    #[test]
    fn try_from_slice_correct_length() {
        let buf = [0x11_u8; 32];
        let d = Blake3Digest::try_from(buf.as_slice()).expect("exact 32 bytes");
        assert_eq!(d.0, buf);
    }

    // DG-07: TryFrom<&[u8]> — wrong length returns Err
    #[test]
    fn try_from_slice_wrong_length_short() {
        let buf = [0x11_u8; 16];
        assert!(Blake3Digest::try_from(buf.as_slice()).is_err());
    }

    #[test]
    fn try_from_slice_wrong_length_long() {
        let buf = [0x11_u8; 64];
        assert!(Blake3Digest::try_from(buf.as_slice()).is_err());
    }

    // DG-08: semantic type aliases are the same type (compile-time check via assignment)
    #[test]
    fn type_aliases_are_blake3_digest() {
        let d: BatchDigest = Blake3Digest::ZERO;
        let _: CertDigest = d; // same type, no conversion needed
        let _: BlockDigest = d;
        let _: StateRoot = d;
        let _: TxDigest = d;
        let _: IntentId = d;
    }

    // LowerHex formatting
    #[test]
    fn lower_hex_format() {
        let d = Blake3Digest::from_bytes([0xff; 32]);
        let s = format!("{d:x}");
        assert_eq!(s, "ff".repeat(32));
    }

    // Display is full hex
    #[test]
    fn display_is_full_hex() {
        let d = Blake3Digest::from_bytes([0x00; 32]);
        let s = format!("{d}");
        assert_eq!(s, "00".repeat(32));
    }

    // AsRef<[u8]>
    #[test]
    fn as_ref_slice() {
        let d = Blake3Digest::from_bytes([0xca; 32]);
        let slice: &[u8] = d.as_ref();
        assert_eq!(slice.len(), 32);
        assert!(slice.iter().all(|&b| b == 0xca));
    }

    // DG-09: JSON serialization uses hex string
    #[test]
    fn json_roundtrip_hex_string() {
        let d = Blake3Digest::from_bytes([0xab; 32]);
        let json = serde_json::to_string(&d).expect("json serialize");
        assert!(json.starts_with('"'), "JSON should be a string: {json}");
        assert_eq!(json.len(), 66, "quotes + 64 hex chars");
        let d2: Blake3Digest = serde_json::from_str(&json).expect("json deserialize");
        assert_eq!(d, d2);
    }
}
