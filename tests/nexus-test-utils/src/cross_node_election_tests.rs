//! S-3: Cross-node election determinism tests.
//!
//! Verifies that independent "simulated nodes" running the same election
//! logic on the same staking snapshot produce identical committee results,
//! regardless of construction order, thread of execution, or intermediate
//! state differences.

#[cfg(test)]
mod tests {
    use nexus_consensus::types::ReputationScore;
    use nexus_node::staking_snapshot::{
        attempt_rotation, elect_committee, elect_committee_with_policy, CommitteeRotationPolicy,
        ElectionPolicy, RotationOutcome, StakingSnapshot, ValidatorStakeRecord,
    };
    use nexus_node::validator_identity::address_from_validator_index;
    use nexus_primitives::{AccountAddress, EpochNumber, ValidatorIndex};
    use std::collections::HashSet;

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

    // ── S-3a: N independent elections on the same snapshot yield identical results ──

    #[test]
    fn independent_elections_produce_identical_results() {
        let policy = ElectionPolicy::default();
        let records: Vec<_> = (0..7u8)
            .map(|i| active_record(i, 2_000_000_000 + (i as u64) * 500_000_000))
            .collect();
        let snapshot = StakingSnapshot::new(EpochNumber(5), records);

        // Simulate 10 independent "nodes" running the same election
        let results: Vec<_> = (0..10)
            .map(|_| elect_committee(&snapshot, &policy, EpochNumber(5)).unwrap())
            .collect();

        let reference = &results[0];
        for (i, result) in results.iter().enumerate().skip(1) {
            assert_eq!(
                reference.elected.len(),
                result.elected.len(),
                "node {i} elected count differs"
            );
            for (a, b) in reference.elected.iter().zip(result.elected.iter()) {
                assert_eq!(
                    a.address, b.address,
                    "node {i} address mismatch at idx {}",
                    a.committee_index
                );
                assert_eq!(
                    a.effective_stake, b.effective_stake,
                    "node {i} stake mismatch"
                );
                assert_eq!(
                    a.committee_index, b.committee_index,
                    "node {i} index mismatch"
                );
            }
            assert_eq!(
                reference.total_effective_stake,
                result.total_effective_stake
            );
        }
    }

    // ── S-3b: Election order is invariant to record insertion order ──

    #[test]
    fn election_invariant_to_record_insertion_order() {
        let policy = ElectionPolicy::default();

        // Forward order
        let records_fwd: Vec<_> = (0..7u8)
            .map(|i| active_record(i, 1_000_000_000 + (i as u64) * 200_000_000))
            .collect();

        // Reverse order
        let records_rev: Vec<_> = (0..7u8)
            .rev()
            .map(|i| active_record(i, 1_000_000_000 + (i as u64) * 200_000_000))
            .collect();

        // Shuffled order (interleaved odd/even)
        let mut records_shuffle = Vec::new();
        for i in (1..7u8).step_by(2) {
            records_shuffle.push(active_record(i, 1_000_000_000 + (i as u64) * 200_000_000));
        }
        for i in (0..7u8).step_by(2) {
            records_shuffle.push(active_record(i, 1_000_000_000 + (i as u64) * 200_000_000));
        }

        let snap_fwd = StakingSnapshot::new(EpochNumber(3), records_fwd);
        let snap_rev = StakingSnapshot::new(EpochNumber(3), records_rev);
        let snap_shuf = StakingSnapshot::new(EpochNumber(3), records_shuffle);

        let r_fwd = elect_committee(&snap_fwd, &policy, EpochNumber(3)).unwrap();
        let r_rev = elect_committee(&snap_rev, &policy, EpochNumber(3)).unwrap();
        let r_shuf = elect_committee(&snap_shuf, &policy, EpochNumber(3)).unwrap();

        // All three must produce identical elected sets
        assert_eq!(r_fwd.elected.len(), r_rev.elected.len());
        assert_eq!(r_fwd.elected.len(), r_shuf.elected.len());

        for i in 0..r_fwd.elected.len() {
            assert_eq!(r_fwd.elected[i].address, r_rev.elected[i].address);
            assert_eq!(r_fwd.elected[i].address, r_shuf.elected[i].address);
            assert_eq!(
                r_fwd.elected[i].effective_stake,
                r_rev.elected[i].effective_stake
            );
        }
    }

