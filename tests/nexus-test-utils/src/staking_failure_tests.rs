// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! S-4: Staking failure and fallback tests.
//!
//! Validates every error path in the election pipeline:
//!   - Insufficient candidates (below MIN_COMMITTEE_SIZE)
//!   - Insufficient total stake (below MIN_TOTAL_EFFECTIVE_STAKE)
//!   - Snapshot absent (contract not deployed / state unreadable)
//!   - Slashed validators reducing eligible set below threshold
//!   - Penalty accumulation causing effective stake to drop below min
//!   - Non-active validators (unbonding/withdrawn) excluded
//!   - Election interval configuration vs. actual behavior consistency

#[cfg(test)]
mod tests {
    use nexus_consensus::types::ReputationScore;
    use nexus_node::staking_snapshot::{
        attempt_rotation, elect_committee, CommitteeRotationPolicy, ElectionPolicy,
        RotationOutcome, StakingSnapshot, StakingSnapshotError, ValidatorStakeRecord,
        MIN_COMMITTEE_SIZE, MIN_TOTAL_EFFECTIVE_STAKE,
    };
    use nexus_node::validator_identity::address_from_validator_index;
    use nexus_primitives::{AccountAddress, EpochNumber, ValidatorIndex};

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

    // ── S-4a: Zero candidates → Fallback with InsufficientValidators ──

    #[test]
    fn empty_snapshot_produces_fallback() {
        let policy = CommitteeRotationPolicy::with_interval(1);
        let snapshot = StakingSnapshot::new(EpochNumber(1), vec![]);

        let outcome = attempt_rotation(Some(&snapshot), &policy, EpochNumber(1));
        match outcome {
            RotationOutcome::Fallback { reason } => match reason {
                StakingSnapshotError::InsufficientValidators { found, required } => {
                    assert_eq!(found, 0);
                    assert_eq!(required, MIN_COMMITTEE_SIZE);
                }
                other => panic!("expected InsufficientValidators, got: {other}"),
            },
            other => panic!("expected Fallback, got: {other:?}"),
        }
    }

    // ── S-4b: Too few candidates (3 < MIN_COMMITTEE_SIZE=4) ──

    #[test]
    fn below_min_committee_size_produces_fallback() {
        let policy = CommitteeRotationPolicy::with_interval(1);
        let records: Vec<_> = (0..3u8).map(|i| active_record(i, 2_000_000_000)).collect();
        let snapshot = StakingSnapshot::new(EpochNumber(1), records);

        let outcome = attempt_rotation(Some(&snapshot), &policy, EpochNumber(1));
        match outcome {
            RotationOutcome::Fallback { reason } => match reason {
                StakingSnapshotError::InsufficientValidators { found, required } => {
                    assert_eq!(found, 3);
                    assert_eq!(required, MIN_COMMITTEE_SIZE);
                }
                other => panic!("expected InsufficientValidators, got: {other}"),
            },
            other => panic!("expected Fallback, got: {other:?}"),
        }
    }

    // ── S-4c: Total stake below threshold ──

    #[test]
    fn below_min_total_stake_produces_fallback() {
        let policy = CommitteeRotationPolicy::with_interval(1);
        // 4 validators with 500M voo each = 2B total < 4B threshold
        let records: Vec<_> = (0..4u8).map(|i| active_record(i, 500_000_000)).collect();
        let snapshot = StakingSnapshot::new(EpochNumber(1), records);

        let outcome = attempt_rotation(Some(&snapshot), &policy, EpochNumber(1));
        match outcome {
            RotationOutcome::Fallback { reason } => match reason {
                StakingSnapshotError::InsufficientTotalStake { total, required } => {
                    // 4 × 500M = 2B, but they're filtered out because
                    // each < MIN_ELIGIBLE_STAKE (1B), so found=0
                    // OR if they pass eligibility, total=2B < required=4B
                    assert!(
                        total < required,
                        "total {total} should be less than required {required}"
                    );
                }
                StakingSnapshotError::InsufficientValidators { .. } => {
                    // Also acceptable: below-threshold validators filtered out
                }
                other => panic!("expected stake/validator error, got: {other}"),
            },
            other => panic!("expected Fallback, got: {other:?}"),
        }
    }

    // ── S-4d: No snapshot (contract not deployed) → Fallback ──

    #[test]
    fn missing_snapshot_produces_fallback() {
        let policy = CommitteeRotationPolicy::with_interval(1);

        let outcome = attempt_rotation(None, &policy, EpochNumber(1));
        match outcome {
            RotationOutcome::Fallback { reason } => match reason {
                StakingSnapshotError::InsufficientValidators { found: 0, .. } => {}
                other => panic!("expected InsufficientValidators(found=0), got: {other}"),
            },
            other => panic!("expected Fallback, got: {other:?}"),
        }
    }

    // ── S-4e: Slash causes committee to drop below threshold → Fallback ──

