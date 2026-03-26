//! Staking snapshot provider — builds a `StakingSnapshot` from committed
//! chain state at epoch boundaries.
//!
//! The provider reads validator identities from the
//! [`ValidatorIdentityRegistry`] and enriches them with consensus-layer
//! slash and reputation state from the live engine committee.
//!
//! # Two-tier data source
//!
//! 1. **Canonical staking state** (Move contract): `ValidatorStake`
//!    resources stored under each validator's account address. When the
//!    staking contract is deployed, the provider reads actual on-chain
//!    bonded/penalty/status data.
//!
//! 2. **Genesis-derived fallback**: when no staking contract is deployed
//!    (pre-staking-deployment epoch), the provider synthesises stake
//!    records from the genesis committee's stake assignments enriched
//!    with live consensus slash/reputation state.
//!
//! Both paths produce the same `StakingSnapshot` type consumed by
//! [`crate::staking_snapshot::attempt_rotation`].

#![forbid(unsafe_code)]

use std::sync::{Arc, Mutex};

use nexus_consensus::ConsensusEngine;
use nexus_primitives::{AccountAddress, EpochNumber};
use nexus_storage::traits::StateStorage;
use tracing::{debug, warn};

use crate::staking_snapshot::{StakingSnapshot, ValidatorStakeRecord};
use crate::validator_identity::{address_from_validator_index, ValidatorIdentityRegistry};

/// Build a staking snapshot provider closure suitable for passing to
/// [`crate::execution_bridge::spawn_execution_bridge`].
///
/// The returned closure captures the consensus engine, storage, and
/// identity registry. When invoked (at epoch boundaries), it reads the
/// current committee state and attempts to build a canonical staking
/// snapshot.
pub fn build_snapshot_provider<S: StateStorage + 'static>(
    engine: Arc<Mutex<ConsensusEngine>>,
    store: S,
    identity_registry: Arc<ValidatorIdentityRegistry>,
) -> Arc<dyn Fn() -> Option<StakingSnapshot> + Send + Sync> {
    Arc::new(move || snapshot_from_engine_and_state(&engine, &store, &identity_registry))
}

/// Attempt to build a staking snapshot.
///
/// **Priority order**:
/// 1. Try reading on-chain staking contract state (when deployed).
/// 2. Fall back to genesis-derived records enriched with live consensus
///    slash/reputation data.
fn snapshot_from_engine_and_state<S: StateStorage>(
    engine: &Arc<Mutex<ConsensusEngine>>,
    store: &S,
    identity_registry: &ValidatorIdentityRegistry,
) -> Option<StakingSnapshot> {
    let eng = engine.lock().ok()?;
    let epoch = eng.epoch();
    let committee = eng.committee();
    let validators = committee.all_validators();

    if validators.is_empty() {
        warn!("snapshot provider: empty committee, cannot build snapshot");
        return None;
    }

    // Try on-chain staking state first.
    let on_chain = try_read_on_chain_staking(store, identity_registry, epoch);
    if let Some(snapshot) = on_chain {
        debug!(
            epoch = epoch.0,
            records = snapshot.records.len(),
            "snapshot provider: built snapshot from on-chain staking state"
        );
        return Some(enrich_with_consensus_state(snapshot, validators));
    }

    // Fallback: derive from current committee.
    let records: Vec<ValidatorStakeRecord> = validators
        .iter()
        .map(|v| {
            let addr = address_from_validator_index(v.index);
            ValidatorStakeRecord {
                address: addr,
                bonded: v.stake.0,
                penalty_total: 0,
                status: if v.is_slashed { 2 } else { 0 },
                registered_epoch: 0,
                unbond_epoch: 0,
                is_slashed: v.is_slashed,
                reputation: v.reputation,
            }
        })
        .collect();

    debug!(
        epoch = epoch.0,
        records = records.len(),
        "snapshot provider: built snapshot from genesis committee (fallback)"
    );
    Some(StakingSnapshot::new(epoch, records))
}