    // ── S-3c: Tie-breaking is deterministic (same stake → sort by address) ──

    #[test]
    fn tie_breaking_deterministic_by_address() {
        let policy = ElectionPolicy::default();
        let equal_stake = 5_000_000_000u64;

        let records: Vec<_> = (0..7u8).map(|i| active_record(i, equal_stake)).collect();
        let snapshot = StakingSnapshot::new(EpochNumber(2), records);

        let r1 = elect_committee(&snapshot, &policy, EpochNumber(2)).unwrap();
        let r2 = elect_committee(&snapshot, &policy, EpochNumber(2)).unwrap();

        // With equal stakes, order should be by address bytes (ascending)
        for (a, b) in r1.elected.iter().zip(r2.elected.iter()) {
            assert_eq!(a.address, b.address);
            assert_eq!(a.committee_index, b.committee_index);
        }

        // Verify addresses are indeed in ascending order (since stakes are equal)
        for w in r1.elected.windows(2) {
            assert!(
                w[0].address.0 < w[1].address.0,
                "addresses should be in ascending order for equal stakes"
            );
        }
    }

    // ── S-3d: Multi-epoch sequence from same initial state is consistent ──

    #[test]
    fn multi_epoch_sequence_deterministic() {
        let policy = CommitteeRotationPolicy::with_interval(1);
        let base_records: Vec<_> = (0..5u8)
            .map(|i| active_record(i, 2_000_000_000 + (i as u64) * 100_000_000))
            .collect();

        // Two independent "nodes" process 20 epochs
        let mut elections_a = Vec::new();
        let mut elections_b = Vec::new();

        for epoch in 1..=20u64 {
            let snap = StakingSnapshot::new(EpochNumber(epoch - 1), base_records.clone());
            let outcome_a = attempt_rotation(Some(&snap), &policy, EpochNumber(epoch));
            let outcome_b = attempt_rotation(Some(&snap), &policy, EpochNumber(epoch));

            match (&outcome_a, &outcome_b) {
                (RotationOutcome::Elected(a), RotationOutcome::Elected(b)) => {
                    elections_a.push(a.elected.iter().map(|e| e.address).collect::<Vec<_>>());
                    elections_b.push(b.elected.iter().map(|e| e.address).collect::<Vec<_>>());
                }
                _ => panic!("epoch {epoch}: both should elect"),
            }
        }

        assert_eq!(elections_a.len(), elections_b.len());
        for (i, (a, b)) in elections_a.iter().zip(elections_b.iter()).enumerate() {
            assert_eq!(a, b, "epoch {} elected set differs between nodes", i + 1);
        }
    }

    // ── S-3e: Policy-filtered election is deterministic ──

    #[test]
    fn policy_filtered_election_deterministic() {
        let mut policy = CommitteeRotationPolicy::with_interval(1);
        policy.exclude_slashed = true;

        let mut records: Vec<_> = (0..7u8).map(|i| active_record(i, 2_000_000_000)).collect();
        // Slash validators 2 and 5
        records[2].is_slashed = true;
        records[5].is_slashed = true;

        let snapshot = StakingSnapshot::new(EpochNumber(1), records.clone());

        let r1 = elect_committee_with_policy(&snapshot, &policy, EpochNumber(1)).unwrap();
        let r2 = elect_committee_with_policy(&snapshot, &policy, EpochNumber(1)).unwrap();

        assert_eq!(r1.elected.len(), 5); // 7 - 2 slashed
        assert_eq!(r1.elected.len(), r2.elected.len());

        let addrs_1: HashSet<_> = r1.elected.iter().map(|e| e.address).collect();
        let addrs_2: HashSet<_> = r2.elected.iter().map(|e| e.address).collect();
        assert_eq!(addrs_1, addrs_2);

        // Slashed addresses must not appear
        assert!(!addrs_1.contains(&addr(2)));
        assert!(!addrs_1.contains(&addr(5)));
    }

    // ── S-3f: Heterogeneous stake produces consistent ranking ──

