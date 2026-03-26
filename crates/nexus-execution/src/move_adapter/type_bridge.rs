// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Move ↔ Nexus type conversion bridge.
//!
//! All Move-internal type representations are isolated within `move_adapter/`
//! (TLD-03 §5 isolation principle).  This module provides bidirectional
//! conversions between Nexus core types and Move-compatible representations.
//!
//! When the real `move-core-types` crate is added (feature-gated), the
//! `MoveAddress` alias will be replaced by
//! `move_core_types::account_address::AccountAddress`.

// Items in this module are foundational for T-2005/T-2006; not yet consumed.
#![allow(dead_code)]

use nexus_primitives::{AccountAddress, ContractAddress};
use std::fmt;

// ── Error type ──────────────────────────────────────────────────────────

/// Errors that can occur during type bridge operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TypeBridgeError {
    /// The byte slice is not the expected length for an address.
    InvalidAddress {
        /// Human-readable description.
        reason: String,
    },
    /// A value could not be decoded from bytes.
    DecodingError {
        /// Human-readable description.
        reason: String,
    },
}

impl fmt::Display for TypeBridgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidAddress { reason } => write!(f, "invalid address: {reason}"),
            Self::DecodingError { reason } => write!(f, "decoding error: {reason}"),
        }
    }
}

impl std::error::Error for TypeBridgeError {}

// ── Move-compatible address type ────────────────────────────────────────

/// Move-compatible 32-byte address.
///
/// When real `move-core-types` is integrated, this will become a newtype
/// wrapper around `move_core_types::account_address::AccountAddress`.
/// For now it is a transparent 32-byte array.
pub(crate) type MoveAddress = [u8; 32];

// ── Address conversions ─────────────────────────────────────────────────

/// Convert a Nexus [`AccountAddress`] to a [`MoveAddress`].
///
/// Both are 32-byte arrays with identical layout, so the conversion
/// is a direct copy.
#[inline]
pub(crate) fn nexus_to_move_address(addr: &AccountAddress) -> MoveAddress {
    addr.0
}

/// Convert a [`MoveAddress`] back to a Nexus [`AccountAddress`].
#[inline]
pub(crate) fn move_to_nexus_address(addr: &MoveAddress) -> AccountAddress {
    AccountAddress(*addr)
}

/// Convert a [`ContractAddress`] to an [`AccountAddress`] for Move VM dispatch.
///
/// Both types share the same 32-byte internal layout.  This conversion
/// is used at the executor ↔ Move VM boundary so that the VM adapter
/// operates exclusively on `AccountAddress`.
#[inline]
pub(crate) fn contract_to_account(contract: &ContractAddress) -> AccountAddress {
    AccountAddress(contract.0)
}

/// Convert an [`AccountAddress`] back to a [`ContractAddress`].
#[inline]
pub(crate) fn account_to_contract(account: &AccountAddress) -> ContractAddress {
    ContractAddress(account.0)
}

/// Parse a 32-byte slice into an [`AccountAddress`].
///
/// # Errors
///
/// Returns [`TypeBridgeError::InvalidAddress`] if the slice is not exactly 32 bytes.
pub(crate) fn address_from_bytes(bytes: &[u8]) -> Result<AccountAddress, TypeBridgeError> {
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| TypeBridgeError::InvalidAddress {
            reason: format!("expected 32 bytes, got {}", bytes.len()),
        })?;
    Ok(AccountAddress(arr))
}

// ── Scalar value encoding / decoding ────────────────────────────────────

/// Encode a `u64` as little-endian bytes (BCS-compatible).
#[inline]
pub(crate) fn encode_u64(value: u64) -> Vec<u8> {
    value.to_le_bytes().to_vec()
}

/// Decode a `u64` from little-endian bytes.
///
/// # Errors
///
/// Returns [`TypeBridgeError::DecodingError`] if the slice is not exactly 8 bytes.
pub(crate) fn decode_u64(bytes: &[u8]) -> Result<u64, TypeBridgeError> {
    let arr: [u8; 8] = bytes
        .try_into()
        .map_err(|_| TypeBridgeError::DecodingError {
            reason: format!("expected 8 bytes for u64, got {}", bytes.len()),
        })?;
    Ok(u64::from_le_bytes(arr))
}

/// Encode a `u128` as little-endian bytes (BCS-compatible).
#[inline]
pub(crate) fn encode_u128(value: u128) -> Vec<u8> {
    value.to_le_bytes().to_vec()
}

/// Decode a `u128` from little-endian bytes.
///
/// # Errors
///
/// Returns [`TypeBridgeError::DecodingError`] if the slice is not exactly 16 bytes.
pub(crate) fn decode_u128(bytes: &[u8]) -> Result<u128, TypeBridgeError> {
    let arr: [u8; 16] = bytes
        .try_into()
        .map_err(|_| TypeBridgeError::DecodingError {
            reason: format!("expected 16 bytes for u128, got {}", bytes.len()),
        })?;
    Ok(u128::from_le_bytes(arr))
}

