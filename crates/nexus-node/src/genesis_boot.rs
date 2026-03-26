//! Genesis loading and chain bootstrapping.
//!
//! Reads the genesis JSON file, validates it, and seeds the storage
//! layer and consensus committee for first boot.
//!
//! # Atomicity
//!
//! All allocations and the genesis-applied marker are written in a
//! **single RocksDB write batch**. If the write fails, no marker is
//! stored, and the next startup will re-attempt the full import.
//! If the marker already exists (re-start after successful import),
//! the allocation step is skipped.

#![forbid(unsafe_code)]

use std::path::Path;

use anyhow::Context;
use nexus_config::genesis::{GenesisConfig, GenesisValidatorEntry};
use nexus_consensus::types::{ReputationScore, ValidatorInfo};
use nexus_consensus::Committee;
use nexus_crypto::falcon::FalconVerifyKey;
use nexus_primitives::{AccountAddress, EpochNumber, ShardId, ValidatorIndex};
use nexus_storage::{ColumnFamily, StateStorage, WriteBatchOps};

/// Well-known key in `cf_state` that marks genesis as fully applied.
///
/// Value is the genesis chain-id encoded as UTF-8.
const GENESIS_MARKER_KEY: &[u8] = b"__nexus_genesis_applied__";

/// Outcome of genesis bootstrapping — everything the node needs to start.
#[derive(Debug)]
pub struct GenesisBootResult {
    /// The consensus committee built from genesis validators.
    pub committee: Committee,
    /// Number of execution shards specified in genesis.
    pub num_shards: u16,
    /// Chain identifier.
    pub chain_id: String,
}

/// Load and validate a genesis JSON file, then seed the storage layer.
///
/// Returns the consensus committee and chain parameters.
///
/// **Idempotent:** if a previous boot already wrote the genesis marker
/// to storage, allocations are skipped. All writes (allocations +
/// marker) go through a single atomic write batch so partial seeding
/// cannot occur.
pub fn boot_from_genesis<S: StateStorage>(
    genesis_path: &Path,
    store: &S,
    shard_id: ShardId,
) -> anyhow::Result<GenesisBootResult> {
    let content = std::fs::read_to_string(genesis_path)
        .with_context(|| format!("failed to read genesis file: {}", genesis_path.display()))?;
    let genesis: GenesisConfig =
        serde_json::from_str(&content).context("failed to parse genesis JSON")?;
    genesis.validate().context("genesis validation failed")?;

    // ── Build consensus committee from validator entries ─────────────
    let validators: Vec<ValidatorInfo> = genesis
        .validators
        .iter()
        .enumerate()
        .map(|(i, entry)| convert_validator(i, entry))
        .collect::<anyhow::Result<Vec<_>>>()
        .context("failed to convert genesis validators")?;

    let committee =
        Committee::new(EpochNumber(0), validators).context("failed to create genesis committee")?;

    // ── Check if genesis was already applied ─────────────────────────
    let already_applied = is_genesis_applied(store)?;
    if already_applied {
        tracing::info!("genesis marker found — skipping allocation seeding");
    } else {
        seed_allocations_atomic(store, shard_id, &genesis)?;
        tracing::info!(
            allocations = genesis.allocations.len(),
            "genesis allocations seeded atomically"
        );
    }

    Ok(GenesisBootResult {
        committee,
        num_shards: genesis.num_shards,
        chain_id: genesis.chain_id,
    })
}

/// Check whether the genesis marker exists in storage.
pub fn is_genesis_applied<S: StateStorage>(store: &S) -> anyhow::Result<bool> {
    let val = store
        .get_sync(ColumnFamily::State.as_str(), GENESIS_MARKER_KEY)
        .context("failed to read genesis marker from storage")?;
    Ok(val.is_some())
}

/// Convert a genesis validator entry to a consensus `ValidatorInfo`.
fn convert_validator(index: usize, entry: &GenesisValidatorEntry) -> anyhow::Result<ValidatorInfo> {
    let falcon_bytes = hex::decode(&entry.falcon_verify_key_hex)
        .with_context(|| format!("invalid hex for validator {index} falcon key"))?;
    let falcon_pub_key = FalconVerifyKey::from_bytes(&falcon_bytes)
        .with_context(|| format!("invalid Falcon key for validator {index}"))?;

    Ok(ValidatorInfo {
        index: ValidatorIndex(index as u32),
        falcon_pub_key,
        stake: entry.stake,
        reputation: ReputationScore::MAX,
        is_slashed: false,
        shard_id: entry.shard_id,
    })
}

