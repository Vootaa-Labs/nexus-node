// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Staking snapshot reader — reads on-chain staking state at epoch boundaries.
//!
//! At each election boundary the node must derive the next committee from
//! the canonical committed state.  This module provides a deterministic
//! pipeline:
//!
//! 1. Read each known validator's `ValidatorStake` resource from committed
//!    state (via the staking contract's view functions or raw resource keys).
//! 2. Filter to eligible candidates (active, effective stake ≥ minimum).
//! 3. Sort by effective stake descending, with a stable tie-breaker on
//!    the raw account address bytes (lexicographic ascending).
//! 4. Return a `StakingSnapshot` that can be fed to the election policy.
//!
//! ## Epoch Activation Semantics
//!
//! The rule is:
//!
//! > **Stake changes committed before block `B` are reflected in the state
//! > root of `B`.  When the epoch manager decides to advance at commit
//! > `B`, the staking snapshot is read from that state root.  The derived
//! > committee takes effect at `epoch + 1`.**
//!
//! This guarantees that every node reading the same committed state root
//! produces an identical committee — the election is a pure function of
//! committed state.

use nexus_consensus::types::{ReputationScore, ValidatorInfo};
use nexus_consensus::Committee;
use nexus_primitives::{AccountAddress, Amount, EpochNumber, ValidatorIndex};
use serde::{Deserialize, Serialize};
use std::fmt;

// ── Error ────────────────────────────────────────────────────────────────────

/// Errors from staking snapshot operations.
#[derive(Debug)]
pub enum StakingSnapshotError {
    /// Too few eligible validators to form a committee.
    InsufficientValidators {
        /// Number of eligible candidates found.
        found: usize,
        /// Minimum required.
        required: usize,
    },
    /// Total effective stake is below the safety threshold.
    InsufficientTotalStake {
        /// Sum of effective stake across all eligible candidates.
        total: u64,
        /// Minimum required total stake.
        required: u64,
    },
    /// Failed to construct a `Committee` from the elected set.
    CommitteeConstruction(String),
}

impl fmt::Display for StakingSnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InsufficientValidators { found, required } => {
                write!(
                    f,
                    "insufficient validators: found {found}, need at least {required}"
                )
            }
            Self::InsufficientTotalStake { total, required } => {
                write!(
                    f,
                    "insufficient total stake: {total} voo, need at least {required} voo"
                )
            }
            Self::CommitteeConstruction(reason) => {
                write!(f, "committee construction failed: {reason}")
            }
        }
    }
}

impl std::error::Error for StakingSnapshotError {}

// ── Staking Record (Rust mirror of Move struct) ──────────────────────────────

/// Rust-side mirror of a single `ValidatorStake` record from the staking
/// contract.  Values are read from committed state at epoch boundaries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatorStakeRecord {
    /// Validator account address.
    pub address: AccountAddress,
    /// Total bonded stake in voo.
    pub bonded: u64,
    /// Cumulative penalties applied.
    pub penalty_total: u64,
    /// Status code (0 = Active, 1 = Unbonding, 2 = Withdrawn).
    pub status: u8,
    /// Epoch at which the validator registered.
    pub registered_epoch: u64,
    /// Epoch at which unbonding was requested.
    pub unbond_epoch: u64,
    /// Whether this validator was slashed in the current consensus epoch.
    /// Populated from the live committee state at election time.
    #[serde(default)]
    pub is_slashed: bool,
    /// Reputation score from the consensus layer (0–10000, default MAX).
    /// Populated from the live committee state at election time.
    #[serde(default = "default_reputation")]
    pub reputation: ReputationScore,
}

fn default_reputation() -> ReputationScore {
    ReputationScore::default()
}

impl ValidatorStakeRecord {
    /// Effective stake: bonded minus penalties. Returns 0 if penalties
    /// exceed bonded amount.
    pub fn effective_stake(&self) -> u64 {
        self.bonded.saturating_sub(self.penalty_total)
    }

    /// Whether this validator is currently active.
    pub fn is_active(&self) -> bool {
        self.status == 0
    }
}

// ── Staking Snapshot ─────────────────────────────────────────────────────────

/// A frozen point-in-time view of all staking records, read from committed
/// state at an epoch boundary.  Deterministic: same records → same snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StakingSnapshot {
    /// The epoch at which this snapshot was captured.
    pub captured_at_epoch: EpochNumber,
    /// All staking records (unfiltered — includes inactive / withdrawn).
    pub records: Vec<ValidatorStakeRecord>,
}

/// Minimum number of validators required to form a safe committee.
pub const MIN_COMMITTEE_SIZE: usize = 4;

/// Minimum total effective stake (in voo) to accept an election result.
/// Default: 4 NXS (4 validators × 1 NXS minimum each).
pub const MIN_TOTAL_EFFECTIVE_STAKE: u64 = 4_000_000_000;

/// Minimum per-validator effective stake to be eligible for election (1 NXS).
pub const MIN_ELIGIBLE_STAKE: u64 = 1_000_000_000;

impl StakingSnapshot {
    /// Create a new snapshot from raw records captured at the given epoch.
    pub fn new(captured_at_epoch: EpochNumber, records: Vec<ValidatorStakeRecord>) -> Self {
        Self {
            captured_at_epoch,
            records,
        }
    }