/// Encode a boolean as a single byte (`0x00` = false, `0x01` = true).
#[inline]
pub(crate) fn encode_bool(value: bool) -> Vec<u8> {
    vec![u8::from(value)]
}

/// Decode a boolean from a single byte.
///
/// # Errors
///
/// Returns [`TypeBridgeError::DecodingError`] if the slice is not exactly 1 byte
/// or the byte is not `0x00` or `0x01`.
pub(crate) fn decode_bool(bytes: &[u8]) -> Result<bool, TypeBridgeError> {
    if bytes.len() != 1 {
        return Err(TypeBridgeError::DecodingError {
            reason: format!("expected 1 byte for bool, got {}", bytes.len()),
        });
    }
    match bytes[0] {
        0 => Ok(false),
        1 => Ok(true),
        other => Err(TypeBridgeError::DecodingError {
            reason: format!("invalid bool byte: 0x{other:02x} (expected 0x00 or 0x01)"),
        }),
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(b: u8) -> AccountAddress {
        AccountAddress([b; 32])
    }

    fn contract(b: u8) -> ContractAddress {
        ContractAddress([b; 32])
    }

    // ── Address roundtrip tests ─────────────────────────────────────

    #[test]
    fn nexus_move_address_roundtrip() {
        let original = addr(0xAB);
        let move_addr = nexus_to_move_address(&original);
        let back = move_to_nexus_address(&move_addr);
        assert_eq!(original, back);
    }

    #[test]
    fn contract_account_roundtrip() {
        let original = contract(0xCD);
        let account = contract_to_account(&original);
        let back = account_to_contract(&account);
        assert_eq!(original, back);
    }

    #[test]
    fn address_from_bytes_success() {
        let bytes = [0x42u8; 32];
        let addr = address_from_bytes(&bytes).unwrap();
        assert_eq!(addr.0, bytes);
    }

    #[test]
    fn address_from_bytes_wrong_length() {
        let err = address_from_bytes(&[0u8; 16]).unwrap_err();
        assert!(matches!(err, TypeBridgeError::InvalidAddress { .. }));
    }

    #[test]
    fn zero_address_roundtrip() {
        let zero = AccountAddress::ZERO;
        let move_addr = nexus_to_move_address(&zero);
        assert_eq!(move_addr, [0u8; 32]);
        assert_eq!(move_to_nexus_address(&move_addr), zero);
    }

    // ── Scalar encoding roundtrip tests ─────────────────────────────

    #[test]
    fn u64_roundtrip() {
        for val in [0u64, 1, 42, u64::MAX, u64::MAX / 2] {
            let encoded = encode_u64(val);
            assert_eq!(encoded.len(), 8);
            assert_eq!(decode_u64(&encoded).unwrap(), val);
        }
    }

    #[test]
    fn u64_decode_wrong_length() {
        let err = decode_u64(&[0u8; 4]).unwrap_err();
        assert!(matches!(err, TypeBridgeError::DecodingError { .. }));
    }

    #[test]
    fn u128_roundtrip() {
        for val in [0u128, 1, u128::MAX, u128::MAX / 2] {
            let encoded = encode_u128(val);
            assert_eq!(encoded.len(), 16);
            assert_eq!(decode_u128(&encoded).unwrap(), val);
        }
    }

    #[test]
    fn u128_decode_wrong_length() {
        let err = decode_u128(&[0u8; 8]).unwrap_err();
        assert!(matches!(err, TypeBridgeError::DecodingError { .. }));
    }

    #[test]
    fn bool_roundtrip() {
        let f = encode_bool(false);
        let t = encode_bool(true);
        assert_eq!(f, vec![0x00]);
        assert_eq!(t, vec![0x01]);
        assert!(!decode_bool(&f).unwrap());
        assert!(decode_bool(&t).unwrap());
    }

    #[test]
    fn bool_decode_invalid() {
        let err = decode_bool(&[0x02]).unwrap_err();
        assert!(matches!(err, TypeBridgeError::DecodingError { .. }));
    }

    #[test]
    fn bool_decode_empty() {
        let err = decode_bool(&[]).unwrap_err();
        assert!(matches!(err, TypeBridgeError::DecodingError { .. }));
    }

    // ── Determinism tests ───────────────────────────────────────────

    #[test]
    fn address_conversion_is_deterministic() {
        let a1 = addr(0xFF);
        let a2 = addr(0xFF);
        assert_eq!(nexus_to_move_address(&a1), nexus_to_move_address(&a2));
    }

    #[test]
    fn distinct_addresses_produce_distinct_move_addresses() {
        let a = nexus_to_move_address(&addr(0x01));
        let b = nexus_to_move_address(&addr(0x02));
        assert_ne!(a, b);
    }
}