    #[test]
    fn slash_reduces_below_threshold_produces_fallback() {
        let mut policy = CommitteeRotationPolicy::with_interval(1);
        policy.exclude_slashed = true;

        // 5 validators, slash 2 → only 3 remain < MIN_COMMITTEE_SIZE(4)
        let mut records: Vec<_> = (0..5u8).map(|i| active_record(i, 2_000_000_000)).collect();
        records[1].is_slashed = true;
        records[3].is_slashed = true;

        let snapshot = StakingSnapshot::new(EpochNumber(1), records);

        let outcome = attempt_rotation(Some(&snapshot), &policy, EpochNumber(1));
        match outcome {
            RotationOutcome::Fallback { reason } => match reason {
                StakingSnapshotError::InsufficientValidators { found, required } => {
                    assert_eq!(found, 3);
                    assert_eq!(required, MIN_COMMITTEE_SIZE);
                }
                other => panic!("expected InsufficientValidators, got: {other}"),
            },
            other => panic!("expected Fallback, got: {other:?}"),
        }
    }

    // ── S-4f: Penalty accumulation drops effective stake below threshold ──

    #[test]
    fn penalty_drops_validators_below_eligible_stake() {
        let policy = ElectionPolicy::default();

        // 4 validators: 3 have penalties that drop them below MIN_ELIGIBLE_STAKE
        let mut records = vec![
            active_record(0, 2_000_000_000), // 2B effective
            active_record(1, 1_500_000_000), // will have 1.5B - 1B = 500M < 1B → ineligible
            active_record(2, 1_200_000_000), // will have 1.2B - 800M = 400M → ineligible
            active_record(3, 1_100_000_000), // will have 1.1B - 900M = 200M → ineligible
        ];
        records[1].penalty_total = 1_000_000_000;
        records[2].penalty_total = 800_000_000;
        records[3].penalty_total = 900_000_000;

        let snapshot = StakingSnapshot::new(EpochNumber(1), records);
        let result = elect_committee(&snapshot, &policy, EpochNumber(1));

        match result {
            Err(StakingSnapshotError::InsufficientValidators { found, required }) => {
                assert_eq!(found, 1); // only validator 0 eligible
                assert_eq!(required, MIN_COMMITTEE_SIZE);
            }
            other => panic!("expected InsufficientValidators, got: {other:?}"),
        }
    }

    // ── S-4g: Non-active validators excluded ──

    #[test]
    fn unbonding_and_withdrawn_validators_excluded() {
        let policy = ElectionPolicy::default();

        let mut records: Vec<_> = (0..6u8).map(|i| active_record(i, 2_000_000_000)).collect();
        // validator 2: unbonding (status 1)
        records[2].status = 1;
        // validator 4: withdrawn (status 2)
        records[4].status = 2;

        let snapshot = StakingSnapshot::new(EpochNumber(1), records);
        let result = elect_committee(&snapshot, &policy, EpochNumber(1)).unwrap();

        // 6 - 2 non-active = 4 elected
        assert_eq!(result.elected.len(), 4);

        let elected_addrs: Vec<_> = result.elected.iter().map(|e| e.address).collect();
        assert!(
            !elected_addrs.contains(&addr(2)),
            "unbonding should be excluded"
        );
        assert!(
            !elected_addrs.contains(&addr(4)),
            "withdrawn should be excluded"
        );
    }

    // ── S-4h: Slash after rotation → next rotation reflects slash ──

    #[test]
    fn slash_reflected_in_subsequent_rotation() {
        let mut policy = CommitteeRotationPolicy::with_interval(1);
        policy.exclude_slashed = true;

        // Epoch 1: all active, validator 3 has highest stake
        let mut records: Vec<_> = (0..5u8).map(|i| active_record(i, 2_000_000_000)).collect();
        records[3].bonded = 10_000_000_000;

        let snap1 = StakingSnapshot::new(EpochNumber(0), records.clone());
        let outcome1 = attempt_rotation(Some(&snap1), &policy, EpochNumber(1));
        match &outcome1 {
            RotationOutcome::Elected(r) => {
                // Validator 3 should be first (highest stake)
                assert_eq!(r.elected[0].address, addr(3));
            }
            other => panic!("expected Elected, got: {other:?}"),
        }

        // Epoch 2: validator 3 slashed
        records[3].is_slashed = true;
        let snap2 = StakingSnapshot::new(EpochNumber(1), records);
        let outcome2 = attempt_rotation(Some(&snap2), &policy, EpochNumber(2));
        match &outcome2 {
            RotationOutcome::Elected(r) => {
                let addrs: Vec<_> = r.elected.iter().map(|e| e.address).collect();
                assert!(
                    !addrs.contains(&addr(3)),
                    "slashed validator 3 must be excluded"
                );
                assert_eq!(r.elected.len(), 4);
            }
            other => panic!("expected Elected, got: {other:?}"),
        }
    }

    // ── S-4i: Election interval config vs. actual behavior ──

