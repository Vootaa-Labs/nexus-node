// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! S-5: Staking release regression tests.
//!
//! End-to-end regression suite verifying the complete lifecycle:
//!   1. Genesis → staking → first election
//!   2. Stake / unbond / withdraw → committee composition change
//!   3. Multi-node determinism across multiple epochs
//!   4. Cold restart → committee & epoch history preserved
//!   5. RPC DTO consistency with internal state

#[cfg(test)]
mod tests {
    use nexus_consensus::types::{EpochTransition, EpochTransitionTrigger, ReputationScore};
    use nexus_consensus::ValidatorRegistry;
    use nexus_node::epoch_store;
    use nexus_node::genesis_boot;
    use nexus_node::snapshot_provider::build_staking_resource_key;
    use nexus_node::staking_snapshot::{
        attempt_rotation, CommitteeRotationPolicy, ElectionResult, PersistedElectionResult,
        RotationOutcome, StakingSnapshot, ValidatorStakeRecord,
    };
    use nexus_node::validator_identity::{
        address_from_validator_index, load_identity_registry, persist_identity_registry,
        ValidatorIdentityRegistry,
    };
    use nexus_primitives::{AccountAddress, EpochNumber, ShardId, TimestampMs, ValidatorIndex};
    use nexus_rpc::dto::{
        ElectedValidatorDto, ElectionResultDto, RotationPolicyDto, StakingValidatorDto,
        StakingValidatorsResponse,
    };
    use nexus_storage::traits::StateStorage;
    use nexus_storage::ColumnFamily;
    use nexus_storage::MemoryStore;

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
        buf.extend_from_slice(&0u64.to_le_bytes()); // registered_epoch
        buf.extend_from_slice(&0u64.to_le_bytes()); // unbond_epoch
        buf.extend_from_slice(&0u64.to_le_bytes()); // metadata_tag
        buf
    }

    fn make_transition(from: u64, to: u64) -> EpochTransition {
        EpochTransition {
            from_epoch: EpochNumber(from),
            to_epoch: EpochNumber(to),
            trigger: EpochTransitionTrigger::Manual,
            final_commit_count: from * 10,
            transitioned_at: TimestampMs::now(),
        }
    }

    // ── S-5a: Genesis → first election lifecycle ──

    #[test]
    fn genesis_to_first_election_lifecycle() {
        // 1. Boot from genesis
        let genesis = nexus_config::genesis::GenesisConfig::for_testing();
        let store = MemoryStore::new();

        let dir = std::env::temp_dir().join("nexus-s5a-genesis");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("genesis.json");
        std::fs::write(&path, serde_json::to_string_pretty(&genesis).unwrap()).unwrap();

        let boot = genesis_boot::boot_from_genesis(&path, &store, ShardId(0)).unwrap();
        let n = boot.committee.active_validators().len();
        assert!(n >= 4, "genesis must have ≥ 4 validators");

        // 2. Build staking snapshot from genesis state
        let mut records = Vec::new();
        for i in 0..n {
            let a = address_from_validator_index(ValidatorIndex(i as u32));
            let key = build_staking_resource_key(&a);
            let raw = store
                .get_sync(ColumnFamily::State.as_str(), &key)
                .unwrap()
                .unwrap();
            let bonded = u64::from_le_bytes(raw[0..8].try_into().unwrap());
            let penalty = u64::from_le_bytes(raw[8..16].try_into().unwrap());
            let status = raw[16];
            records.push(ValidatorStakeRecord {
                address: a,
                bonded,
                penalty_total: penalty,
                status,
                registered_epoch: 0,
                unbond_epoch: 0,
                is_slashed: false,
                reputation: ReputationScore::default(),
            });
        }

        let snapshot = StakingSnapshot::new(EpochNumber(0), records);
        let policy = CommitteeRotationPolicy::with_interval(1);

        // 3. Run first election at epoch 1
        let outcome = attempt_rotation(Some(&snapshot), &policy, EpochNumber(1));
        match outcome {
            RotationOutcome::Elected(result) => {
                assert_eq!(result.for_epoch, EpochNumber(1));
                assert_eq!(result.elected.len(), n);
                assert!(result.total_effective_stake > 0);
            }
            other => panic!("expected Elected, got: {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── S-5b: Stake change → committee composition change ──

    #[test]
    fn stake_change_alters_committee_composition() {
        let policy = CommitteeRotationPolicy::with_interval(1);

        // Epoch 1: 5 validators, validator 0 has highest stake
        let mut records: Vec<_> = (0..5u8).map(|i| active_record(i, 2_000_000_000)).collect();
        records[0].bonded = 10_000_000_000;

        let snap1 = StakingSnapshot::new(EpochNumber(0), records.clone());
        let r1 = match attempt_rotation(Some(&snap1), &policy, EpochNumber(1)) {
            RotationOutcome::Elected(r) => r,
            other => panic!("expected Elected, got: {other:?}"),
        };
        assert_eq!(
            r1.elected[0].address,
            addr(0),
            "validator 0 should be first"
        );

        // Epoch 2: validator 0 unbonds, validator 4 bonds more
        records[0].status = 1; // unbonding
        records[4].bonded = 15_000_000_000;

        let snap2 = StakingSnapshot::new(EpochNumber(1), records);
        let r2 = match attempt_rotation(Some(&snap2), &policy, EpochNumber(2)) {
            RotationOutcome::Elected(r) => r,
            other => panic!("expected Elected, got: {other:?}"),
        };

        // Validator 0 should be gone (unbonding), validator 4 should be first
        let addrs: Vec<_> = r2.elected.iter().map(|e| e.address).collect();
        assert!(!addrs.contains(&addr(0)), "unbonding validator 0 excluded");
        assert_eq!(
            r2.elected[0].address,
            addr(4),
            "validator 4 should be first"
        );
    }

    // ── S-5c: Multi-epoch determinism with evolving stake ──

    #[test]
    fn multi_epoch_evolving_stake_determinism() {
        let policy = CommitteeRotationPolicy::with_interval(1);

        let mut records_a: Vec<_> = (0..6u8)
            .map(|i| active_record(i, 2_000_000_000 + (i as u64) * 500_000_000))
            .collect();
        let mut records_b = records_a.clone();

        let mut results_a = Vec::new();
        let mut results_b = Vec::new();

        for epoch in 1..=10u64 {
            // Both "nodes" see the same evolving state
            let snap_a = StakingSnapshot::new(EpochNumber(epoch - 1), records_a.clone());
            let snap_b = StakingSnapshot::new(EpochNumber(epoch - 1), records_b.clone());

            let ra = match attempt_rotation(Some(&snap_a), &policy, EpochNumber(epoch)) {
                RotationOutcome::Elected(r) => r,
                other => panic!("node_a epoch {epoch}: expected Elected, got: {other:?}"),
            };
            let rb = match attempt_rotation(Some(&snap_b), &policy, EpochNumber(epoch)) {
                RotationOutcome::Elected(r) => r,
                other => panic!("node_b epoch {epoch}: expected Elected, got: {other:?}"),
            };

            results_a.push(
                ra.elected
                    .iter()
                    .map(|e| (e.address, e.effective_stake))
                    .collect::<Vec<_>>(),
            );
            results_b.push(
                rb.elected
                    .iter()
                    .map(|e| (e.address, e.effective_stake))
                    .collect::<Vec<_>>(),
            );

            // Evolve stake symmetrically for both nodes
            if epoch == 3 {
                records_a[1].penalty_total = 500_000_000;
                records_b[1].penalty_total = 500_000_000;
            }
            if epoch == 6 {
                records_a[4].bonded += 3_000_000_000;
                records_b[4].bonded += 3_000_000_000;
            }
        }

        for (i, (a, b)) in results_a.iter().zip(results_b.iter()).enumerate() {
            assert_eq!(a, b, "epoch {} results differ between nodes", i + 1);
        }
    }

    // ── S-5d: Cold restart preserves committee and election result ──

    #[test]
    fn cold_restart_preserves_committee_and_election() {
        use crate::fixtures::consensus::TestCommittee;

        let store = MemoryStore::new();
        let tc = TestCommittee::new(5, EpochNumber(0));
        let (engine, _sks, _vks) = tc.into_engine();
        let committee = engine.committee().clone();

        // Persist initial epoch
        epoch_store::persist_initial_epoch(&store, &committee).unwrap();

        // Simulate election and persist transition
        let records: Vec<_> = (0..5u8).map(|i| active_record(i, 3_000_000_000)).collect();
        let snapshot = StakingSnapshot::new(EpochNumber(0), records);
        let policy = CommitteeRotationPolicy::with_interval(1);

        let election = match attempt_rotation(Some(&snapshot), &policy, EpochNumber(1)) {
            RotationOutcome::Elected(r) => r,
            other => panic!("expected Elected, got: {other:?}"),
        };
        let persisted_election = PersistedElectionResult::from(&election);

        // Build new committee for epoch 1 (reuse validators for simplicity)
        let tc2 = TestCommittee::new(5, EpochNumber(1));
        let new_committee = tc2.committee;

        let transition = make_transition(0, 1);
        epoch_store::persist_epoch_transition_with_election(
            &store,
            &new_committee,
            &transition,
            Some(&persisted_election),
        )
        .unwrap();

        // Simulate cold restart: load everything back
        let loaded = epoch_store::load_epoch_state(&store).unwrap().unwrap();

        assert_eq!(loaded.epoch, EpochNumber(1));
        assert_eq!(loaded.committee.epoch(), EpochNumber(1));
        assert_eq!(
            loaded.committee.active_validators().len(),
            new_committee.active_validators().len()
        );

        let er = loaded
            .election_result
            .expect("election result should be persisted");
        assert_eq!(er.for_epoch, EpochNumber(1));
        assert_eq!(er.elected.len(), election.elected.len());
        assert!(!er.is_fallback);
    }

    // ── S-5e: Identity registry survives cold restart ──

    #[test]
    fn identity_registry_cold_restart_regression() {
        let store = MemoryStore::new();
        let tc = crate::fixtures::consensus::TestCommittee::new(6, EpochNumber(0));

        let registry = ValidatorIdentityRegistry::new();
        registry.seed_from_committee(&tc.committee);
        assert_eq!(registry.len(), 6);

        // Persist
        persist_identity_registry(&store, &registry).unwrap();

        // Simulate restart: load into new registry
        let loaded = load_identity_registry(&store).unwrap();
        assert_eq!(loaded.len(), 6);

        for (a, original_key) in registry.all_entries() {
            let restored = loaded.lookup(&a).expect("restored should exist");
            assert_eq!(original_key.as_bytes(), restored.as_bytes());
        }
    }

    // ── S-5f: Election result DTO faithfully represents internal state ──

    #[test]
    fn election_dto_matches_internal_state() {
        let records: Vec<_> = (0..5u8)
            .map(|i| active_record(i, 2_000_000_000 + (i as u64) * 100_000_000))
            .collect();
        let snapshot = StakingSnapshot::new(EpochNumber(3), records);
        let policy = CommitteeRotationPolicy::with_interval(1);

        let election = match attempt_rotation(Some(&snapshot), &policy, EpochNumber(4)) {
            RotationOutcome::Elected(r) => r,
            other => panic!("expected Elected, got: {other:?}"),
        };

        // Build DTO manually (simulating what the REST handler does)
        let dto = ElectionResultDto {
            for_epoch: election.for_epoch,
            snapshot_epoch: election.snapshot_epoch,
            elected: election
                .elected
                .iter()
                .map(|e| ElectedValidatorDto {
                    address_hex: hex::encode(e.address.0),
                    effective_stake: e.effective_stake,
                    committee_index: e.committee_index,
                })
                .collect(),
            total_effective_stake: election.total_effective_stake,
            is_fallback: false,
        };

        // Verify DTO fidelity
        assert_eq!(dto.for_epoch, EpochNumber(4));
        assert_eq!(dto.snapshot_epoch, EpochNumber(3));
        assert_eq!(dto.elected.len(), election.elected.len());
        assert!(!dto.is_fallback);

        // Verify JSON round-trip
        let json = serde_json::to_string(&dto).unwrap();
        let decoded: ElectionResultDto = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.for_epoch, dto.for_epoch);
        assert_eq!(decoded.elected.len(), dto.elected.len());
        assert_eq!(decoded.total_effective_stake, dto.total_effective_stake);
    }

    // ── S-5g: Rotation policy DTO round-trip ──

    #[test]
    fn rotation_policy_dto_roundtrip() {
        let dto = RotationPolicyDto {
            election_epoch_interval: 3,
            max_committee_size: 100,
            min_committee_size: 4,
            min_total_effective_stake: 4_000_000_000,
            exclude_slashed: true,
            min_reputation_score: 0,
        };

        let json = serde_json::to_string(&dto).unwrap();
        let decoded: RotationPolicyDto = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded.election_epoch_interval, 3);
        assert_eq!(decoded.max_committee_size, 100);
        assert_eq!(decoded.min_committee_size, 4);
        assert_eq!(decoded.min_total_effective_stake, 4_000_000_000);
        assert!(decoded.exclude_slashed);
    }

    // ── S-5h: Staking validators DTO round-trip ──

    #[test]
    fn staking_validators_dto_roundtrip() {
        let dto = StakingValidatorsResponse {
            snapshot_epoch: EpochNumber(5),
            validators: vec![
                StakingValidatorDto {
                    address_hex: hex::encode([0u8; 32]),
                    bonded: 5_000_000_000,
                    penalty_total: 500_000_000,
                    effective_stake: 4_500_000_000,
                    status: 0,
                    is_slashed: false,
                    reputation: 10000,
                },
                StakingValidatorDto {
                    address_hex: hex::encode([1u8; 32]),
                    bonded: 2_000_000_000,
                    penalty_total: 0,
                    effective_stake: 2_000_000_000,
                    status: 1,
                    is_slashed: false,
                    reputation: 8000,
                },
            ],
            active_count: 1,
            total_effective_stake: 4_500_000_000,
        };

        let json = serde_json::to_string(&dto).unwrap();
        let decoded: StakingValidatorsResponse = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded.snapshot_epoch, EpochNumber(5));
        assert_eq!(decoded.validators.len(), 2);
        assert_eq!(decoded.active_count, 1);
        assert_eq!(decoded.total_effective_stake, 4_500_000_000);
    }

    // ── S-5i: Full pipeline regression: genesis → 5 epochs → persist → reload ──

    #[test]
    fn full_pipeline_multi_epoch_persist_reload() {
        use crate::fixtures::consensus::TestCommittee;

        let store = MemoryStore::new();
        let policy = CommitteeRotationPolicy::with_interval(2);

        // Epoch 0: genesis committee
        let tc = TestCommittee::new(5, EpochNumber(0));
        let (engine, _sks, _vks) = tc.into_engine();
        epoch_store::persist_initial_epoch(&store, engine.committee()).unwrap();

        let base_records: Vec<_> = (0..5u8)
            .map(|i| active_record(i, 3_000_000_000 + (i as u64) * 200_000_000))
            .collect();

        let mut last_election: Option<ElectionResult> = None;

        for epoch in 1..=5u64 {
            let snap = StakingSnapshot::new(EpochNumber(epoch - 1), base_records.clone());
            let outcome = attempt_rotation(Some(&snap), &policy, EpochNumber(epoch));

            // Build new committee (reuse test committee for each epoch)
            let tc_new = TestCommittee::new(5, EpochNumber(epoch));
            let transition = make_transition(epoch - 1, epoch);

            match outcome {
                RotationOutcome::Elected(ref election) => {
                    let per = PersistedElectionResult::from(election);
                    epoch_store::persist_epoch_transition_with_election(
                        &store,
                        &tc_new.committee,
                        &transition,
                        Some(&per),
                    )
                    .unwrap();
                    last_election = Some(election.clone());
                }
                RotationOutcome::NotElectionEpoch => {
                    epoch_store::persist_epoch_transition(&store, &tc_new.committee, &transition)
                        .unwrap();
                }
                RotationOutcome::Fallback { reason } => {
                    panic!("unexpected fallback at epoch {epoch}: {reason}");
                }
            }
        }

        // Verify cold restart loads final state
        let loaded = epoch_store::load_epoch_state(&store).unwrap().unwrap();
        assert_eq!(loaded.epoch, EpochNumber(5));

        // With interval=2, elections at epochs 2, 4 (not 1, 3, 5)
        // Last election should be at epoch 4
        assert!(last_election.is_some());
        let last = last_election.unwrap();
        assert_eq!(last.for_epoch, EpochNumber(4));
    }

    // ── S-5j: Staking BCS encoding consistency ──

    #[test]
    fn staking_bcs_encoding_roundtrip_consistency() {
        let store = MemoryStore::new();

        let test_cases = [
            (1_000_000_000u64, 0u64, 0u8),   // active, no penalty
            (5_000_000_000, 500_000_000, 0), // active, with penalty
            (2_000_000_000, 0, 1),           // unbonding
            (0, 0, 2),                       // withdrawn
            (u64::MAX, u64::MAX, 0),         // max values
        ];

        for (i, (bonded, penalty, status)) in test_cases.iter().enumerate() {
            let a = addr(i as u8);
            let key = build_staking_resource_key(&a);
            let bcs_bytes = encode_staking_bcs(*bonded, *penalty, *status);
            assert_eq!(bcs_bytes.len(), 41);

            store
                .put_sync(ColumnFamily::State.as_str(), key.clone(), bcs_bytes)
                .unwrap();

            // Read back
            let raw = store
                .get_sync(ColumnFamily::State.as_str(), &key)
                .unwrap()
                .unwrap();

            let decoded_bonded = u64::from_le_bytes(raw[0..8].try_into().unwrap());
            let decoded_penalty = u64::from_le_bytes(raw[8..16].try_into().unwrap());
            let decoded_status = raw[16];

            assert_eq!(decoded_bonded, *bonded, "case {i}: bonded mismatch");
            assert_eq!(decoded_penalty, *penalty, "case {i}: penalty mismatch");
            assert_eq!(decoded_status, *status, "case {i}: status mismatch");
        }
    }
}