/// Write initial token allocations, genesis staking state, **and** the
/// genesis marker in a single atomic write batch.
///
/// If this function returns `Ok`, the marker is guaranteed to be present.
/// If it returns `Err`, no partial state is left behind.
fn seed_allocations_atomic<S: StateStorage>(
    store: &S,
    shard_id: ShardId,
    genesis: &GenesisConfig,
) -> anyhow::Result<()> {
    let mut batch = store.new_batch();

    for (i, alloc) in genesis.allocations.iter().enumerate() {
        let addr_bytes = hex::decode(&alloc.address_hex)
            .with_context(|| format!("invalid hex address for allocation {i}"))?;
        let mut addr_array = [0u8; 32];
        if addr_bytes.len() != 32 {
            anyhow::bail!(
                "allocation {i}: address hex must be 32 bytes, got {}",
                addr_bytes.len()
            );
        }
        addr_array.copy_from_slice(&addr_bytes);

        let key = nexus_storage::AccountKey {
            shard_id,
            address: AccountAddress(addr_array),
        };
        batch.put_cf(
            ColumnFamily::State.as_str(),
            key.to_bytes(),
            alloc.amount.0.to_le_bytes().to_vec(),
        );
    }

    // ── Seed initial staking state for genesis validators ───────────
    //
    // Each genesis validator gets a `ValidatorStake` BCS record written
    // to the same key the Move staking contract would use. This ensures
    // the staking snapshot provider can read real on-chain staking data
    // from epoch 0, before the contract is actually invoked.
    seed_genesis_staking_records(&mut batch, genesis)?;

    // Write the genesis marker in the same batch — atomic with allocations.
    batch.put_cf(
        ColumnFamily::State.as_str(),
        GENESIS_MARKER_KEY.to_vec(),
        genesis.chain_id.as_bytes().to_vec(),
    );

    futures::executor::block_on(store.write_batch(batch))
        .context("failed to write genesis allocations + marker to storage")?;
    Ok(())
}