    /// Filter to election-eligible candidates:
    /// - `status == 0` (Active)
    /// - `effective_stake >= MIN_ELIGIBLE_STAKE`
    pub fn eligible_candidates(&self) -> Vec<&ValidatorStakeRecord> {
        self.records
            .iter()
            .filter(|r| r.is_active() && r.effective_stake() >= MIN_ELIGIBLE_STAKE)
            .collect()
    }

    /// Extended eligibility filter that also considers slash and reputation
    /// state from the rotation policy.
    ///
    /// - `status == 0` (Active)
    /// - `effective_stake >= MIN_ELIGIBLE_STAKE`
    /// - If `policy.exclude_slashed`, excludes records where `is_slashed == true`
    /// - If `policy.min_reputation_score > ZERO`, excludes records below threshold
    pub fn eligible_candidates_with_policy<'a>(
        &'a self,
        policy: &CommitteeRotationPolicy,
    ) -> Vec<&'a ValidatorStakeRecord> {
        self.records
            .iter()
            .filter(|r| {
                r.is_active()
                    && r.effective_stake() >= MIN_ELIGIBLE_STAKE
                    && !(policy.exclude_slashed && r.is_slashed)
                    && r.reputation >= policy.min_reputation_score
            })
            .collect()
    }

    /// Derive the deterministic sorted candidate list using the extended
    /// policy filter.
    pub fn sorted_candidates_with_policy(
        &self,
        policy: &CommitteeRotationPolicy,
    ) -> Vec<&ValidatorStakeRecord> {
        let mut candidates = self.eligible_candidates_with_policy(policy);
        candidates.sort_by(|a, b| {
            b.effective_stake()
                .cmp(&a.effective_stake())
                .then_with(|| a.address.0.cmp(&b.address.0))
        });
        candidates
    }

    /// Derive the deterministic sorted candidate list for election.
    ///
    /// Sort order:
    /// 1. Effective stake, **descending**.
    /// 2. Tie-breaker: account address bytes, **lexicographic ascending**.
    ///
    /// This ensures every node produces the same ordering from the same
    /// snapshot.
    pub fn sorted_candidates(&self) -> Vec<&ValidatorStakeRecord> {
        let mut candidates = self.eligible_candidates();
        candidates.sort_by(|a, b| {
            // Primary: higher effective stake first.
            b.effective_stake()
                .cmp(&a.effective_stake())
                // Secondary: lower address first (stable tie-breaker).
                .then_with(|| a.address.0.cmp(&b.address.0))
        });
        candidates
    }
}

// ── Election Policy ──────────────────────────────────────────────────────────

/// Configuration for the election policy.
#[derive(Debug, Clone)]
pub struct ElectionPolicy {
    /// Maximum committee size (0 = unlimited, take all eligible).
    pub max_committee_size: usize,
    /// Minimum number of validators for a valid committee.
    pub min_committee_size: usize,
    /// Minimum total effective stake across the elected set.
    pub min_total_effective_stake: u64,
}

impl Default for ElectionPolicy {
    fn default() -> Self {
        Self {
            max_committee_size: 0,
            min_committee_size: MIN_COMMITTEE_SIZE,
            min_total_effective_stake: MIN_TOTAL_EFFECTIVE_STAKE,
        }
    }
}

/// Result of a successful election.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElectionResult {
    /// The epoch this committee is elected for.
    pub for_epoch: EpochNumber,
    /// The snapshot epoch the election was derived from.
    pub snapshot_epoch: EpochNumber,
    /// Elected validators with their assigned indices.
    pub elected: Vec<ElectedValidator>,
    /// Total effective stake across the elected set.
    pub total_effective_stake: u64,
}

/// A validator elected into the next committee.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElectedValidator {
    /// Account address.
    pub address: AccountAddress,
    /// Effective stake (bonded - penalty).
    pub effective_stake: u64,
    /// Assigned committee index (position in sorted order).
    pub committee_index: u32,
}

// ── Committee Rotation Policy ────────────────────────────────────────────────

/// Configuration governing when and how committee rotation occurs.
///
/// Wraps the per-election `ElectionPolicy` with the rotation interval
/// from `ConsensusConfig::validator_election_epoch_interval`.
#[derive(Debug, Clone)]
pub struct CommitteeRotationPolicy {
    /// Per-election safety thresholds.
    pub election: ElectionPolicy,
    /// Re-elect committee every `election_epoch_interval` epochs.
    /// A value of 1 means every epoch boundary triggers an election.
    /// A value of 0 is treated as 1 (always elect).
    pub election_epoch_interval: u64,
    /// Minimum reputation score (0.0–1.0 mapped to 0–10000) for election
    /// eligibility.  Validators below this threshold are excluded even if
    /// their stake qualifies them.  Default: 0 (no reputation filter).
    pub min_reputation_score: ReputationScore,
    /// Whether slashed validators are excluded from the next election.
    /// Default: true.
    pub exclude_slashed: bool,
}

impl Default for CommitteeRotationPolicy {
    fn default() -> Self {
        Self {
            election: ElectionPolicy::default(),
            election_epoch_interval: 1,
            min_reputation_score: ReputationScore::ZERO,
            exclude_slashed: true,
        }
    }
}

impl CommitteeRotationPolicy {
    /// Create a rotation policy from the consensus config's interval.
    pub fn with_interval(interval: u64) -> Self {
        Self {
            election_epoch_interval: if interval == 0 { 1 } else { interval },
            ..Default::default()
        }
    }
}

