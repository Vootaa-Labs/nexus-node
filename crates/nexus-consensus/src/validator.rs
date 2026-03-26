// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! PoS committee management and quorum computation.
//!
//! [`Committee`] is the canonical implementation of [`ValidatorRegistry`].
//! It stores the active validator set for a single epoch and provides
//! O(1) quorum checks via pre-computed thresholds.
//!
//! # Quorum rule
//!
//! Byzantine tolerance requires strictly more than 2/3 of total stake.
//! Quorum threshold: `⌊total_stake × 2 / 3⌋ + 1` (**stake-weighted**).

use crate::error::{ConsensusError, ConsensusResult};
use crate::traits::ValidatorRegistry;
use crate::types::{ReputationScore, ValidatorBitset, ValidatorInfo};
use nexus_primitives::{Amount, EpochNumber, ValidatorIndex};

/// PoS validator committee for a single epoch.
///
/// Validators are indexed by [`ValidatorIndex`] (zero-based).
/// Slashing takes effect immediately and recalculates the quorum threshold.
#[derive(Debug, Clone)]
pub struct Committee {
    epoch: EpochNumber,
    /// All validators (including slashed).
    validators: Vec<ValidatorInfo>,
    /// Pre-computed stake-weighted quorum threshold (⌊total_stake × 2/3⌋ + 1).
    quorum: Amount,
    /// Pre-computed total stake of active validators.
    total_stake: Amount,
}

impl Committee {
    /// Create a new committee from a list of validators.
    ///
    /// Validators are stored in the order provided. Each validator's
    /// `index` field should be unique and match its position.
    ///
    /// # Errors
    ///
    /// Returns [`ConsensusError::StakeOverflow`] if total active stake exceeds `u64::MAX`.
    pub fn new(epoch: EpochNumber, validators: Vec<ValidatorInfo>) -> ConsensusResult<Self> {
        let (total_stake, quorum) = Self::compute_stats(&validators)?;
        Ok(Self {
            epoch,
            validators,
            quorum,
            total_stake,
        })
    }

    /// The epoch this committee is valid for.
    pub fn epoch(&self) -> EpochNumber {
        self.epoch
    }

    /// Number of active (non-slashed) validators.
    pub fn active_count(&self) -> u32 {
        self.validators.iter().filter(|v| !v.is_slashed).count() as u32
    }

    /// Total number of validators (including slashed).
    pub fn total_count(&self) -> u32 {
        self.validators.len() as u32
    }

    /// Slash a validator immediately.
    ///
    /// Recalculates quorum threshold and total stake.
    ///
    /// # Errors
    ///
    /// Returns [`ConsensusError::UnknownValidator`] if the index is not in the committee.
    /// Returns [`ConsensusError::SlashedValidator`] if already slashed.
    pub fn slash(&mut self, index: ValidatorIndex) -> ConsensusResult<()> {
        let v = self
            .validators
            .iter_mut()
            .find(|v| v.index == index)
            .ok_or(ConsensusError::UnknownValidator(index))?;

        if v.is_slashed {
            return Err(ConsensusError::SlashedValidator(index));
        }
        v.is_slashed = true;

        let (total_stake, quorum) = Self::compute_stats(&self.validators)?;
        self.total_stake = total_stake;
        self.quorum = quorum;

        Ok(())
    }

    /// Update a validator's reputation score.
    ///
    /// # Errors
    ///
    /// Returns [`ConsensusError::UnknownValidator`] if the index is not in the committee.
    pub fn set_reputation(
        &mut self,
        index: ValidatorIndex,
        score: ReputationScore,
    ) -> ConsensusResult<()> {
        let v = self
            .validators
            .iter_mut()
            .find(|v| v.index == index)
            .ok_or(ConsensusError::UnknownValidator(index))?;
        v.reputation = score;
        Ok(())
    }

    /// Compute stake-weighted quorum threshold and total stake from current validator set.
    ///
    /// Quorum = ⌊total_stake × 2 / 3⌋ + 1  (strictly more than 2/3).
    ///
    /// Returns an error if total stake overflows `u64`.
    fn compute_stats(validators: &[ValidatorInfo]) -> ConsensusResult<(Amount, Amount)> {
        let active: Vec<_> = validators.iter().filter(|v| !v.is_slashed).collect();
        let total_stake_raw = active
            .iter()
            .try_fold(0u64, |acc, v| acc.checked_add(v.stake.0))
            .ok_or(ConsensusError::StakeOverflow)?;
        let quorum_stake = total_stake_raw
            .checked_mul(2)
            .ok_or(ConsensusError::StakeOverflow)?
            / 3
            + 1;
        Ok((Amount(total_stake_raw), Amount(quorum_stake)))
    }

    /// Read-only snapshot of all validators (including slashed).
    pub fn all_validators(&self) -> &[ValidatorInfo] {
        &self.validators
    }