/// Try to read staking state from the on-chain Move contract.
///
/// The staking contract stores `ValidatorStake` resources under each
/// validator's account address. We iterate known identities and attempt
/// to read their staking records from `cf_state`.
fn try_read_on_chain_staking<S: StateStorage>(
    store: &S,
    identity_registry: &ValidatorIdentityRegistry,
    captured_at_epoch: EpochNumber,
) -> Option<StakingSnapshot> {
    let entries = identity_registry.all_entries();
    if entries.is_empty() {
        return None;
    }

    let mut records = Vec::with_capacity(entries.len());
    let mut found_any = false;

    for (addr, _key) in &entries {
        if let Some(record) = read_validator_stake_record(store, addr) {
            found_any = true;
            records.push(record);
        }
    }

    if !found_any {
        return None;
    }

    Some(StakingSnapshot::new(captured_at_epoch, records))
}

/// Read a single validator's staking record from Move contract state.
///
/// The Move resource is stored under the validator's account address
/// with the staking module's resource key. Returns `None` if the record
/// doesn't exist (validator not registered in staking contract).
fn read_validator_stake_record<S: StateStorage>(
    store: &S,
    addr: &AccountAddress,
) -> Option<ValidatorStakeRecord> {
    // The Move VM stores resources in cf_state using a composite key:
    //   shard_prefix(4) + account_address(32) + resource_tag
    //
    // For the staking module, the resource tag depends on the module
    // address and struct name. We use a well-known prefix pattern.
    let staking_key = build_staking_resource_key(addr);

    let value = store.get_sync("cf_state", &staking_key).ok()??;

    // Deserialise the Move resource. The BCS layout of ValidatorStake is:
    //   bonded: u64 | penalty_total: u64 | status: u8 |
    //   registered_epoch: u64 | unbond_epoch: u64 | metadata_tag: u64
    parse_validator_stake_bcs(addr, &value)
}

/// Build the storage key used by the Move VM for a validator's staking
/// resource.
///
/// Layout matches the Move-compatible resource storage format:
/// `{shard_id(4)}{address(32)}{resource_tag}`.
pub fn build_staking_resource_key(addr: &AccountAddress) -> Vec<u8> {
    // Shard 0 prefix (4 bytes BE).
    let mut key = vec![0u8; 4];
    key.extend_from_slice(&addr.0);
    // Resource tag for staking::ValidatorStake.
    // The tag is: module_address(32) + module_name_len(ULEB) + module_name
    //           + struct_name_len(ULEB) + struct_name + type_params_count(ULEB)
    //
    // Staking contract address = 0xCAFE (deployed at dev address).
    let mut tag = Vec::new();
    // Module address (32 bytes, 0xCAFE in last 2 bytes).
    let mut mod_addr = [0u8; 32];
    mod_addr[30] = 0xCA;
    mod_addr[31] = 0xFE;
    tag.extend_from_slice(&mod_addr);
    // Module name: "staking" (length 7).
    tag.push(7);
    tag.extend_from_slice(b"staking");
    // Struct name: "ValidatorStake" (length 14).
    tag.push(14);
    tag.extend_from_slice(b"ValidatorStake");
    // Type params count: 0.
    tag.push(0);

    key.extend_from_slice(&tag);
    key
}

/// Parse BCS-encoded ValidatorStake resource from Move contract state.
fn parse_validator_stake_bcs(addr: &AccountAddress, data: &[u8]) -> Option<ValidatorStakeRecord> {
    // BCS layout: u64 + u64 + u8 + u64 + u64 + u64 = 41 bytes
    if data.len() < 41 {
        return None;
    }
    let bonded = u64::from_le_bytes(data[0..8].try_into().ok()?);
    let penalty_total = u64::from_le_bytes(data[8..16].try_into().ok()?);
    let status = data[16];
    let registered_epoch = u64::from_le_bytes(data[17..25].try_into().ok()?);
    let unbond_epoch = u64::from_le_bytes(data[25..33].try_into().ok()?);
    // metadata_tag at [33..41] — not needed for staking snapshot.

    Some(ValidatorStakeRecord {
        address: *addr,
        bonded,
        penalty_total,
        status,
        registered_epoch,
        unbond_epoch,
        is_slashed: false,
        reputation: nexus_consensus::types::ReputationScore::default(),
    })
}

