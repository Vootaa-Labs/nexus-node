// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Move package metadata — structured contract deployment artifacts.
//!
//! Follows TLD-09 §4 package layout specification:
//! - Every published contract stores [`PackageMetadata`] alongside its bytecode.
//! - Metadata includes named-address bindings, per-module hashes, ABI hash,
//!   and upgrade policy.
//! - Stored BCS-encoded under `MODULE_METADATA_KEY` at the contract address.

use nexus_primitives::AccountAddress;
use serde::{Deserialize, Serialize};

// ── Storage key ─────────────────────────────────────────────────────────

/// Storage key for the serialised [`PackageMetadata`] under the contract
/// address.
pub(crate) const MODULE_METADATA_KEY: &[u8] = b"package_metadata";

// ── Upgrade policy ──────────────────────────────────────────────────────

/// Controls whether and how a published package may be upgraded.
///
/// TLD-09 §5.2:
/// - Default is `Immutable` unless the package explicitly opts in.
/// - `Compatible` requires ABI compatibility verification.
/// - `GovernanceOnly` cannot be published via normal user transactions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum UpgradePolicy {
    /// Package code cannot be changed after publish.
    Immutable,
    /// Package may be upgraded if the new version is ABI-compatible.
    Compatible,
    /// Package may only be upgraded through governance transactions.
    GovernanceOnly,
}

impl Default for UpgradePolicy {
    fn default() -> Self {
        Self::Immutable
    }
}

// ── Package metadata ────────────────────────────────────────────────────

/// Structured metadata for a published Move package.
///
/// Stored alongside the bytecode at the contract address under
/// [`MODULE_METADATA_KEY`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct PackageMetadata {
    /// Human-readable package name (e.g. `"counter"`).
    pub name: String,
    /// BLAKE3 hash of the entire package (concatenated module bytecodes).
    pub package_hash: [u8; 32],
    /// Named address bindings (name → resolved address).
    pub named_addresses: Vec<(String, AccountAddress)>,
    /// Per-module hashes: `(module_name, BLAKE3_digest)`.
    pub module_hashes: Vec<(String, [u8; 32])>,
    /// BLAKE3 hash of the serialised ABI.
    pub abi_hash: [u8; 32],
    /// Upgrade policy for this package.
    pub upgrade_policy: UpgradePolicy,
    /// Deployer address.
    pub deployer: AccountAddress,
    /// Deployment version (incremented on compatible upgrades).
    pub version: u64,
}

/// Encode package metadata to BCS bytes.
pub(crate) fn encode_metadata(meta: &PackageMetadata) -> Result<Vec<u8>, bcs::Error> {
    bcs::to_bytes(meta)
}

/// Decode package metadata from BCS bytes.
#[allow(dead_code)]
pub(crate) fn decode_metadata(bytes: &[u8]) -> Result<PackageMetadata, bcs::Error> {
    bcs::from_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_metadata() -> PackageMetadata {
        PackageMetadata {
            name: "counter".into(),
            package_hash: [0xAA; 32],
            named_addresses: vec![("counter_addr".into(), AccountAddress([0xBB; 32]))],
            module_hashes: vec![("counter".into(), [0xCC; 32])],
            abi_hash: [0xDD; 32],
            upgrade_policy: UpgradePolicy::Immutable,
            deployer: AccountAddress([0xEE; 32]),
            version: 1,
        }
    }

    #[test]
    fn metadata_round_trip() {
        let meta = sample_metadata();
        let encoded = encode_metadata(&meta).expect("encode");
        let decoded = decode_metadata(&encoded).expect("decode");
        assert_eq!(meta, decoded);
    }

    #[test]
    fn upgrade_policy_default_is_immutable() {
        assert_eq!(UpgradePolicy::default(), UpgradePolicy::Immutable);
    }
}