/// Determine whether an election should occur at the given epoch boundary.
///
/// Returns `true` when `next_epoch % interval == 0`, i.e. the epoch about
/// to start is an election boundary.  Epoch 0 is never an election — the
/// genesis committee is used.
pub fn is_election_boundary(next_epoch: EpochNumber, interval: u64) -> bool {
    let interval = if interval == 0 { 1 } else { interval };
    if next_epoch.0 == 0 {
        return false;
    }
    next_epoch.0 % interval == 0
}

// ── Election Outcome Enum ────────────────────────────────────────────────────

/// Outcome of a committee rotation attempt at an epoch boundary.
#[derive(Debug)]
pub enum RotationOutcome {
    /// A new committee was elected from the staking snapshot.
    Elected(ElectionResult),
    /// This epoch is not an election boundary — carry forward the
    /// current committee.
    NotElectionEpoch,
    /// Election failed (insufficient validators/stake) — the current
    /// committee is carried forward as a safety fallback.
    Fallback {
        /// The error that prevented election.
        reason: StakingSnapshotError,
    },
}

// ── Persisted Election Result ────────────────────────────────────────────────

/// Serialisable record of an election result, persisted alongside the
/// epoch transition so cold-start recovery knows how the committee was
/// derived.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedElectionResult {
    /// The epoch this committee was elected for.
    pub for_epoch: EpochNumber,
    /// The snapshot epoch the election was derived from.
    pub snapshot_epoch: EpochNumber,
    /// Elected validator addresses and effective stakes.
    pub elected: Vec<ElectedValidator>,
    /// Total effective stake of the elected set.
    pub total_effective_stake: u64,
    /// Whether this was a real election or a fallback carry-forward.
    pub is_fallback: bool,
}

impl From<&ElectionResult> for PersistedElectionResult {
    fn from(r: &ElectionResult) -> Self {
        Self {
            for_epoch: r.for_epoch,
            snapshot_epoch: r.snapshot_epoch,
            elected: r.elected.clone(),
            total_effective_stake: r.total_effective_stake,
            is_fallback: false,
        }
    }
}

/// Run the deterministic election from a staking snapshot.
///
/// This is a **pure function**: same snapshot + same policy → same result.
/// No randomness, no external state reads.
pub fn elect_committee(
    snapshot: &StakingSnapshot,
    policy: &ElectionPolicy,
    for_epoch: EpochNumber,
) -> Result<ElectionResult, StakingSnapshotError> {
    let sorted = snapshot.sorted_candidates();

    // Apply maximum committee size limit.
    let elected_records: Vec<&ValidatorStakeRecord> = if policy.max_committee_size > 0 {
        sorted.into_iter().take(policy.max_committee_size).collect()
    } else {
        sorted
    };

    // Safety check: minimum committee size.
    if elected_records.len() < policy.min_committee_size {
        return Err(StakingSnapshotError::InsufficientValidators {
            found: elected_records.len(),
            required: policy.min_committee_size,
        });
    }

    // Safety check: minimum total effective stake.
    let total_effective: u64 = elected_records.iter().map(|r| r.effective_stake()).sum();

    if total_effective < policy.min_total_effective_stake {
        return Err(StakingSnapshotError::InsufficientTotalStake {
            total: total_effective,
            required: policy.min_total_effective_stake,
        });
    }

    let elected: Vec<ElectedValidator> = elected_records
        .iter()
        .enumerate()
        .map(|(i, r)| ElectedValidator {
            address: r.address,
            effective_stake: r.effective_stake(),
            committee_index: i as u32,
        })
        .collect();

    Ok(ElectionResult {
        for_epoch,
        snapshot_epoch: snapshot.captured_at_epoch,
        elected,
        total_effective_stake: total_effective,
    })
}

/// Run the deterministic election using a full `CommitteeRotationPolicy`.
///
/// This variant applies slash and reputation filtering from the policy
/// before running the standard election logic.
pub fn elect_committee_with_policy(
    snapshot: &StakingSnapshot,
    policy: &CommitteeRotationPolicy,
    for_epoch: EpochNumber,
) -> Result<ElectionResult, StakingSnapshotError> {
    let sorted = snapshot.sorted_candidates_with_policy(policy);

    let elected_records: Vec<&ValidatorStakeRecord> = if policy.election.max_committee_size > 0 {
        sorted
            .into_iter()
            .take(policy.election.max_committee_size)
            .collect()
    } else {
        sorted
    };

    if elected_records.len() < policy.election.min_committee_size {
        return Err(StakingSnapshotError::InsufficientValidators {
            found: elected_records.len(),
            required: policy.election.min_committee_size,
        });
    }

    let total_effective: u64 = elected_records.iter().map(|r| r.effective_stake()).sum();

    if total_effective < policy.election.min_total_effective_stake {
        return Err(StakingSnapshotError::InsufficientTotalStake {
            total: total_effective,
            required: policy.election.min_total_effective_stake,
        });
    }

    let elected: Vec<ElectedValidator> = elected_records
        .iter()
        .enumerate()
        .map(|(i, r)| ElectedValidator {
            address: r.address,
            effective_stake: r.effective_stake(),
            committee_index: i as u32,
        })
        .collect();

    Ok(ElectionResult {
        for_epoch,
        snapshot_epoch: snapshot.captured_at_epoch,
        elected,
        total_effective_stake: total_effective,
    })
}