/// Enrich a staking snapshot with slash/reputation data from the
/// live consensus committee.
fn enrich_with_consensus_state(
    mut snapshot: StakingSnapshot,
    validators: &[nexus_consensus::types::ValidatorInfo],
) -> StakingSnapshot {
    // Build a lookup from falcon key bytes → (is_slashed, reputation).
    let consensus_state: std::collections::HashMap<
        Vec<u8>,
        (bool, nexus_consensus::types::ReputationScore),
    > = validators
        .iter()
        .map(|v| {
            let addr = address_from_validator_index(v.index);
            (addr.0.to_vec(), (v.is_slashed, v.reputation))
        })
        .collect();

    for record in &mut snapshot.records {
        if let Some(&(slashed, rep)) = consensus_state.get(record.address.0.as_slice()) {
            record.is_slashed = slashed;
            record.reputation = rep;
        }
    }

    snapshot
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_consensus::types::ReputationScore;
    use nexus_primitives::{Amount, ValidatorIndex};

    fn addr(b: u8) -> AccountAddress {
        address_from_validator_index(ValidatorIndex(b as u32))
    }

    #[test]
    fn build_staking_resource_key_is_deterministic() {
        let addr1 = addr(0);
        let addr2 = addr(0);
        let key1 = build_staking_resource_key(&addr1);
        let key2 = build_staking_resource_key(&addr2);
        assert_eq!(key1, key2);
    }

    #[test]
    fn build_staking_resource_key_different_addrs() {
        let key1 = build_staking_resource_key(&addr(0));
        let key2 = build_staking_resource_key(&addr(1));
        assert_ne!(key1, key2);
    }

    #[test]
    fn parse_valid_bcs() {
        let a = addr(1);
        let mut data = Vec::new();
        data.extend_from_slice(&2_000_000_000u64.to_le_bytes()); // bonded
        data.extend_from_slice(&100_000_000u64.to_le_bytes()); // penalty_total
        data.push(0u8); // status = Active
        data.extend_from_slice(&5u64.to_le_bytes()); // registered_epoch
        data.extend_from_slice(&0u64.to_le_bytes()); // unbond_epoch
        data.extend_from_slice(&0u64.to_le_bytes()); // metadata_tag

        let record = parse_validator_stake_bcs(&a, &data).unwrap();
        assert_eq!(record.bonded, 2_000_000_000);
        assert_eq!(record.penalty_total, 100_000_000);
        assert_eq!(record.status, 0);
        assert_eq!(record.registered_epoch, 5);
        assert!(record.is_active());
        assert_eq!(record.effective_stake(), 1_900_000_000);
    }

    #[test]
    fn parse_too_short_returns_none() {
        let a = addr(0);
        let data = vec![0u8; 20]; // too short
        assert!(parse_validator_stake_bcs(&a, &data).is_none());
    }

    #[test]
    fn enrich_applies_slash_state() {
        let a = addr(0);
        let snapshot = StakingSnapshot::new(
            EpochNumber(5),
            vec![ValidatorStakeRecord {
                address: a,
                bonded: 2_000_000_000,
                penalty_total: 0,
                status: 0,
                registered_epoch: 0,
                unbond_epoch: 0,
                is_slashed: false,
                reputation: ReputationScore::default(),
            }],
        );

        let slashed_validators = vec![nexus_consensus::types::ValidatorInfo {
            index: ValidatorIndex(0),
            falcon_pub_key: {
                use nexus_crypto::{FalconSigner, Signer};
                let (_, vk) = FalconSigner::generate_keypair();
                vk
            },
            stake: Amount(2_000_000_000),
            reputation: ReputationScore::from_f32(0.5),
            is_slashed: true,
            shard_id: None,
        }];
        let enriched = enrich_with_consensus_state(snapshot, &slashed_validators);
        assert!(enriched.records[0].is_slashed);
        assert_eq!(
            enriched.records[0].reputation,
            ReputationScore::from_f32(0.5)
        );
    }
}
