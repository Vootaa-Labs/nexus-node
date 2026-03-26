// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Account and contract addresses, token identifiers, and the `Amount` type.
//!
//! # Address derivation
//!
//! All Nexus addresses are deterministically derived via domain-separated BLAKE3:
//!
//! - **Account address**: `BLAKE3(b"nexus::account::address::v1" || ML-DSA-pubkey-bytes)`
//! - **Contract address**: `BLAKE3(b"nexus::contract::address::v1" || deployer-addr || bytecode-hash)`

use std::fmt;

use crate::error::DigestDecodeError;
use serde::{Deserialize, Serialize};

/// Nexus account address — 32 bytes derived from the owner's ML-DSA public key.
///
/// BCS serialization: exactly 32 bytes with no length prefix.
/// JSON serialization: lowercase hex string (64 characters).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct AccountAddress(
    /// Raw 32-byte address.
    pub [u8; 32],
);

impl Serialize for AccountAddress {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if serializer.is_human_readable() {
            serializer.serialize_str(&self.to_hex())
        } else {
            self.0.serialize(serializer)
        }
    }
}

impl<'de> Deserialize<'de> for AccountAddress {
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

impl AccountAddress {
    /// All-zero address. **Must not** appear on production paths; for test use only.
    pub const ZERO: Self = Self([0u8; 32]);

    /// Derive a deterministic account address from an ML-DSA (Dilithium) public key.
    ///
    /// Address = `BLAKE3(b"nexus::account::address::v1" || pk_bytes)`
    pub fn from_dilithium_pubkey(pk_bytes: &[u8]) -> Self {
        let mut h = blake3::Hasher::new();
        h.update(b"nexus::account::address::v1");
        h.update(pk_bytes);
        Self(*h.finalize().as_bytes())
    }

    /// Encode as a lowercase hex string (64 characters).
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Decode from a lowercase hex string.
    ///
    /// # Errors
    /// Returns [`DigestDecodeError::InvalidHex`] if the string is not valid hex
    /// or not exactly 64 characters long.
    pub fn from_hex(s: &str) -> Result<Self, DigestDecodeError> {
        let mut bytes = [0u8; 32];
        hex::decode_to_slice(s, &mut bytes).map_err(|e| DigestDecodeError::InvalidHex {
            reason: e.to_string(),
        })?;
        Ok(Self(bytes))
    }
}

impl AsRef<[u8]> for AccountAddress {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for AccountAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hex = self.to_hex();
        write!(f, "AccountAddress({}…{})", &hex[..8], &hex[56..])
    }
}

impl fmt::Display for AccountAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{}", self.to_hex())
    }
}

// ─────────────────────────────────────────────────────────────────────────────

/// Nexus contract address — 32 bytes derived from deployer address and bytecode hash.
///
/// BCS serialization: exactly 32 bytes with no length prefix.
/// JSON serialization: lowercase hex string (64 characters).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContractAddress(
    /// Raw 32-byte contract address.
    pub [u8; 32],
);

impl Serialize for ContractAddress {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if serializer.is_human_readable() {
            serializer.serialize_str(&self.to_hex())
        } else {
            self.0.serialize(serializer)
        }
    }
}

impl<'de> Deserialize<'de> for ContractAddress {
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

impl ContractAddress {
    /// All-zero contract address. **Must not** appear on production paths; for test use only.
    pub const ZERO: Self = Self([0u8; 32]);

    /// Derive a deterministic contract address.
    ///
    /// Address = `BLAKE3(b"nexus::contract::address::v1" || deployer-addr || bytecode_hash)`
    pub fn from_deployment(deployer: &AccountAddress, bytecode_hash: &[u8]) -> Self {
        let mut h = blake3::Hasher::new();
        h.update(b"nexus::contract::address::v1");
        h.update(&deployer.0);
        h.update(bytecode_hash);
        Self(*h.finalize().as_bytes())
    }