    #[test]
    fn heterogeneous_stake_consistent_ranking() {
        let policy = ElectionPolicy::default();

        // Deliberately uneven stakes
        let stakes = [
            10_000_000_000u64, // validator 0: 10 NXS
            1_000_000_000,     // validator 1: 1 NXS
            5_000_000_000,     // validator 2: 5 NXS
            3_000_000_000,     // validator 3: 3 NXS
            7_000_000_000,     // validator 4: 7 NXS
            2_000_000_000,     // validator 5: 2 NXS
            8_000_000_000,     // validator 6: 8 NXS
        ];

        let records: Vec<_> = stakes
            .iter()
            .enumerate()
            .map(|(i, &s)| active_record(i as u8, s))
            .collect();

        let snapshot = StakingSnapshot::new(EpochNumber(1), records);
        let result = elect_committee(&snapshot, &policy, EpochNumber(1)).unwrap();

        // Expected order: by effective stake DESC, then address ASC
        // 10B(0), 8B(6), 7B(4), 5B(2), 3B(3), 2B(5), 1B(1)
        let expected_order = [0u8, 6, 4, 2, 3, 5, 1];
        for (i, expected_idx) in expected_order.iter().enumerate() {
            assert_eq!(
                result.elected[i].address,
                addr(*expected_idx),
                "position {i}: expected validator {expected_idx}"
            );
        }

        // Re-run to confirm consistency
        let result2 = elect_committee(&snapshot, &policy, EpochNumber(1)).unwrap();
        for (a, b) in result.elected.iter().zip(result2.elected.iter()) {
            assert_eq!(a.address, b.address);
        }
    }

    // ── S-3g: max_committee_size cap is deterministic ──

    #[test]
    fn max_committee_size_cap_deterministic() {
        let policy = ElectionPolicy {
            max_committee_size: 4,
            ..Default::default()
        };

        let records: Vec<_> = (0..10u8)
            .map(|i| active_record(i, 2_000_000_000 + (i as u64) * 100_000_000))
            .collect();
        let snapshot = StakingSnapshot::new(EpochNumber(1), records);

        let r1 = elect_committee(&snapshot, &policy, EpochNumber(1)).unwrap();
        let r2 = elect_committee(&snapshot, &policy, EpochNumber(1)).unwrap();

        assert_eq!(r1.elected.len(), 4);
        assert_eq!(r2.elected.len(), 4);

        // Must select the top-4 by stake DESC
        for (a, b) in r1.elected.iter().zip(r2.elected.iter()) {
            assert_eq!(a.address, b.address);
            assert_eq!(a.effective_stake, b.effective_stake);
        }

        // Top-4 by stake should be validators 9, 8, 7, 6 (highest bonded)
        let top4_addrs: Vec<_> = r1.elected.iter().map(|e| e.address).collect();
        assert_eq!(top4_addrs[0], addr(9));
        assert_eq!(top4_addrs[1], addr(8));
        assert_eq!(top4_addrs[2], addr(7));
        assert_eq!(top4_addrs[3], addr(6));
    }

    // ── S-3h: Penalty-reduced effective stake ordering is deterministic ──

    #[test]
    fn penalty_reduces_effective_stake_deterministic() {
        let policy = ElectionPolicy::default();

        let mut records: Vec<_> = (0..5u8).map(|i| active_record(i, 5_000_000_000)).collect();
        // Validator 2 has penalty, reducing effective stake
        records[2].penalty_total = 3_000_000_000;

        let snapshot = StakingSnapshot::new(EpochNumber(1), records);

        let r1 = elect_committee(&snapshot, &policy, EpochNumber(1)).unwrap();
        let r2 = elect_committee(&snapshot, &policy, EpochNumber(1)).unwrap();

        // Validator 2 has effective_stake = 5B - 3B = 2B, should be last
        let last = r1.elected.last().unwrap();
        assert_eq!(last.address, addr(2));
        assert_eq!(last.effective_stake, 2_000_000_000);

        // Consistency
        for (a, b) in r1.elected.iter().zip(r2.elected.iter()) {
            assert_eq!(a.address, b.address);
            assert_eq!(a.effective_stake, b.effective_stake);
        }
    }
}
