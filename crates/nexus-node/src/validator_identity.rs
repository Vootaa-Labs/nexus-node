// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Validator identity registry — maps `AccountAddress` to cryptographic keys.
//!
//! Populated at genesis from the validator set, and potentially updated
//! when new validators register through the on-chain staking contract.
//! This registry is the authoritative source for the
//! `AccountAddress → FalconVerifyKey` mapping needed by
//! [`crate::staking_snapshot::election_to_committee`].
//!
//! # Storage layout
//!
//! All entries live in `cf_state` under the `__validator_identity__:` prefix.
//!
//! | Key | Value (BCS) |
//! |-----|-------------|
//! | `__validator_identity__:{addr_32}` | `PersistedValidatorIdentity` |
//! | `__validator_identity_count__` | `u32` |

#![forbid(unsafe_code)]

use anyhow::Context;
use nexus_crypto::falcon::FalconVerifyKey;
use nexus_primitives::AccountAddress;
use nexus_storage::traits::{StateStorage, WriteBatchOps};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Column family used for identity storage.
const CF: &str = "cf_state";

const PREFIX_IDENTITY: &[u8] = b"__validator_identity__:";
const KEY_IDENTITY_COUNT: &[u8] = b"__validator_identity_count__";

fn identity_key(addr: &AccountAddress) -> Vec<u8> {
    let mut key = PREFIX_IDENTITY.to_vec();
    key.extend_from_slice(&addr.0);
    key
}

/// Serialisable identity record persisted to storage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedValidatorIdentity {
    /// Raw bytes of the Falcon-512 verify key.
    pub falcon_key_bytes: Vec<u8>,
    /// Epoch at which this identity was first registered.
    pub registered_at_epoch: u64,
}

/// In-memory validator identity registry.
///
/// Thread-safe (via interior `RwLock`) so it can be shared between
/// the execution bridge (writer at genesis/staking events) and the
/// staking snapshot provider (reader at election time).
#[derive(Clone)]
pub struct ValidatorIdentityRegistry {
    inner: Arc<RwLock<HashMap<AccountAddress, FalconVerifyKey>>>,
}

impl Default for ValidatorIdentityRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ValidatorIdentityRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Populate the registry from the genesis committee.
    ///
    /// Each validator's committee index is used to derive a deterministic
    /// address (index as big-endian u32 in the last 4 bytes of a zeroed
    /// 32-byte array). This matches the address derivation used in the
    /// staking snapshot flow.
    pub fn seed_from_committee(&self, committee: &nexus_consensus::Committee) {
        let mut map = self.inner.write().expect("identity registry lock");
        for v in committee.all_validators() {
            let addr = address_from_validator_index(v.index);
            map.insert(addr, v.falcon_pub_key.clone());
        }
    }

    /// Register (or update) a single validator identity.
    pub fn register(&self, addr: AccountAddress, key: FalconVerifyKey) {
        let mut map = self.inner.write().expect("identity registry lock");
        map.insert(addr, key);
    }

    /// Look up the Falcon verify key for a given address.
    pub fn lookup(&self, addr: &AccountAddress) -> Option<FalconVerifyKey> {
        let map = self.inner.read().expect("identity registry lock");
        map.get(addr).cloned()
    }

    /// Return the number of registered identities.
    pub fn len(&self) -> usize {
        let map = self.inner.read().expect("identity registry lock");
        map.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Return all registered (address, key) pairs.
    pub fn all_entries(&self) -> Vec<(AccountAddress, FalconVerifyKey)> {
        let map = self.inner.read().expect("identity registry lock");
        map.iter().map(|(a, k)| (*a, k.clone())).collect()
    }
}

/// Derive a deterministic `AccountAddress` from a validator committee index.
///
/// Layout: 32 zero bytes with the index encoded as big-endian u32 in the
/// last 4 positions. This is the canonical mapping used throughout the
/// staking snapshot and election pipeline.
pub fn address_from_validator_index(index: nexus_primitives::ValidatorIndex) -> AccountAddress {
    let mut bytes = [0u8; 32];
    bytes[28..32].copy_from_slice(&index.0.to_be_bytes());
    AccountAddress(bytes)
}

// ── Persistence ──────────────────────────────────────────────────────────────

/// Persist the entire identity registry to storage in an atomic batch.
pub fn persist_identity_registry<S: StateStorage>(
    store: &S,
    registry: &ValidatorIdentityRegistry,
) -> anyhow::Result<()> {
    let entries = registry.all_entries();
    let mut batch = store.new_batch();

    for (addr, key) in &entries {
        let record = PersistedValidatorIdentity {
            falcon_key_bytes: key.as_bytes().to_vec(),
            registered_at_epoch: 0,
        };
        batch.put_cf(
            CF,
            identity_key(addr),
            bcs::to_bytes(&record).context("BCS encode identity")?,
        );
    }
    batch.put_cf(
        CF,
        KEY_IDENTITY_COUNT.to_vec(),
        bcs::to_bytes(&(entries.len() as u32)).context("BCS encode identity count")?,
    );

    futures::executor::block_on(store.write_batch(batch)).context("write identity batch")?;
    Ok(())
}