    /// Encode as a lowercase hex string (64 characters).
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Decode from a lowercase hex string.
    ///
    /// # Errors
    /// Returns [`DigestDecodeError::InvalidHex`] if the string is not valid hex
    /// or not exactly 64 characters long.
    pub fn from_hex(s: &str) -> Result<Self, DigestDecodeError> {
        let mut bytes = [0u8; 32];
        hex::decode_to_slice(s, &mut bytes).map_err(|e| DigestDecodeError::InvalidHex {
            reason: e.to_string(),
        })?;
        Ok(Self(bytes))
    }
}

impl AsRef<[u8]> for ContractAddress {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for ContractAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hex = self.to_hex();
        write!(f, "ContractAddress({}…{})", &hex[..8], &hex[56..])
    }
}

impl fmt::Display for ContractAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{}", self.to_hex())
    }
}

// ─────────────────────────────────────────────────────────────────────────────

/// Token identifier — either the platform native token or a contract-defined token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TokenId {
    /// Platform-native token (NXS).
    Native,
    /// Contract-defined token (Move Coin Resource).
    Contract(ContractAddress),
}

// ─────────────────────────────────────────────────────────────────────────────

/// Token amount in the smallest representable unit (voo).
///
/// Precision: `1 NXS = 10^9 voo`.
/// `u64::MAX ≈ 18.44 × 10^9 NXS` — sufficient for all foreseeable supply.
///
/// This type intentionally does **not** implement `Add`/`Sub` to prevent
/// accidental unchecked arithmetic; callers should use saturating or
/// checked arithmetic explicitly.
///
/// Serialized as 8 bytes (u64 little-endian) in BCS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Amount(
    /// Raw amount in voo (smallest unit).
    pub u64,
);

impl Amount {
    /// Zero amount.
    pub const ZERO: Self = Self(0);
    /// One voo (the smallest representable unit).
    pub const ONE_VOO: Self = Self(1);
    /// One NXS expressed in voo (10^9 voo).
    pub const ONE_NXS: Self = Self(1_000_000_000);
}

// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // AD-01: AccountAddress derivation is deterministic
    #[test]
    fn account_address_is_deterministic() {
        let pk = b"mock_dilithium_public_key_for_testing";
        let a1 = AccountAddress::from_dilithium_pubkey(pk);
        let a2 = AccountAddress::from_dilithium_pubkey(pk);
        assert_eq!(a1, a2);
    }

    // AD-02: Different public keys produce different addresses
    #[test]
    fn account_address_is_unique_per_pubkey() {
        let a1 = AccountAddress::from_dilithium_pubkey(b"pubkey_alice");
        let a2 = AccountAddress::from_dilithium_pubkey(b"pubkey_bob");
        assert_ne!(
            a1, a2,
            "different public keys must yield different addresses"
        );
    }

    // AD-03: to_hex / from_hex roundtrip
    #[test]
    fn account_address_hex_roundtrip() {
        let addr = AccountAddress::from_dilithium_pubkey(b"test_key_material");
        let hex = addr.to_hex();
        assert_eq!(hex.len(), 64);
        let addr2 = AccountAddress::from_hex(&hex).expect("valid hex");
        assert_eq!(addr, addr2);
    }

    #[test]
    fn account_address_from_hex_rejects_invalid() {
        assert!(AccountAddress::from_hex("not_hex").is_err());
        assert!(AccountAddress::from_hex("deadbeef").is_err()); // too short
    }

    // AD-04: ContractAddress derivation is deterministic
    #[test]
    fn contract_address_is_deterministic() {
        let deployer = AccountAddress::from_dilithium_pubkey(b"deployer_key");
        let bytecode_hash = [0xcc_u8; 32];
        let c1 = ContractAddress::from_deployment(&deployer, &bytecode_hash);
        let c2 = ContractAddress::from_deployment(&deployer, &bytecode_hash);
        assert_eq!(c1, c2);
    }

    #[test]
    fn contract_address_differs_from_account_address() {
        let deployer = AccountAddress::from_dilithium_pubkey(b"deployer_key");
        let contract = ContractAddress::from_deployment(&deployer, b"bytecode");
        // Different domain separators must yield different values for the same input
        assert_ne!(deployer.0, contract.0);
    }

    // AD-05: Amount::ZERO is zero
    #[test]
    fn amount_zero_is_zero() {
        assert_eq!(Amount::ZERO.0, 0u64);
    }

    // AD-06: Amount::ONE_NXS is 10^9 voo
    #[test]
    fn amount_one_nxs_is_correct() {
        assert_eq!(Amount::ONE_NXS.0, 1_000_000_000u64);
    }

    // AD-06b: Amount::ONE_VOO is 1
    #[test]
    fn amount_one_voo_is_correct() {
        assert_eq!(Amount::ONE_VOO.0, 1u64);
    }

    // AD-07: TokenId works as a HashMap key
    #[test]
    fn token_id_as_hashmap_key() {
        let mut map: HashMap<TokenId, &str> = HashMap::new();
        map.insert(TokenId::Native, "NXS");
        assert_eq!(map[&TokenId::Native], "NXS");
    }

    // AD-08: AccountAddress::ZERO is all zeros
    #[test]
    fn account_address_zero_is_all_zeros() {
        assert_eq!(AccountAddress::ZERO.0, [0u8; 32]);
    }

    // AccountAddress Display / Debug
    #[test]
    fn account_address_display_has_0x_prefix() {
        let addr = AccountAddress::ZERO;
        let s = format!("{addr}");
        assert!(s.starts_with("0x"), "Display should start with 0x");
        assert_eq!(s.len(), 66, "0x + 64 hex chars");
    }

    // BCS roundtrip for Amount
    #[test]
    fn amount_bcs_roundtrip() {
        let a = Amount::ONE_NXS;
        let bytes = bcs::to_bytes(&a).expect("bcs serialize");
        let a2: Amount = bcs::from_bytes(&bytes).expect("bcs deserialize");
        assert_eq!(a, a2);
    }

    // AD-09: AccountAddress JSON serialization uses hex string
    #[test]
    fn account_address_json_roundtrip() {
        let addr = AccountAddress::from_dilithium_pubkey(b"test_key_for_json");
        let json = serde_json::to_string(&addr).expect("json serialize");
        // JSON should be a quoted hex string, not an array of numbers
        assert!(json.starts_with('"'), "JSON should be a string: {json}");
        assert_eq!(json.len(), 66, "quotes + 64 hex chars");
        let addr2: AccountAddress = serde_json::from_str(&json).expect("json deserialize");
        assert_eq!(addr, addr2);
    }

    // AD-10: AccountAddress BCS roundtrip (raw bytes, not hex)
    #[test]
    fn account_address_bcs_roundtrip() {
        let addr = AccountAddress::from_dilithium_pubkey(b"bcs_test_key");
        let bytes = bcs::to_bytes(&addr).expect("bcs serialize");
        assert_eq!(bytes.len(), 32, "BCS should be exactly 32 bytes");
        let addr2: AccountAddress = bcs::from_bytes(&bytes).expect("bcs deserialize");
        assert_eq!(addr, addr2);
    }

    // AD-11: ContractAddress JSON serialization uses hex string
    #[test]
    fn contract_address_json_roundtrip() {
        let deployer = AccountAddress::from_dilithium_pubkey(b"deployer_json");
        let ca = ContractAddress::from_deployment(&deployer, b"code_hash");
        let json = serde_json::to_string(&ca).expect("json serialize");
        assert!(json.starts_with('"'), "JSON should be a string: {json}");
        let ca2: ContractAddress = serde_json::from_str(&json).expect("json deserialize");
        assert_eq!(ca, ca2);
    }

    // AD-12: ContractAddress BCS roundtrip
    #[test]
    fn contract_address_bcs_roundtrip() {
        let deployer = AccountAddress::from_dilithium_pubkey(b"deployer_bcs");
        let ca = ContractAddress::from_deployment(&deployer, b"code");
        let bytes = bcs::to_bytes(&ca).expect("bcs serialize");
        assert_eq!(bytes.len(), 32);
        let ca2: ContractAddress = bcs::from_bytes(&bytes).expect("bcs deserialize");
        assert_eq!(ca, ca2);
    }
}
