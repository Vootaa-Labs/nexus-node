//! R-5: Staking rotation and recovery end-to-end tests.
//!
//! Validates the full staking -> election -> committee rotation pipeline
//! and verifies cold-restart integrity of the staking/identity layer.

#[cfg(test)]
mod tests {
    use nexus_consensus::types::ReputationScore;
    use nexus_consensus::ValidatorRegistry;
    use nexus_node::epoch_store;
    use nexus_node::genesis_boot;
    use nexus_node::snapshot_provider::build_staking_resource_key;
    use nexus_node::staking_snapshot::{
        attempt_rotation, CommitteeRotationPolicy, RotationOutcome, StakingSnapshot,
        ValidatorStakeRecord,
    };
    use nexus_node::validator_identity::{
        address_from_validator_index, load_identity_registry, persist_identity_registry,
        ValidatorIdentityRegistry,
    };
    use nexus_primitives::{AccountAddress, EpochNumber, ShardId, ValidatorIndex};
    use nexus_storage::traits::StateStorage;
    use nexus_storage::ColumnFamily;
    use nexus_storage::MemoryStore;

    use crate::fixtures::consensus::TestCommittee;

    fn addr(i: u8) -> AccountAddress {
        address_from_validator_index(ValidatorIndex(i as u32))
    }

    fn active_record(i: u8, bonded: u64) -> ValidatorStakeRecord {
        ValidatorStakeRecord {
            address: addr(i),
            bonded,
            penalty_total: 0,
            status: 0,
            registered_epoch: 0,
            unbond_epoch: 0,
            is_slashed: false,
            reputation: ReputationScore::default(),
        }
    }

    fn encode_staking_bcs(bonded: u64, penalty: u64, status: u8) -> Vec<u8> {
        let mut buf = Vec::with_capacity(41);
        buf.extend_from_slice(&bonded.to_le_bytes());
        buf.extend_from_slice(&penalty.to_le_bytes());
        buf.push(status);
        buf.extend_from_slice(&0u64.to_le_bytes());
        buf.extend_from_slice(&0u64.to_le_bytes());
        buf.extend_from_slice(&0u64.to_le_bytes());
        buf
    }