/// Attempt the full committee rotation pipeline.
///
/// 1. Check if `next_epoch` is an election boundary.
/// 2. If yes, run the election using `snapshot` and `policy`.
/// 3. If election fails, return `Fallback` so the caller can carry
///    forward the current committee.
/// 4. If not an election boundary, return `NotElectionEpoch`.
///
/// This is a **pure function** that does not read from storage.
pub fn attempt_rotation(
    snapshot: Option<&StakingSnapshot>,
    policy: &CommitteeRotationPolicy,
    next_epoch: EpochNumber,
) -> RotationOutcome {
    if !is_election_boundary(next_epoch, policy.election_epoch_interval) {
        return RotationOutcome::NotElectionEpoch;
    }

    let snapshot = match snapshot {
        Some(s) => s,
        None => {
            return RotationOutcome::Fallback {
                reason: StakingSnapshotError::InsufficientValidators {
                    found: 0,
                    required: policy.election.min_committee_size,
                },
            };
        }
    };

    match elect_committee_with_policy(snapshot, policy, next_epoch) {
        Ok(result) => RotationOutcome::Elected(result),
        Err(e) => RotationOutcome::Fallback { reason: e },
    }
}

/// Convert an `ElectionResult` into a consensus `Committee`.
///
/// This bridges the staking layer to the consensus layer by constructing
/// `ValidatorInfo` entries from the elected set.  The caller must provide
/// the public keys for each elected validator (looked up from chain
/// identity or genesis state).
///
/// `key_lookup` maps `AccountAddress → FalconVerifyKey`.  If a key is
/// missing for an elected validator the function returns an error.
pub fn election_to_committee(
    result: &ElectionResult,
    key_lookup: &dyn Fn(&AccountAddress) -> Option<nexus_crypto::FalconVerifyKey>,
) -> Result<Committee, StakingSnapshotError> {
    let mut validators = Vec::with_capacity(result.elected.len());

    for ev in &result.elected {
        let pub_key = key_lookup(&ev.address).ok_or_else(|| {
            StakingSnapshotError::CommitteeConstruction(format!(
                "missing public key for validator {}",
                hex::encode(ev.address.0)
            ))
        })?;

        validators.push(ValidatorInfo {
            index: ValidatorIndex(ev.committee_index),
            falcon_pub_key: pub_key,
            stake: Amount(ev.effective_stake),
            reputation: ReputationScore::default(),
            is_slashed: false,
            shard_id: None,
        });
    }

    Committee::new(result.for_epoch, validators)
        .map_err(|e| StakingSnapshotError::CommitteeConstruction(format!("consensus error: {e}")))
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(b: u8) -> AccountAddress {
        AccountAddress([b; 32])
    }

    fn record(b: u8, bonded: u64, penalty: u64, status: u8) -> ValidatorStakeRecord {
        ValidatorStakeRecord {
            address: addr(b),
            bonded,
            penalty_total: penalty,
            status,
            registered_epoch: 0,
            unbond_epoch: 0,
            is_slashed: false,
            reputation: ReputationScore::default(),
        }
    }

    fn slashed_record(b: u8, bonded: u64, penalty: u64) -> ValidatorStakeRecord {
        ValidatorStakeRecord {
            address: addr(b),
            bonded,
            penalty_total: penalty,
            status: 0,
            registered_epoch: 0,
            unbond_epoch: 0,
            is_slashed: true,
            reputation: ReputationScore::default(),
        }
    }

    fn low_rep_record(b: u8, bonded: u64, rep: f32) -> ValidatorStakeRecord {
        ValidatorStakeRecord {
            address: addr(b),
            bonded,
            penalty_total: 0,
            status: 0,
            registered_epoch: 0,
            unbond_epoch: 0,
            is_slashed: false,
            reputation: ReputationScore::from_f32(rep),
        }
    }

    fn one_nxs() -> u64 {
        1_000_000_000
    }

    #[test]
    fn effective_stake_calculation() {
        let r = record(1, 5 * one_nxs(), one_nxs(), 0);
        assert_eq!(r.effective_stake(), 4 * one_nxs());
    }

    #[test]
    fn effective_stake_penalty_exceeds_bonded() {
        let r = record(1, one_nxs(), 2 * one_nxs(), 0);
        assert_eq!(r.effective_stake(), 0);
    }

    #[test]
    fn eligible_candidates_filters_inactive() {
        let snap = StakingSnapshot::new(
            EpochNumber(5),
            vec![
                record(1, 2 * one_nxs(), 0, 0), // active, eligible
                record(2, 2 * one_nxs(), 0, 1), // unbonding — excluded
                record(3, 2 * one_nxs(), 0, 2), // withdrawn — excluded
                record(4, one_nxs() / 2, 0, 0), // active but below min — excluded
            ],
        );
        let eligible = snap.eligible_candidates();
        assert_eq!(eligible.len(), 1);
        assert_eq!(eligible[0].address, addr(1));
    }

    #[test]
    fn sorted_candidates_deterministic_order() {
        let snap = StakingSnapshot::new(
            EpochNumber(1),
            vec![
                record(3, 5 * one_nxs(), 0, 0),
                record(1, 10 * one_nxs(), 0, 0),
                record(2, 5 * one_nxs(), 0, 0), // same stake as 3, lower address
            ],
        );

        let sorted = snap.sorted_candidates();
        assert_eq!(sorted.len(), 3);
        // Highest stake first.
        assert_eq!(sorted[0].address, addr(1));
        // Tie-breaker: lower address first.
        assert_eq!(sorted[1].address, addr(2));
        assert_eq!(sorted[2].address, addr(3));
    }

    #[test]
    fn elect_committee_minimum_size_check() {
        let snap = StakingSnapshot::new(
            EpochNumber(1),
            vec![
                record(1, 2 * one_nxs(), 0, 0),
                record(2, 2 * one_nxs(), 0, 0),
                // only 2 eligible — below default min of 4
            ],
        );
        let policy = ElectionPolicy::default();
        let result = elect_committee(&snap, &policy, EpochNumber(2));
        assert!(matches!(
            result,
            Err(StakingSnapshotError::InsufficientValidators { found: 2, .. })
        ));
    }

    #[test]
    fn elect_committee_minimum_stake_check() {
        let snap = StakingSnapshot::new(
            EpochNumber(1),
            vec![
                record(1, one_nxs(), 0, 0),
                record(2, one_nxs(), 0, 0),
                record(3, one_nxs(), 0, 0),
                record(4, one_nxs(), 0, 0),
                // 4 validators × 1 NXS = 4 NXS total — exactly at threshold
            ],
        );
        let policy = ElectionPolicy::default();
        let result = elect_committee(&snap, &policy, EpochNumber(2));
        assert!(result.is_ok());
        let er = result.unwrap();
        assert_eq!(er.elected.len(), 4);
        assert_eq!(er.total_effective_stake, 4 * one_nxs());
    }

    #[test]
    fn elect_committee_respects_max_size() {
        let snap = StakingSnapshot::new(
            EpochNumber(1),
            vec![
                record(1, 10 * one_nxs(), 0, 0),
                record(2, 8 * one_nxs(), 0, 0),
                record(3, 6 * one_nxs(), 0, 0),
                record(4, 4 * one_nxs(), 0, 0),
                record(5, 2 * one_nxs(), 0, 0),
                record(6, one_nxs(), 0, 0),
            ],
        );
        let policy = ElectionPolicy {
            max_committee_size: 4,
            min_committee_size: 4,
            min_total_effective_stake: MIN_TOTAL_EFFECTIVE_STAKE,
        };
        let result = elect_committee(&snap, &policy, EpochNumber(2)).unwrap();
        // Top 4 by stake: addr(1), addr(2), addr(3), addr(4)
        assert_eq!(result.elected.len(), 4);
        assert_eq!(result.elected[0].address, addr(1));
        assert_eq!(result.elected[3].address, addr(4));
    }

    #[test]
    fn elect_committee_penalty_reduces_rank() {
        // Validator 1 has high bonded but heavy penalty; validator 2 wins.
        let snap = StakingSnapshot::new(
            EpochNumber(1),
            vec![
                record(1, 10 * one_nxs(), 8 * one_nxs(), 0), // effective: 2 NXS
                record(2, 5 * one_nxs(), 0, 0),              // effective: 5 NXS
                record(3, 3 * one_nxs(), 0, 0),              // effective: 3 NXS
                record(4, 2 * one_nxs(), 0, 0),              // effective: 2 NXS
            ],
        );
        let policy = ElectionPolicy {
            max_committee_size: 0,
            min_committee_size: 4,
            min_total_effective_stake: 4 * one_nxs(),
        };
        let result = elect_committee(&snap, &policy, EpochNumber(2)).unwrap();

        // Sorted: addr(2)=5, addr(3)=3, addr(1)=2, addr(4)=2
        // Tie-breaker for addr(1) vs addr(4): addr(1) < addr(4)
        assert_eq!(result.elected[0].address, addr(2));
        assert_eq!(result.elected[1].address, addr(3));
        assert_eq!(result.elected[2].address, addr(1));
        assert_eq!(result.elected[3].address, addr(4));
    }

    #[test]
    fn election_result_indices_sequential() {
        let snap = StakingSnapshot::new(
            EpochNumber(0),
            vec![
                record(1, 3 * one_nxs(), 0, 0),
                record(2, 2 * one_nxs(), 0, 0),
                record(3, 4 * one_nxs(), 0, 0),
                record(4, one_nxs(), 0, 0),
            ],
        );
        let policy = ElectionPolicy::default();
        let result = elect_committee(&snap, &policy, EpochNumber(1)).unwrap();

        for (i, ev) in result.elected.iter().enumerate() {
            assert_eq!(ev.committee_index, i as u32);
        }
    }

    // ── Phase Q: Committee Rotation Policy Tests ─────────────────────────

    #[test]
    fn is_election_boundary_interval_1() {
        // interval=1 → every epoch (except 0) is an election.
        assert!(!is_election_boundary(EpochNumber(0), 1));
        assert!(is_election_boundary(EpochNumber(1), 1));
        assert!(is_election_boundary(EpochNumber(2), 1));
        assert!(is_election_boundary(EpochNumber(100), 1));
    }

    #[test]
    fn is_election_boundary_interval_3() {
        // interval=3 → only epochs 3, 6, 9, ... are election boundaries.
        assert!(!is_election_boundary(EpochNumber(0), 3));
        assert!(!is_election_boundary(EpochNumber(1), 3));
        assert!(!is_election_boundary(EpochNumber(2), 3));
        assert!(is_election_boundary(EpochNumber(3), 3));
        assert!(!is_election_boundary(EpochNumber(4), 3));
        assert!(!is_election_boundary(EpochNumber(5), 3));
        assert!(is_election_boundary(EpochNumber(6), 3));
    }

    #[test]
    fn is_election_boundary_interval_0_treated_as_1() {
        // interval=0 is treated as 1.
        assert!(!is_election_boundary(EpochNumber(0), 0));
        assert!(is_election_boundary(EpochNumber(1), 0));
        assert!(is_election_boundary(EpochNumber(5), 0));
    }

    #[test]
    fn rotation_policy_default() {
        let policy = CommitteeRotationPolicy::default();
        assert_eq!(policy.election_epoch_interval, 1);
        assert!(policy.exclude_slashed);
        assert_eq!(policy.min_reputation_score, ReputationScore::ZERO);
    }

    #[test]
    fn rotation_policy_with_interval() {
        let policy = CommitteeRotationPolicy::with_interval(5);
        assert_eq!(policy.election_epoch_interval, 5);

        // interval=0 normalised to 1
        let policy0 = CommitteeRotationPolicy::with_interval(0);
        assert_eq!(policy0.election_epoch_interval, 1);
    }

    #[test]
    fn attempt_rotation_not_election_epoch() {
        let policy = CommitteeRotationPolicy::with_interval(3);
        // Epoch 1 is not an election boundary with interval=3.
        let outcome = attempt_rotation(None, &policy, EpochNumber(1));
        assert!(matches!(outcome, RotationOutcome::NotElectionEpoch));
    }

    #[test]
    fn attempt_rotation_no_snapshot_falls_back() {
        let policy = CommitteeRotationPolicy::with_interval(1);
        // Election epoch but no snapshot → fallback.
        let outcome = attempt_rotation(None, &policy, EpochNumber(1));
        assert!(matches!(outcome, RotationOutcome::Fallback { .. }));
    }

    #[test]
    fn attempt_rotation_insufficient_validators_falls_back() {
        let snap = StakingSnapshot::new(
            EpochNumber(0),
            vec![
                record(1, 2 * one_nxs(), 0, 0),
                record(2, 2 * one_nxs(), 0, 0),
                // only 2 — below min of 4
            ],
        );
        let policy = CommitteeRotationPolicy::default();
        let outcome = attempt_rotation(Some(&snap), &policy, EpochNumber(1));
        assert!(matches!(outcome, RotationOutcome::Fallback { .. }));
    }

    #[test]
    fn attempt_rotation_successful_election() {
        let snap = StakingSnapshot::new(
            EpochNumber(0),
            vec![
                record(1, 3 * one_nxs(), 0, 0),
                record(2, 2 * one_nxs(), 0, 0),
                record(3, 4 * one_nxs(), 0, 0),
                record(4, one_nxs(), 0, 0),
            ],
        );
        let policy = CommitteeRotationPolicy::default();
        let outcome = attempt_rotation(Some(&snap), &policy, EpochNumber(1));
        match outcome {
            RotationOutcome::Elected(result) => {
                assert_eq!(result.for_epoch, EpochNumber(1));
                assert_eq!(result.elected.len(), 4);
                // Highest stake first.
                assert_eq!(result.elected[0].address, addr(3));
            }
            _ => panic!("expected Elected outcome"),
        }
    }

    #[test]
    fn attempt_rotation_respects_interval() {
        let snap = StakingSnapshot::new(
            EpochNumber(0),
            vec![
                record(1, 3 * one_nxs(), 0, 0),
                record(2, 2 * one_nxs(), 0, 0),
                record(3, 4 * one_nxs(), 0, 0),
                record(4, one_nxs(), 0, 0),
            ],
        );
        let policy = CommitteeRotationPolicy::with_interval(5);

        // Epoch 1–4 → not election epochs.
        for e in 1..5 {
            let outcome = attempt_rotation(Some(&snap), &policy, EpochNumber(e));
            assert!(
                matches!(outcome, RotationOutcome::NotElectionEpoch),
                "epoch {e} should not be an election boundary"
            );
        }

        // Epoch 5 → election.
        let outcome = attempt_rotation(Some(&snap), &policy, EpochNumber(5));
        assert!(matches!(outcome, RotationOutcome::Elected(_)));
    }

    // ── Phase Q: Slash / Reputation Filter Tests ─────────────────────────

    #[test]
    fn slashed_validator_excluded_by_policy() {
        let snap = StakingSnapshot::new(
            EpochNumber(0),
            vec![
                record(1, 3 * one_nxs(), 0, 0),
                record(2, 2 * one_nxs(), 0, 0),
                slashed_record(3, 10 * one_nxs(), 0), // highest stake but slashed
                record(4, one_nxs(), 0, 0),
            ],
        );
        let policy = CommitteeRotationPolicy {
            exclude_slashed: true,
            ..Default::default()
        };
        let eligible = snap.eligible_candidates_with_policy(&policy);
        // Slashed validator 3 should be excluded.
        assert_eq!(eligible.len(), 3);
        assert!(eligible.iter().all(|r| r.address != addr(3)));
    }

    #[test]
    fn slashed_validator_included_when_policy_allows() {
        let snap = StakingSnapshot::new(
            EpochNumber(0),
            vec![
                record(1, 3 * one_nxs(), 0, 0),
                record(2, 2 * one_nxs(), 0, 0),
                slashed_record(3, 10 * one_nxs(), 0),
                record(4, one_nxs(), 0, 0),
            ],
        );
        let policy = CommitteeRotationPolicy {
            exclude_slashed: false,
            ..Default::default()
        };
        let eligible = snap.eligible_candidates_with_policy(&policy);
        assert_eq!(eligible.len(), 4);
    }

    #[test]
    fn low_reputation_validator_excluded_by_policy() {
        let snap = StakingSnapshot::new(
            EpochNumber(0),
            vec![
                record(1, 3 * one_nxs(), 0, 0),         // rep=MAX (default)
                record(2, 2 * one_nxs(), 0, 0),         // rep=MAX
                low_rep_record(3, 10 * one_nxs(), 0.1), // low rep
                record(4, one_nxs(), 0, 0),
            ],
        );
        let policy = CommitteeRotationPolicy {
            min_reputation_score: ReputationScore::from_f32(0.5),
            ..Default::default()
        };
        let eligible = snap.eligible_candidates_with_policy(&policy);
        assert_eq!(eligible.len(), 3);
        assert!(eligible.iter().all(|r| r.address != addr(3)));
    }

    #[test]
    fn elect_with_policy_excludes_slashed() {
        let snap = StakingSnapshot::new(
            EpochNumber(0),
            vec![
                record(1, 3 * one_nxs(), 0, 0),
                record(2, 2 * one_nxs(), 0, 0),
                slashed_record(3, 10 * one_nxs(), 0),
                record(4, one_nxs(), 0, 0),
                record(5, one_nxs(), 0, 0), // need 4 eligible after excluding 3
            ],
        );
        let policy = CommitteeRotationPolicy {
            exclude_slashed: true,
            ..Default::default()
        };
        let result = elect_committee_with_policy(&snap, &policy, EpochNumber(1)).unwrap();
        assert_eq!(result.elected.len(), 4);
        assert!(result.elected.iter().all(|e| e.address != addr(3)));
        // addr(1)=3 NXS should be first
        assert_eq!(result.elected[0].address, addr(1));
    }

    // ── Phase Q: Determinism & Stability Tests ───────────────────────────

    #[test]
    fn same_snapshot_same_election_result_across_calls() {
        let snap = StakingSnapshot::new(
            EpochNumber(5),
            vec![
                record(1, 3 * one_nxs(), 0, 0),
                record(2, 7 * one_nxs(), one_nxs(), 0),
                record(3, 4 * one_nxs(), 0, 0),
                record(4, one_nxs(), 0, 0),
            ],
        );
        let policy = ElectionPolicy::default();
        let r1 = elect_committee(&snap, &policy, EpochNumber(6)).unwrap();
        let r2 = elect_committee(&snap, &policy, EpochNumber(6)).unwrap();

        assert_eq!(r1.elected.len(), r2.elected.len());
        for (a, b) in r1.elected.iter().zip(r2.elected.iter()) {
            assert_eq!(a.address, b.address);
            assert_eq!(a.effective_stake, b.effective_stake);
            assert_eq!(a.committee_index, b.committee_index);
        }
        assert_eq!(r1.total_effective_stake, r2.total_effective_stake);
    }

    #[test]
    fn tie_breaker_is_stable_across_insertion_order() {
        // Two validators with identical stake — result must be identical
        // regardless of insertion order in the snapshot.
        let records_order_a = vec![
            record(0xAA, 5 * one_nxs(), 0, 0),
            record(0xBB, 5 * one_nxs(), 0, 0),
            record(0xCC, 3 * one_nxs(), 0, 0),
            record(0xDD, 2 * one_nxs(), 0, 0),
        ];
        let records_order_b = vec![
            record(0xDD, 2 * one_nxs(), 0, 0),
            record(0xBB, 5 * one_nxs(), 0, 0),
            record(0xCC, 3 * one_nxs(), 0, 0),
            record(0xAA, 5 * one_nxs(), 0, 0),
        ];
        let snap_a = StakingSnapshot::new(EpochNumber(1), records_order_a);
        let snap_b = StakingSnapshot::new(EpochNumber(1), records_order_b);
        let policy = ElectionPolicy::default();

        let ra = elect_committee(&snap_a, &policy, EpochNumber(2)).unwrap();
        let rb = elect_committee(&snap_b, &policy, EpochNumber(2)).unwrap();

        for (a, b) in ra.elected.iter().zip(rb.elected.iter()) {
            assert_eq!(a.address, b.address, "tie-breaker must be stable");
            assert_eq!(a.committee_index, b.committee_index);
        }
    }

    // ── Phase Q: Persisted Election Result Tests ─────────────────────────

    #[test]
    fn persisted_election_result_from_election() {
        let snap = StakingSnapshot::new(
            EpochNumber(0),
            vec![
                record(1, 3 * one_nxs(), 0, 0),
                record(2, 2 * one_nxs(), 0, 0),
                record(3, 4 * one_nxs(), 0, 0),
                record(4, one_nxs(), 0, 0),
            ],
        );
        let policy = ElectionPolicy::default();
        let result = elect_committee(&snap, &policy, EpochNumber(1)).unwrap();
        let persisted = PersistedElectionResult::from(&result);

        assert_eq!(persisted.for_epoch, EpochNumber(1));
        assert_eq!(persisted.snapshot_epoch, EpochNumber(0));
        assert_eq!(persisted.elected.len(), 4);
        assert_eq!(
            persisted.total_effective_stake,
            result.total_effective_stake
        );
        assert!(!persisted.is_fallback);
    }

    #[test]
    fn persisted_election_result_serialization_roundtrip() {
        let snap = StakingSnapshot::new(
            EpochNumber(0),
            vec![
                record(1, 3 * one_nxs(), 0, 0),
                record(2, 2 * one_nxs(), 0, 0),
                record(3, 4 * one_nxs(), 0, 0),
                record(4, one_nxs(), 0, 0),
            ],
        );
        let policy = ElectionPolicy::default();
        let result = elect_committee(&snap, &policy, EpochNumber(1)).unwrap();
        let persisted = PersistedElectionResult::from(&result);

        let bytes = bcs::to_bytes(&persisted).unwrap();
        let restored: PersistedElectionResult = bcs::from_bytes(&bytes).unwrap();

        assert_eq!(restored.for_epoch, persisted.for_epoch);
        assert_eq!(restored.elected.len(), persisted.elected.len());
        assert_eq!(
            restored.total_effective_stake,
            persisted.total_effective_stake
        );
        assert_eq!(restored.is_fallback, persisted.is_fallback);
    }

    #[test]
    fn is_active_returns_false_for_nonzero_status() {
        let r = record(1, one_nxs(), 0, 1); // status=1 → inactive
        assert!(!r.is_active());
    }

    #[test]
    fn is_active_returns_true_for_zero_status() {
        let r = record(1, one_nxs(), 0, 0);
        assert!(r.is_active());
    }

    #[test]
    fn election_to_committee_success() {
        use nexus_crypto::{FalconSigner, Signer};
        let (_, key) = FalconSigner::generate_keypair();
        let target = addr(7);
        let result = ElectionResult {
            for_epoch: EpochNumber(3),
            snapshot_epoch: EpochNumber(2),
            elected: vec![ElectedValidator {
                address: target,
                effective_stake: one_nxs(),
                committee_index: 0,
            }],
            total_effective_stake: one_nxs(),
        };
        let committee = election_to_committee(&result, &|a| {
            if *a == target {
                Some(key.clone())
            } else {
                None
            }
        })
        .expect("should build committee");
        assert_eq!(committee.all_validators().len(), 1);
    }

    #[test]
    fn election_to_committee_fails_on_missing_key() {
        let result = ElectionResult {
            for_epoch: EpochNumber(3),
            snapshot_epoch: EpochNumber(2),
            elected: vec![ElectedValidator {
                address: addr(9),
                effective_stake: one_nxs(),
                committee_index: 0,
            }],
            total_effective_stake: one_nxs(),
        };
        let err = election_to_committee(&result, &|_| None).unwrap_err();
        assert!(
            matches!(err, StakingSnapshotError::CommitteeConstruction(_)),
            "expected CommitteeConstruction error"
        );
    }

    #[test]
    fn eligible_candidates_with_policy_direct() {
        let snap = StakingSnapshot::new(
            EpochNumber(0),
            vec![
                record(1, 3 * one_nxs(), 0, 0),      // active, eligible
                slashed_record(2, 2 * one_nxs(), 0), // slashed
                record(3, 0, 0, 0),                  // zero effective stake → ineligible
            ],
        );
        let policy = CommitteeRotationPolicy {
            exclude_slashed: true,
            ..CommitteeRotationPolicy::default()
        };
        let candidates = snap.eligible_candidates_with_policy(&policy);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].address, addr(1));
    }

    // ── Error Display coverage ───────────────────────────────────────────

    #[test]
    fn error_display_insufficient_validators() {
        let err = StakingSnapshotError::InsufficientValidators { found: 2, required: 4 };
        let msg = format!("{err}");
        assert!(msg.contains("insufficient validators"), "{msg}");
        assert!(msg.contains("2"));
        assert!(msg.contains("4"));
    }

    #[test]
    fn error_display_insufficient_total_stake() {
        let err = StakingSnapshotError::InsufficientTotalStake { total: 100, required: 1000 };
        let msg = format!("{err}");
        assert!(msg.contains("insufficient total stake"), "{msg}");
        assert!(msg.contains("100"));
    }

    #[test]
    fn error_display_committee_construction() {
        let err = StakingSnapshotError::CommitteeConstruction("test reason".to_owned());
        let msg = format!("{err}");
        assert!(msg.contains("committee construction failed"), "{msg}");
        assert!(msg.contains("test reason"));
    }

    #[test]
    fn error_is_std_error() {
        let err = StakingSnapshotError::InsufficientValidators { found: 0, required: 4 };
        // Exercises the std::error::Error impl
        let _: &dyn std::error::Error = &err;
    }

    #[test]
    fn persisted_election_is_not_fallback_by_default() {
        let snap = StakingSnapshot::new(
            EpochNumber(0),
            vec![
                record(1, 3 * one_nxs(), 0, 0),
                record(2, 2 * one_nxs(), 0, 0),
                record(3, 4 * one_nxs(), 0, 0),
                record(4, one_nxs(), 0, 0),
            ],
        );
        let policy = ElectionPolicy::default();
        let result = elect_committee(&snap, &policy, EpochNumber(1)).unwrap();
        let persisted = PersistedElectionResult::from(&result);
        assert!(!persisted.is_fallback);
    }
}