/// Write initial `ValidatorStake` BCS records for every genesis validator.
///
/// The BCS layout matches `snapshot_provider::parse_validator_stake_bcs`:
///   bonded(u64) | penalty_total(u64) | status(u8) |
///   registered_epoch(u64) | unbond_epoch(u64) | metadata_tag(u64)
///
/// All values are LE; status = 0 (Active), penalties/unbond = 0.
fn seed_genesis_staking_records(
    batch: &mut impl WriteBatchOps,
    genesis: &GenesisConfig,
) -> anyhow::Result<()> {
    use crate::snapshot_provider::build_staking_resource_key;
    use crate::validator_identity::address_from_validator_index;

    for (i, entry) in genesis.validators.iter().enumerate() {
        let addr = address_from_validator_index(ValidatorIndex(i as u32));
        let key = build_staking_resource_key(&addr);

        // BCS-encoded ValidatorStake: 41 bytes total.
        let mut record = Vec::with_capacity(41);
        record.extend_from_slice(&entry.stake.0.to_le_bytes()); // bonded
        record.extend_from_slice(&0u64.to_le_bytes()); // penalty_total
        record.push(0u8); // status: Active
        record.extend_from_slice(&0u64.to_le_bytes()); // registered_epoch
        record.extend_from_slice(&0u64.to_le_bytes()); // unbond_epoch
        record.extend_from_slice(&0u64.to_le_bytes()); // metadata_tag

        batch.put_cf(ColumnFamily::State.as_str(), key, record);
    }

    tracing::info!(
        validators = genesis.validators.len(),
        "genesis staking state seeded for all validators"
    );
    Ok(())
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use libp2p::PeerId as Libp2pPeerId;
    use nexus_config::genesis::GenesisConfig;
    use nexus_consensus::ValidatorRegistry;
    use nexus_primitives::Amount;
    use nexus_storage::MemoryStore;

    #[test]
    fn boot_from_genesis_with_test_config() {
        let genesis = GenesisConfig::for_testing();
        let store = MemoryStore::new();
        let shard_id = ShardId(0);

        // Write genesis to a temp file.
        let dir = std::env::temp_dir().join("nexus-genesis-boot-test");
        std::fs::create_dir_all(&dir).unwrap();
        let genesis_path = dir.join("genesis.json");
        let json = serde_json::to_string_pretty(&genesis).unwrap();
        std::fs::write(&genesis_path, &json).unwrap();

        // Before boot, marker should not exist.
        assert!(!is_genesis_applied(&store).unwrap());

        let result = boot_from_genesis(&genesis_path, &store, shard_id).unwrap();

        assert_eq!(result.committee.active_validators().len(), 4);
        assert_eq!(result.num_shards, 1);
        assert_eq!(result.chain_id, "nexus-test-0");

        // Verify allocations were seeded.
        let key = nexus_storage::AccountKey {
            shard_id,
            address: AccountAddress::ZERO,
        };
        let raw = store
            .get_sync(ColumnFamily::State.as_str(), &key.to_bytes())
            .unwrap()
            .expect("allocation should be seeded");
        let amount = u64::from_le_bytes(raw.try_into().unwrap());
        assert_eq!(amount, 1_000_000_000);

        // After boot, marker should exist.
        assert!(is_genesis_applied(&store).unwrap());

        // Cleanup.
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn boot_from_genesis_is_idempotent() {
        let genesis = GenesisConfig::for_testing();
        let store = MemoryStore::new();
        let shard_id = ShardId(0);

        let dir = std::env::temp_dir().join("nexus-genesis-idempotent-test");
        std::fs::create_dir_all(&dir).unwrap();
        let genesis_path = dir.join("genesis.json");
        let json = serde_json::to_string_pretty(&genesis).unwrap();
        std::fs::write(&genesis_path, &json).unwrap();

        // First boot.
        let r1 = boot_from_genesis(&genesis_path, &store, shard_id).unwrap();
        assert_eq!(r1.chain_id, "nexus-test-0");

        // Second boot — should skip seeding but still return committee.
        let r2 = boot_from_genesis(&genesis_path, &store, shard_id).unwrap();
        assert_eq!(r2.committee.active_validators().len(), 4);
        assert_eq!(r2.chain_id, "nexus-test-0");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn boot_from_genesis_invalid_file() {
        let path = std::env::temp_dir().join("nexus-no-such-file.json");
        let store = MemoryStore::new();
        let result = boot_from_genesis(&path, &store, ShardId(0));
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("failed to read genesis file"));
    }

    #[test]
    fn boot_from_genesis_invalid_json() {
        let dir = std::env::temp_dir().join("nexus-genesis-bad-json");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("genesis.json");
        std::fs::write(&path, "not valid json {}").unwrap();

        let store = MemoryStore::new();
        let result = boot_from_genesis(&path, &store, ShardId(0));
        assert!(result.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn convert_validator_round_trip() {
        use nexus_crypto::{FalconSigner, Signer};
        let (_, vk) = FalconSigner::generate_keypair();
        let entry = GenesisValidatorEntry {
            name: "test-val".to_owned(),
            network_peer_id: Libp2pPeerId::random().to_string(),
            falcon_verify_key_hex: hex::encode(vk.as_bytes()),
            dilithium_verify_key_hex: hex::encode([0xAA; 32]),
            kyber_encaps_key_hex: hex::encode([0xBB; 32]),
            stake: Amount(1_000),
            shard_id: Some(ShardId(0)),
        };
        let info = convert_validator(0, &entry).unwrap();
        assert_eq!(info.index, ValidatorIndex(0));
        assert_eq!(info.stake, Amount(1_000));
        assert_eq!(info.shard_id, Some(ShardId(0)));
        assert!(!info.is_slashed);
    }

    #[test]
    fn seed_allocations_empty_list() {
        let genesis = GenesisConfig {
            chain_id: "test".to_owned(),
            genesis_timestamp: nexus_primitives::TimestampMs(0),
            num_shards: 1,
            validators: vec![],
            allocations: vec![],
            consensus: Default::default(),
        };
        let store = MemoryStore::new();
        seed_allocations_atomic(&store, ShardId(0), &genesis).unwrap();
        // Even with empty allocations, the genesis marker should be written.
        assert!(is_genesis_applied(&store).unwrap());
    }

    #[test]
    fn genesis_seeds_staking_records() {
        use crate::snapshot_provider::build_staking_resource_key;
        use crate::validator_identity::address_from_validator_index;

        let genesis = GenesisConfig::for_testing();
        let store = MemoryStore::new();
        seed_allocations_atomic(&store, ShardId(0), &genesis).unwrap();

        // Verify that each genesis validator has a staking record.
        for (i, entry) in genesis.validators.iter().enumerate() {
            let addr = address_from_validator_index(ValidatorIndex(i as u32));
            let key = build_staking_resource_key(&addr);
            let raw = store
                .get_sync(ColumnFamily::State.as_str(), &key)
                .unwrap()
                .expect("staking record should be seeded");

            // BCS: 41 bytes = u64 + u64 + u8 + u64 + u64 + u64
            assert_eq!(raw.len(), 41);
            let bonded = u64::from_le_bytes(raw[0..8].try_into().unwrap());
            assert_eq!(bonded, entry.stake.0);
            let status = raw[16];
            assert_eq!(status, 0, "genesis validator should be Active");
        }
    }
}