    // R-5a: Genesis boot seeds staking records
    #[test]
    fn genesis_boot_seeds_readable_staking_records() {
        let genesis = nexus_config::genesis::GenesisConfig::for_testing();
        let store = MemoryStore::new();

        let dir = std::env::temp_dir().join("nexus-r5a-genesis-boot");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("genesis.json");
        std::fs::write(&path, serde_json::to_string_pretty(&genesis).unwrap()).unwrap();

        let result = genesis_boot::boot_from_genesis(&path, &store, ShardId(0)).unwrap();
        let n = result.committee.active_validators().len();

        for i in 0..n {
            let a = address_from_validator_index(ValidatorIndex(i as u32));
            let key = build_staking_resource_key(&a);
            let raw = store
                .get_sync(ColumnFamily::State.as_str(), &key)
                .unwrap()
                .unwrap_or_else(|| panic!("staking record missing for validator {i}"));
            assert_eq!(raw.len(), 41);
            let bonded = u64::from_le_bytes(raw[0..8].try_into().unwrap());
            assert!(bonded > 0);
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    // R-5b: Snapshot provider reads seeded staking data
    #[test]
    fn snapshot_provider_reads_seeded_staking_records() {
        use nexus_node::snapshot_provider::build_snapshot_provider;
        use std::sync::{Arc, Mutex};

        let store = MemoryStore::new();
        let tc = TestCommittee::new(4, EpochNumber(0));
        let (engine, _sks, _vks) = tc.into_engine();

        let registry = ValidatorIdentityRegistry::new();
        registry.seed_from_committee(engine.committee());
        let registry = Arc::new(registry);

        for i in 0..4u8 {
            let a = addr(i);
            let key = build_staking_resource_key(&a);
            let bcs_bytes = encode_staking_bcs(1_000_000_000, 0, 0);
            store
                .put_sync(ColumnFamily::State.as_str(), key, bcs_bytes)
                .unwrap();
        }

        let engine_handle = Arc::new(Mutex::new(engine));
        let provider = build_snapshot_provider(engine_handle, store, registry);

        let snapshot = (provider)().expect("snapshot should be built");
        assert_eq!(snapshot.records.len(), 4);
        for record in &snapshot.records {
            assert_eq!(record.bonded, 1_000_000_000);
            assert!(record.is_active());
        }
    }

    // R-5c: Election determinism
    #[test]
    fn election_from_snapshot_is_deterministic() {
        let policy = CommitteeRotationPolicy::with_interval(1);
        let records: Vec<_> = (0..7u8)
            .map(|i| active_record(i, 1_000_000_000 + (i as u64) * 100_000_000))
            .collect();
        let snapshot = StakingSnapshot::new(EpochNumber(1), records);

        let r1 = attempt_rotation(Some(&snapshot), &policy, EpochNumber(1));
        let r2 = attempt_rotation(Some(&snapshot), &policy, EpochNumber(1));

        match (&r1, &r2) {
            (RotationOutcome::Elected(e1), RotationOutcome::Elected(e2)) => {
                assert_eq!(e1.elected.len(), e2.elected.len());
                for (a, b) in e1.elected.iter().zip(e2.elected.iter()) {
                    assert_eq!(a.address, b.address);
                    assert_eq!(a.effective_stake, b.effective_stake);
                }
            }
            _ => panic!("both rotations should produce Elected outcome"),
        }
    }

    // R-5c2: Election interval enforcement
    #[test]
    fn election_respects_interval() {
        let policy = CommitteeRotationPolicy::with_interval(3);
        let records: Vec<_> = (0..4u8).map(|i| active_record(i, 1_000_000_000)).collect();

        let snap1 = StakingSnapshot::new(EpochNumber(1), records.clone());
        assert!(matches!(
            attempt_rotation(Some(&snap1), &policy, EpochNumber(1)),
            RotationOutcome::NotElectionEpoch
        ));

        let snap2 = StakingSnapshot::new(EpochNumber(2), records.clone());
        assert!(matches!(
            attempt_rotation(Some(&snap2), &policy, EpochNumber(2)),
            RotationOutcome::NotElectionEpoch
        ));

        let snap3 = StakingSnapshot::new(EpochNumber(3), records);
        assert!(matches!(
            attempt_rotation(Some(&snap3), &policy, EpochNumber(3)),
            RotationOutcome::Elected(_)
        ));
    }

    // R-5d: Identity registry persistence round-trip
    #[test]
    fn identity_registry_persist_and_reload() {
        let store = MemoryStore::new();
        let tc = TestCommittee::new(7, EpochNumber(0));

        let registry = ValidatorIdentityRegistry::new();
        registry.seed_from_committee(&tc.committee);
        assert_eq!(registry.len(), 7);

        persist_identity_registry(&store, &registry).unwrap();

        let loaded = load_identity_registry(&store).unwrap();
        assert_eq!(loaded.len(), 7);

        for (a, original_key) in registry.all_entries() {
            let loaded_key = loaded.lookup(&a).expect("identity must survive");
            assert_eq!(original_key.as_bytes(), loaded_key.as_bytes());
        }
    }

    // R-5e: Cold-restart staking + identity integrity
    #[test]
    fn cold_restart_preserves_staking_and_identity() {
        let store = MemoryStore::new();

        let genesis = nexus_config::genesis::GenesisConfig::for_testing();
        let dir = std::env::temp_dir().join("nexus-r5e-cold-restart");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("genesis.json");
        std::fs::write(&path, serde_json::to_string_pretty(&genesis).unwrap()).unwrap();
        let boot = genesis_boot::boot_from_genesis(&path, &store, ShardId(0)).unwrap();

        let registry = ValidatorIdentityRegistry::new();
        registry.seed_from_committee(&boot.committee);
        persist_identity_registry(&store, &registry).unwrap();

        let original_entries = registry.all_entries();
        let n = original_entries.len();
        drop(registry);

        let reloaded = load_identity_registry(&store).unwrap();
        assert_eq!(reloaded.len(), n);

        for i in 0..n {
            let a = address_from_validator_index(ValidatorIndex(i as u32));
            let key = build_staking_resource_key(&a);
            let raw = store.get_sync(ColumnFamily::State.as_str(), &key).unwrap();
            assert!(raw.is_some(), "staking record must survive restart");
            assert_eq!(raw.unwrap().len(), 41);
        }

        for (a, original_key) in &original_entries {
            let reloaded_key = reloaded.lookup(a).expect("identity must survive restart");
            assert_eq!(original_key.as_bytes(), reloaded_key.as_bytes());
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    // R-5f: Slashed validators excluded from election
    #[test]
    fn slashed_validator_excluded_from_rotation() {
        let policy = CommitteeRotationPolicy::with_interval(1);
        let mut records: Vec<_> = (0..4u8).map(|i| active_record(i, 1_000_000_000)).collect();
        records[2].is_slashed = true;

        let snapshot = StakingSnapshot::new(EpochNumber(1), records);
        let outcome = attempt_rotation(Some(&snapshot), &policy, EpochNumber(1));

        match outcome {
            RotationOutcome::Elected(result) => {
                for elected in &result.elected {
                    assert_ne!(elected.address, addr(2), "slashed validator excluded");
                }
            }
            RotationOutcome::Fallback { .. } => { /* acceptable */ }
            other => panic!("unexpected: {:?}", other),
        }
    }

    // R-5g: Full pipeline genesis -> rotation -> persisted election
    #[test]
    fn full_pipeline_genesis_to_election_to_persistence() {
        use nexus_node::staking_snapshot::PersistedElectionResult;

        let store = MemoryStore::new();
        let genesis = nexus_config::genesis::GenesisConfig::for_testing();
        let dir = std::env::temp_dir().join("nexus-r5g-full-pipeline");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("genesis.json");
        std::fs::write(&path, serde_json::to_string_pretty(&genesis).unwrap()).unwrap();

        let boot = genesis_boot::boot_from_genesis(&path, &store, ShardId(0)).unwrap();
        let n = boot.committee.active_validators().len();

        let mut records = Vec::with_capacity(n);
        for i in 0..n {
            let a = address_from_validator_index(ValidatorIndex(i as u32));
            let key = build_staking_resource_key(&a);
            let raw = store
                .get_sync(ColumnFamily::State.as_str(), &key)
                .unwrap()
                .unwrap();
            let bonded = u64::from_le_bytes(raw[0..8].try_into().unwrap());
            records.push(ValidatorStakeRecord {
                address: a,
                bonded,
                penalty_total: 0,
                status: 0,
                registered_epoch: 0,
                unbond_epoch: 0,
                is_slashed: false,
                reputation: ReputationScore::default(),
            });
        }
        let snapshot = StakingSnapshot::new(EpochNumber(1), records);

        let policy = CommitteeRotationPolicy::with_interval(1);
        let outcome = attempt_rotation(Some(&snapshot), &policy, EpochNumber(1));

        let election_result = match outcome {
            RotationOutcome::Elected(r) => r,
            other => panic!("expected Elected, got {:?}", other),
        };
        assert!(!election_result.elected.is_empty());

        let persisted = PersistedElectionResult::from(&election_result);
        let persisted_bytes = bcs::to_bytes(&persisted).unwrap();
        let election_key = epoch_store::election_key_for(EpochNumber(1));
        store
            .put_sync(
                ColumnFamily::State.as_str(),
                election_key.clone(),
                persisted_bytes,
            )
            .unwrap();

        let loaded_bytes = store
            .get_sync(ColumnFamily::State.as_str(), &election_key)
            .unwrap()
            .expect("persisted");
        let loaded: PersistedElectionResult = bcs::from_bytes(&loaded_bytes).unwrap();
        assert_eq!(loaded.for_epoch, EpochNumber(1));
        assert_eq!(loaded.elected.len(), election_result.elected.len());

        let _ = std::fs::remove_dir_all(&dir);
    }

    // R-5h: RPC DTO round-trip
    #[test]
    fn election_result_dto_roundtrip() {
        use nexus_rpc::dto::{ElectedValidatorDto, ElectionResultDto};

        let policy = CommitteeRotationPolicy::with_interval(1);
        let records: Vec<_> = (0..4u8)
            .map(|i| active_record(i, 1_000_000_000 + (i as u64) * 100_000_000))
            .collect();
        let snapshot = StakingSnapshot::new(EpochNumber(5), records);
        let result = match attempt_rotation(Some(&snapshot), &policy, EpochNumber(5)) {
            RotationOutcome::Elected(r) => r,
            other => panic!("expected Elected, got {:?}", other),
        };

        let dto = ElectionResultDto {
            for_epoch: EpochNumber(5),
            snapshot_epoch: result.snapshot_epoch,
            elected: result
                .elected
                .iter()
                .map(|e| ElectedValidatorDto {
                    address_hex: hex::encode(e.address.0),
                    effective_stake: e.effective_stake,
                    committee_index: e.committee_index,
                })
                .collect(),
            total_effective_stake: result.total_effective_stake,
            is_fallback: false,
        };

        let json = serde_json::to_string(&dto).unwrap();
        let parsed: ElectionResultDto = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.for_epoch, EpochNumber(5));
        assert_eq!(parsed.elected.len(), result.elected.len());
        assert!(parsed.total_effective_stake > 0);
    }

    // R-5i: Address derivation consistency
    #[test]
    fn address_derivation_consistent_across_subsystems() {
        for i in 0..10u32 {
            let a = address_from_validator_index(ValidatorIndex(i));
            let idx_bytes = &a.0[28..32];
            assert_eq!(u32::from_be_bytes(idx_bytes.try_into().unwrap()), i);
            assert_eq!(&a.0[0..28], &[0u8; 28]);

            let key1 = build_staking_resource_key(&a);
            let key2 = build_staking_resource_key(&a);
            assert_eq!(key1, key2);
        }
    }

    // R-5j: Multi-epoch rotation pipeline
    #[test]
    fn multi_epoch_rotation_pipeline() {
        let policy = CommitteeRotationPolicy::with_interval(2);
        let records: Vec<_> = (0..7u8)
            .map(|i| active_record(i, 1_000_000_000 + (i as u64) * 50_000_000))
            .collect();

        let mut election_count = 0u32;
        for epoch in 1..=10u64 {
            let snapshot = StakingSnapshot::new(EpochNumber(epoch), records.clone());
            match attempt_rotation(Some(&snapshot), &policy, EpochNumber(epoch)) {
                RotationOutcome::Elected(result) => {
                    election_count += 1;
                    assert!(!result.elected.is_empty());
                    for e in &result.elected {
                        assert!(e.effective_stake > 0);
                    }
                }
                RotationOutcome::NotElectionEpoch => {}
                other => panic!("unexpected at epoch {epoch}: {:?}", other),
            }
        }

        assert_eq!(election_count, 5);
    }
}