    #[test]
    fn election_interval_matches_configured_behavior() {
        let records: Vec<_> = (0..5u8).map(|i| active_record(i, 2_000_000_000)).collect();

        for interval in [1u64, 2, 3, 5, 10] {
            let policy = CommitteeRotationPolicy::with_interval(interval);
            let mut election_epochs = Vec::new();

            for epoch in 0..=30u64 {
                let snap = StakingSnapshot::new(EpochNumber(epoch), records.clone());
                let outcome = attempt_rotation(Some(&snap), &policy, EpochNumber(epoch));
                match outcome {
                    RotationOutcome::Elected(_) => election_epochs.push(epoch),
                    RotationOutcome::NotElectionEpoch => {}
                    RotationOutcome::Fallback { .. } => {
                        panic!("unexpected fallback at epoch {epoch}")
                    }
                }
            }

            // Verify: elections happen exactly at epochs divisible by interval, excluding 0
            let expected: Vec<u64> = (1..=30u64).filter(|e| e % interval == 0).collect();
            assert_eq!(
                election_epochs, expected,
                "interval={interval}: election epochs mismatch"
            );
        }
    }

    // ── S-4j: Interval 0 treated as 1 ──

    #[test]
    fn zero_interval_treated_as_every_epoch() {
        let policy = CommitteeRotationPolicy::with_interval(0);
        let records: Vec<_> = (0..4u8).map(|i| active_record(i, 2_000_000_000)).collect();

        let mut elected_count = 0;
        for epoch in 1..=5u64 {
            let snap = StakingSnapshot::new(EpochNumber(epoch - 1), records.clone());
            match attempt_rotation(Some(&snap), &policy, EpochNumber(epoch)) {
                RotationOutcome::Elected(_) => elected_count += 1,
                other => panic!("epoch {epoch}: expected Elected, got: {other:?}"),
            }
        }
        assert_eq!(elected_count, 5, "interval=0 should elect every epoch");
    }

    // ── S-4k: Epoch 0 never elects (genesis committee) ──

    #[test]
    fn epoch_zero_never_elects() {
        let policy = CommitteeRotationPolicy::with_interval(1);
        let records: Vec<_> = (0..5u8).map(|i| active_record(i, 2_000_000_000)).collect();
        let snap = StakingSnapshot::new(EpochNumber(0), records);

        let outcome = attempt_rotation(Some(&snap), &policy, EpochNumber(0));
        assert!(
            matches!(outcome, RotationOutcome::NotElectionEpoch),
            "epoch 0 must use genesis committee"
        );
    }

    // ── S-4l: Massive slash leaves exactly threshold validators ──

    #[test]
    fn slash_exactly_at_threshold_still_elects() {
        let mut policy = CommitteeRotationPolicy::with_interval(1);
        policy.exclude_slashed = true;

        // 7 validators, slash 3 → exactly 4 remain = MIN_COMMITTEE_SIZE
        let mut records: Vec<_> = (0..7u8).map(|i| active_record(i, 2_000_000_000)).collect();
        records[0].is_slashed = true;
        records[3].is_slashed = true;
        records[5].is_slashed = true;

        let snapshot = StakingSnapshot::new(EpochNumber(1), records);

        let outcome = attempt_rotation(Some(&snapshot), &policy, EpochNumber(1));
        match outcome {
            RotationOutcome::Elected(r) => {
                assert_eq!(r.elected.len(), MIN_COMMITTEE_SIZE);
                // Total staked = 4 × 2B = 8B ≥ 4B threshold
                assert!(r.total_effective_stake >= MIN_TOTAL_EFFECTIVE_STAKE);
            }
            other => panic!("expected Elected at threshold, got: {other:?}"),
        }
    }

    // ── S-4m: Mixed failure conditions ──

    #[test]
    fn mixed_slash_penalty_unbond_reduces_eligible_set() {
        let mut policy = CommitteeRotationPolicy::with_interval(1);
        policy.exclude_slashed = true;

        let mut records: Vec<_> = (0..8u8).map(|i| active_record(i, 2_000_000_000)).collect();
        // validator 0: slashed
        records[0].is_slashed = true;
        // validator 1: unbonding
        records[1].status = 1;
        // validator 2: withdrawn
        records[2].status = 2;
        // validator 3: penalty drops below eligible
        records[3].penalty_total = 1_500_000_000; // effective = 500M < 1B

        // Remaining eligible: 4, 5, 6, 7 = exactly MIN_COMMITTEE_SIZE
        let snapshot = StakingSnapshot::new(EpochNumber(1), records);

        let outcome = attempt_rotation(Some(&snapshot), &policy, EpochNumber(1));
        match outcome {
            RotationOutcome::Elected(r) => {
                assert_eq!(r.elected.len(), 4);
                let addrs: Vec<_> = r.elected.iter().map(|e| e.address).collect();
                assert!(!addrs.contains(&addr(0)));
                assert!(!addrs.contains(&addr(1)));
                assert!(!addrs.contains(&addr(2)));
                assert!(!addrs.contains(&addr(3)));
            }
            other => panic!("expected Elected, got: {other:?}"),
        }
    }
}