/// Load the identity registry from storage.
///
/// Scans `cf_state` for keys matching the identity prefix and
/// reconstructs the in-memory map.
pub fn load_identity_registry<S: StateStorage>(
    store: &S,
) -> anyhow::Result<ValidatorIdentityRegistry> {
    let registry = ValidatorIdentityRegistry::new();

    // Read the count to know if there are any entries at all.
    let count_bytes = store
        .get_sync(CF, KEY_IDENTITY_COUNT)
        .context("read identity count")?;
    let count: u32 = match count_bytes {
        Some(b) => bcs::from_bytes(&b).unwrap_or(0),
        None => return Ok(registry),
    };

    if count == 0 {
        return Ok(registry);
    }

    // Scan identities using range scan with prefix bounds.
    let mut end = PREFIX_IDENTITY.to_vec();
    // Increment last byte to create an exclusive upper bound.
    *end.last_mut().expect("non-empty prefix") += 1;
    let pairs = store
        .scan(CF, PREFIX_IDENTITY, &end)
        .context("identity range scan")?;

    let mut map = registry.inner.write().expect("identity registry lock");
    for (key, value) in pairs {
        if key.len() != PREFIX_IDENTITY.len() + 32 {
            continue;
        }
        let mut addr_bytes = [0u8; 32];
        addr_bytes.copy_from_slice(&key[PREFIX_IDENTITY.len()..]);
        let addr = AccountAddress(addr_bytes);

        let record: PersistedValidatorIdentity =
            bcs::from_bytes(&value).context("BCS decode identity")?;
        let falcon_key = FalconVerifyKey::from_bytes(&record.falcon_key_bytes)
            .context("decode Falcon key from identity store")?;
        map.insert(addr, falcon_key);
    }
    drop(map);

    Ok(registry)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_primitives::ValidatorIndex;

    #[test]
    fn address_from_index_zero() {
        let addr = address_from_validator_index(ValidatorIndex(0));
        assert_eq!(addr.0[28..32], [0, 0, 0, 0]);
        assert_eq!(addr.0[0..28], [0u8; 28]);
    }

    #[test]
    fn address_from_index_one() {
        let addr = address_from_validator_index(ValidatorIndex(1));
        assert_eq!(addr.0[28..32], [0, 0, 0, 1]);
    }

    #[test]
    fn address_from_index_large() {
        let addr = address_from_validator_index(ValidatorIndex(256));
        assert_eq!(addr.0[28..32], [0, 0, 1, 0]);
    }

    #[test]
    fn registry_seed_and_lookup() {
        use nexus_crypto::{FalconSigner, Signer};
        let registry = ValidatorIdentityRegistry::new();
        assert!(registry.is_empty());

        // Insert a dummy entry.
        let addr = address_from_validator_index(ValidatorIndex(0));
        let (_, dummy_key) = FalconSigner::generate_keypair();
        registry.register(addr, dummy_key.clone());

        assert_eq!(registry.len(), 1);
        let found = registry.lookup(&addr).expect("should find key");
        assert_eq!(found.as_bytes(), dummy_key.as_bytes());
    }

    #[test]
    fn registry_lookup_missing() {
        let registry = ValidatorIdentityRegistry::new();
        let addr = address_from_validator_index(ValidatorIndex(42));
        assert!(registry.lookup(&addr).is_none());
    }

    #[test]
    fn registry_update_overwrites() {
        use nexus_crypto::{FalconSigner, Signer};
        let registry = ValidatorIdentityRegistry::new();
        let addr = address_from_validator_index(ValidatorIndex(0));
        let (_, key1) = FalconSigner::generate_keypair();
        let (_, key2) = FalconSigner::generate_keypair();

        registry.register(addr, key1.clone());
        assert_eq!(registry.lookup(&addr).unwrap().as_bytes(), key1.as_bytes());

        registry.register(addr, key2.clone());
        assert_eq!(registry.lookup(&addr).unwrap().as_bytes(), key2.as_bytes());
    }

    #[test]
    fn all_entries_returns_all_registered_keys() {
        use nexus_crypto::{FalconSigner, Signer};
        let registry = ValidatorIdentityRegistry::new();
        assert!(registry.all_entries().is_empty());

        let addr0 = address_from_validator_index(ValidatorIndex(0));
        let addr1 = address_from_validator_index(ValidatorIndex(1));
        let (_, key0) = FalconSigner::generate_keypair();
        let (_, key1) = FalconSigner::generate_keypair();

        registry.register(addr0, key0);
        registry.register(addr1, key1);

        let entries = registry.all_entries();
        assert_eq!(entries.len(), 2);
        assert!(!registry.is_empty());
    }

    #[test]
    fn identity_key_prefix_includes_address() {
        let addr = address_from_validator_index(ValidatorIndex(0));
        let key_bytes = identity_key(&addr);
        assert!(key_bytes.starts_with(b"__validator_identity__:"));
        assert_eq!(&key_bytes[b"__validator_identity__:".len()..], &addr.0[..]);
    }
}