    /// Snapshot the committee for persistence.
    ///
    /// The returned [`PersistentCommittee`] contains only the data
    /// needed to reconstruct the committee on cold restart.
    pub fn to_persistent(&self) -> crate::types::PersistentCommittee {
        crate::types::PersistentCommittee {
            epoch: self.epoch,
            validators: self.validators.clone(),
        }
    }

    /// Reconstruct a committee from a persisted snapshot.
    ///
    /// Recomputes the pre-computed quorum threshold and total stake.
    pub fn from_persistent(snap: crate::types::PersistentCommittee) -> ConsensusResult<Self> {
        Self::new(snap.epoch, snap.validators)
    }
}

impl ValidatorRegistry for Committee {
    fn active_validators(&self) -> Vec<ValidatorInfo> {
        self.validators
            .iter()
            .filter(|v| !v.is_slashed)
            .cloned()
            .collect()
    }

    fn validator_info(&self, index: ValidatorIndex) -> Option<ValidatorInfo> {
        self.validators.iter().find(|v| v.index == index).cloned()
    }

    fn quorum_threshold(&self) -> Amount {
        self.quorum
    }

    fn total_stake(&self) -> Amount {
        self.total_stake
    }

    fn is_quorum(&self, signers: &ValidatorBitset) -> bool {
        let signer_stake: u64 = self
            .validators
            .iter()
            .filter(|v| !v.is_slashed && signers.is_set(v.index))
            .map(|v| v.stake.0)
            .sum();
        signer_stake >= self.quorum.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_test_utils::fixtures::crypto::make_falcon_keypair;

    /// Build a committee of `n` validators, each with `stake` tokens.
    fn make_committee(n: u32, stake: u64) -> Committee {
        let validators: Vec<ValidatorInfo> = (0..n)
            .map(|i| {
                let (_sk, vk) = make_falcon_keypair();
                ValidatorInfo {
                    index: ValidatorIndex(i),
                    falcon_pub_key: vk,
                    stake: Amount(stake),
                    reputation: ReputationScore::MAX,
                    is_slashed: false,
                    shard_id: None,
                }
            })
            .collect();
        Committee::new(EpochNumber(1), validators).expect("test committee")
    }

    // ── Quorum threshold tests ───────────────────────────────────────────

    #[test]
    fn quorum_single_validator() {
        let c = make_committee(1, 100);
        // total_stake=100, quorum = 100*2/3+1 = 67
        assert_eq!(c.quorum_threshold(), Amount(67));
        assert_eq!(c.active_count(), 1);
    }

    #[test]
    fn quorum_four_validators() {
        let c = make_committee(4, 100);
        // total_stake=400, quorum = 400*2/3+1 = 267
        assert_eq!(c.quorum_threshold(), Amount(267));
    }

    #[test]
    fn quorum_100_validators() {
        let c = make_committee(100, 1000);
        // total_stake=100000, quorum = 100000*2/3+1 = 66667
        assert_eq!(c.quorum_threshold(), Amount(66667));
        assert_eq!(c.active_count(), 100);
        assert_eq!(c.total_stake(), Amount(100_000));
    }

    #[test]
    fn quorum_201_validators() {
        let c = make_committee(201, 500);
        // total_stake=100500, quorum = 100500*2/3+1 = 67001
        assert_eq!(c.quorum_threshold(), Amount(67001));
    }

    // ── is_quorum tests ─────────────────────────────────────────────────

    #[test]
    fn is_quorum_exactly_at_threshold() {
        let c = make_committee(4, 100);
        // total_stake=400, quorum=267. Each validator has stake 100.
        let mut bs = ValidatorBitset::new(4);
        bs.set(ValidatorIndex(0));
        bs.set(ValidatorIndex(1));
        assert!(!c.is_quorum(&bs)); // 200 < 267
        bs.set(ValidatorIndex(2));
        assert!(c.is_quorum(&bs)); // 300 >= 267
    }

    #[test]
    fn is_quorum_excludes_slashed() {
        let mut c = make_committee(4, 100);
        c.slash(ValidatorIndex(0)).unwrap();
        // 3 active, total_stake=300, quorum = 300*2/3+1 = 201

        let mut bs = ValidatorBitset::new(4);
        bs.set(ValidatorIndex(0)); // slashed → not counted (0 stake)
        assert!(!c.is_quorum(&bs));

        bs.set(ValidatorIndex(1)); // 100 stake, still < 201
        assert!(!c.is_quorum(&bs));

        bs.set(ValidatorIndex(2)); // 200 stake, still < 201
        assert!(!c.is_quorum(&bs));

        bs.set(ValidatorIndex(3)); // 300 stake >= 201
        assert!(c.is_quorum(&bs));
    }

    // ── active_validators tests ─────────────────────────────────────────

    #[test]
    fn active_validators_excludes_slashed() {
        let mut c = make_committee(5, 100);
        assert_eq!(c.active_validators().len(), 5);

        c.slash(ValidatorIndex(2)).unwrap();
        let active = c.active_validators();
        assert_eq!(active.len(), 4);
        assert!(active.iter().all(|v| v.index != ValidatorIndex(2)));
    }

    // ── Slash tests ─────────────────────────────────────────────────────

    #[test]
    fn slash_recalculates_quorum_and_stake() {
        let mut c = make_committee(7, 100);
        // total_stake=700, quorum = 700*2/3+1 = 467
        assert_eq!(c.quorum_threshold(), Amount(467));
        assert_eq!(c.total_stake(), Amount(700));

        c.slash(ValidatorIndex(3)).unwrap();
        // 6 active, total_stake=600, quorum = 600*2/3+1 = 401
        assert_eq!(c.quorum_threshold(), Amount(401));
        assert_eq!(c.total_stake(), Amount(600));
        assert_eq!(c.active_count(), 6);
    }

    #[test]
    fn slash_unknown_validator_errors() {
        let mut c = make_committee(3, 100);
        let result = c.slash(ValidatorIndex(99));
        assert!(matches!(
            result,
            Err(ConsensusError::UnknownValidator(ValidatorIndex(99)))
        ));
    }

    #[test]
    fn slash_already_slashed_errors() {
        let mut c = make_committee(3, 100);
        c.slash(ValidatorIndex(1)).unwrap();
        let result = c.slash(ValidatorIndex(1));
        assert!(matches!(
            result,
            Err(ConsensusError::SlashedValidator(ValidatorIndex(1)))
        ));
    }

    // ── Reputation tests ────────────────────────────────────────────────

    #[test]
    fn set_reputation_updates_validator() {
        let mut c = make_committee(3, 100);
        let half = ReputationScore::from_f32(0.5);
        c.set_reputation(ValidatorIndex(1), half).unwrap();

        let info = c.validator_info(ValidatorIndex(1)).unwrap();
        assert_eq!(info.reputation, half);
    }

    #[test]
    fn set_reputation_unknown_validator_errors() {
        let mut c = make_committee(3, 100);
        let result = c.set_reputation(ValidatorIndex(99), ReputationScore::ZERO);
        assert!(matches!(
            result,
            Err(ConsensusError::UnknownValidator(ValidatorIndex(99)))
        ));
    }

    // ── Registry default methods ────────────────────────────────────────

    #[test]
    fn is_active_returns_false_for_slashed() {
        let mut c = make_committee(3, 100);
        assert!(c.is_active(ValidatorIndex(0)));
        c.slash(ValidatorIndex(0)).unwrap();
        assert!(!c.is_active(ValidatorIndex(0)));
    }

    #[test]
    fn reputation_returns_zero_for_unknown() {
        let c = make_committee(3, 100);
        assert_eq!(c.reputation(ValidatorIndex(99)), ReputationScore::ZERO);
    }

    // ── Edge cases ──────────────────────────────────────────────────────

    #[test]
    fn empty_committee_quorum_never_met() {
        let c = Committee::new(EpochNumber(1), vec![]).expect("test committee");
        // total_stake=0, quorum = 0*2/3+1 = 1 → can never be met
        assert_eq!(c.quorum_threshold(), Amount(1));
        assert_eq!(c.active_count(), 0);

        let bs = ValidatorBitset::new(0);
        assert!(!c.is_quorum(&bs));
    }

    #[test]
    fn validator_info_returns_none_for_unknown() {
        let c = make_committee(3, 100);
        assert!(c.validator_info(ValidatorIndex(99)).is_none());
    }

    #[test]
    fn epoch_is_stored() {
        let c = make_committee(4, 100);
        assert_eq!(c.epoch(), EpochNumber(1));
    }

    #[test]
    fn total_count_includes_slashed() {
        let mut c = make_committee(5, 100);
        c.slash(ValidatorIndex(0)).unwrap();
        assert_eq!(c.total_count(), 5);
        assert_eq!(c.active_count(), 4);
    }

    // ── SEC-M-002: stake overflow returns error ─────────────────────────

    #[test]
    fn stake_overflow_returns_error() {
        let validators: Vec<ValidatorInfo> = (0..2)
            .map(|i| {
                let (_sk, vk) = make_falcon_keypair();
                ValidatorInfo {
                    index: ValidatorIndex(i),
                    falcon_pub_key: vk,
                    stake: Amount(u64::MAX / 2 + 1),
                    reputation: ReputationScore::MAX,
                    is_slashed: false,
                    shard_id: None,
                }
            })
            .collect();
        let result = Committee::new(EpochNumber(1), validators);
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("stake overflow"),
            "expected StakeOverflow error"
        );
    }
}
