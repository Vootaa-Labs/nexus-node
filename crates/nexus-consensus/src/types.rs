// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Core consensus data types.
//!
//! Implements the Narwhal DAG and Shoal++ protocol types as specified in
//! TLD-04 §4.

use nexus_crypto::{FalconSignature, FalconVerifyKey};
use nexus_primitives::{
    BatchDigest, CertDigest, CommitSequence, EpochNumber, RoundNumber, ShardId, TimestampMs,
    ValidatorIndex,
};
use serde::{Deserialize, Serialize};

// ── Narwhal Types ────────────────────────────────────────────────────────────

/// Maximum BCS-serialized batch size (512 KiB).
pub const MAX_BATCH_SIZE: usize = 512 * 1024;

/// Domain tag for batch digest computation.
pub const BATCH_DOMAIN: &[u8] = b"nexus::narwhal::batch::v1";

/// Domain tag for certificate digest computation.
pub const CERT_DOMAIN: &[u8] = b"nexus::narwhal::cert::v1";

/// Domain tag for Shoal++ vote signing.
pub const VOTE_DOMAIN: &[u8] = b"nexus::shoal::vote::v1";

/// A batch of raw transactions proposed by a Narwhal worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NarwhalBatch {
    /// Proposing validator.
    pub origin: ValidatorIndex,
    /// DAG round in which this batch was proposed.
    pub round: RoundNumber,
    /// Raw transaction bytes (BCS-encoded).
    pub transactions: Vec<Vec<u8>>,
    /// BLAKE3 digest of `(BATCH_DOMAIN || BCS(origin, round, transactions))`.
    pub digest: BatchDigest,
    /// Wall-clock time when the batch was assembled.
    pub created_at: TimestampMs,
}

/// A certificate proving 2f+1 validators witnessed a batch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NarwhalCertificate {
    /// Epoch this certificate belongs to.
    pub epoch: EpochNumber,
    /// The batch this certificate covers.
    pub batch_digest: BatchDigest,
    /// Which validator proposed the batch.
    pub origin: ValidatorIndex,
    /// The DAG round.
    pub round: RoundNumber,
    /// Parent certificates from the previous round.
    pub parents: Vec<CertDigest>,
    /// Aggregated signatures from ≥ 2f+1 validators.
    pub signatures: Vec<(ValidatorIndex, FalconSignature)>,
    /// Compact bitmap of the signers (bit `i` = validator `i` signed).
    pub signers: ValidatorBitset,
    /// BLAKE3 digest of `(CERT_DOMAIN || BCS(header fields))`.
    pub cert_digest: CertDigest,
}

// ── Shoal++ Types ────────────────────────────────────────────────────────────

/// A vote cast during Shoal++ anchor election.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShoalVote {
    /// The validator casting this vote.
    pub voter: ValidatorIndex,
    /// The DAG round of the vote.
    pub round: RoundNumber,
    /// The certificate nominated as anchor.
    pub anchor_cert: CertDigest,
    /// FALCON-512 signature over `(VOTE_DOMAIN || BCS(voter, round, anchor_cert))`.
    pub signature: FalconSignature,
}

/// An elected Shoal++ anchor for a committed sub-DAG.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShoalAnchor {
    /// The anchor certificate digest.
    pub cert_digest: CertDigest,
    /// The round of the anchor.
    pub round: RoundNumber,
    /// The validator selected as leader for this anchor round.
    pub leader: ValidatorIndex,
    /// Reputation score of the leader at election time.
    pub reputation_score: ReputationScore,
}

/// A committed sub-DAG output by the BFT orderer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommittedBatch {
    /// The anchor certificate that triggers this commit.
    pub anchor: CertDigest,
    /// All certificates in the committed causal history (topological order).
    pub certificates: Vec<CertDigest>,
    /// Strictly monotonic global sequence number.
    pub sequence: CommitSequence,
    /// Timestamp when finality was reached.
    pub committed_at: TimestampMs,
}

/// Lifecycle status of a batch in the consensus pipeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BatchStatus {
    /// Batch received but not yet certified.
    Pending,
    /// Batch certified (included in the DAG).
    Certified {
        /// The certificate covering this batch.
        cert_digest: CertDigest,
    },
    /// Batch ordered by Shoal++ (globally sequenced).
    Ordered {
        /// Global sequence number.
        sequence: CommitSequence,
    },
    /// Batch executed and state root computed.
    Executed {
        /// State root after executing this batch.
        state_root: nexus_primitives::StateRoot,
    },
}

// ── Validator Types ──────────────────────────────────────────────────────────

/// Compact bitset representing a subset of validators.
///
/// Bit `i` set means validator with index `i` is included.
/// Supports up to 1024 validators (128 bytes).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidatorBitset {
    /// Raw bits, little-endian byte order.
    bits: Vec<u8>,
    /// Number of validators this bitset is sized for.
    num_validators: u32,
}

impl ValidatorBitset {
    /// Create a new empty bitset for `n` validators.
    pub fn new(num_validators: u32) -> Self {
        let byte_len = ((num_validators as usize) + 7) / 8;
        Self {
            bits: vec![0u8; byte_len],
            num_validators,
        }
    }

    /// Set the bit for validator `index`.
    ///
    /// Returns `false` if index is out of range.
    pub fn set(&mut self, index: ValidatorIndex) -> bool {
        let i = index.0 as usize;
        if i >= self.num_validators as usize {
            return false;
        }
        self.bits[i / 8] |= 1 << (i % 8);
        true
    }

    /// Check whether the bit for validator `index` is set.
    pub fn is_set(&self, index: ValidatorIndex) -> bool {
        let i = index.0 as usize;
        if i >= self.num_validators as usize {
            return false;
        }
        (self.bits[i / 8] & (1 << (i % 8))) != 0
    }

    /// Count the number of set bits.
    pub fn count(&self) -> u32 {
        self.bits.iter().map(|b| b.count_ones()).sum()
    }

    /// The total capacity (number of validators).
    pub fn capacity(&self) -> u32 {
        self.num_validators
    }
}

/// Reputation score for a validator, in the range `[0.0, 1.0]`.
///
/// Used by Shoal++ to weight anchor election.
/// Stored as a fixed-point `u16` internally: `0` = 0.0, `10000` = 1.0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ReputationScore(u16);

impl ReputationScore {
    /// Perfect reputation (1.0).
    pub const MAX: Self = Self(10_000);
    /// Zero reputation (0.0).
    pub const ZERO: Self = Self(0);
    /// Scale factor: internal value = f32 * SCALE.
    const SCALE: f32 = 10_000.0;

    /// Create a reputation score from a float in `[0.0, 1.0]`.
    ///
    /// Values outside this range are clamped.
    pub fn from_f32(value: f32) -> Self {
        let clamped = value.clamp(0.0, 1.0);
        Self((clamped * Self::SCALE) as u16)
    }

    /// Convert to `f32` in `[0.0, 1.0]`.
    pub fn as_f32(self) -> f32 {
        self.0 as f32 / Self::SCALE
    }

    /// Raw internal value (0..=10000).
    pub fn raw(self) -> u16 {
        self.0
    }
}

impl Default for ReputationScore {
    fn default() -> Self {
        Self::MAX
    }
}

/// Static information about a validator in the current committee.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatorInfo {
    /// Position in the committee (zero-based).
    pub index: ValidatorIndex,
    /// FALCON-512 public key for consensus signatures.
    pub falcon_pub_key: FalconVerifyKey,
    /// Staked amount (determines voting power).
    pub stake: nexus_primitives::Amount,
    /// Current reputation score.
    pub reputation: ReputationScore,
    /// Whether this validator has been slashed in the current epoch.
    pub is_slashed: bool,
    /// Assigned execution shard (if applicable).
    pub shard_id: Option<ShardId>,
}

// ── Epoch Management Types ───────────────────────────────────────────────────

/// Serialisable snapshot of a [`Committee`](crate::validator::Committee)
/// for storage persistence and epoch recovery.
///
/// The pre-computed fields (`quorum`, `total_stake`) are recomputed
/// when reconstructing the live [`Committee`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistentCommittee {
    /// Epoch this committee is valid for.
    pub epoch: EpochNumber,
    /// All validators (including those slashed during the epoch).
    pub validators: Vec<ValidatorInfo>,
}

/// What triggered an epoch transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EpochTransitionTrigger {
    /// Commit count reached the configured epoch length.
    CommitThreshold,
    /// Wall-clock epoch duration exceeded.
    TimeElapsed,
    /// Operator or governance action requested the transition.
    Manual,
}

/// Record of a completed epoch transition, persisted for audit and recovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpochTransition {
    /// Epoch that just ended.
    pub from_epoch: EpochNumber,
    /// Epoch that is starting.
    pub to_epoch: EpochNumber,
    /// What caused the transition.
    pub trigger: EpochTransitionTrigger,
    /// Total commits in the ending epoch.
    pub final_commit_count: u64,
    /// Timestamp when the transition was applied.
    pub transitioned_at: TimestampMs,
}

/// Configuration parameters governing epoch lifecycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpochConfig {
    /// Maximum number of Shoal++ commits before triggering an epoch change.
    /// Set to `0` to disable commit-based transitions.
    pub epoch_length_commits: u64,
    /// Maximum wall-clock seconds before triggering an epoch change.
    /// Set to `0` to disable time-based transitions.
    pub epoch_length_seconds: u64,
    /// Minimum commits that must happen before an epoch transition is
    /// allowed (prevents premature transitions on lightly-loaded nets).
    pub min_epoch_commits: u64,
}

impl Default for EpochConfig {
    fn default() -> Self {
        Self {
            // ~10 000 commits per epoch (roughly 24 h at 5 TPS devnet).
            epoch_length_commits: 10_000,
            // 24 h wall-clock max.
            epoch_length_seconds: 86_400,
            // At least 100 commits before allowing a transition.
            min_epoch_commits: 100,
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_crypto::{FalconSigner, Signer};
    use nexus_test_utils::fixtures::crypto::make_falcon_keypair;

    // -- ValidatorBitset tests --

    #[test]
    fn bitset_new_is_empty() {
        let bs = ValidatorBitset::new(100);
        assert_eq!(bs.count(), 0);
        assert_eq!(bs.capacity(), 100);
    }

    #[test]
    fn bitset_set_and_check() {
        let mut bs = ValidatorBitset::new(10);
        assert!(bs.set(ValidatorIndex(3)));
        assert!(bs.set(ValidatorIndex(7)));
        assert!(bs.is_set(ValidatorIndex(3)));
        assert!(bs.is_set(ValidatorIndex(7)));
        assert!(!bs.is_set(ValidatorIndex(0)));
        assert_eq!(bs.count(), 2);
    }

    #[test]
    fn bitset_out_of_range_returns_false() {
        let mut bs = ValidatorBitset::new(4);
        assert!(!bs.set(ValidatorIndex(4)));
        assert!(!bs.set(ValidatorIndex(100)));
        assert!(!bs.is_set(ValidatorIndex(4)));
    }

    #[test]
    fn bitset_large_committee() {
        let n = 300u32;
        let mut bs = ValidatorBitset::new(n);
        for i in 0..n {
            assert!(bs.set(ValidatorIndex(i)));
        }
        assert_eq!(bs.count(), n);
        for i in 0..n {
            assert!(bs.is_set(ValidatorIndex(i)));
        }
    }

    // -- ReputationScore tests --

    #[test]
    fn reputation_roundtrip() {
        let r = ReputationScore::from_f32(0.75);
        let f = r.as_f32();
        assert!((f - 0.75).abs() < 0.001);
    }

    #[test]
    fn reputation_clamp() {
        assert_eq!(ReputationScore::from_f32(-1.0), ReputationScore::ZERO);
        assert_eq!(ReputationScore::from_f32(2.0), ReputationScore::MAX);
    }

    #[test]
    fn reputation_ordering() {
        let low = ReputationScore::from_f32(0.3);
        let high = ReputationScore::from_f32(0.9);
        assert!(low < high);
    }

    #[test]
    fn reputation_default_is_max() {
        assert_eq!(ReputationScore::default(), ReputationScore::MAX);
    }

    // -- BatchStatus tests --

    #[test]
    fn batch_status_pending() {
        let s = BatchStatus::Pending;
        assert_eq!(s, BatchStatus::Pending);
    }

    #[test]
    fn batch_status_certified() {
        let s = BatchStatus::Certified {
            cert_digest: nexus_primitives::Blake3Digest::ZERO,
        };
        if let BatchStatus::Certified { cert_digest } = &s {
            assert_eq!(*cert_digest, nexus_primitives::Blake3Digest::ZERO);
        } else {
            panic!("expected Certified");
        }
    }

    #[test]
    fn batch_status_ordered() {
        let status = BatchStatus::Ordered {
            sequence: CommitSequence(9),
        };
        match status {
            BatchStatus::Ordered { sequence } => assert_eq!(sequence, CommitSequence(9)),
            other => panic!("expected Ordered, got {other:?}"),
        }
    }

    #[test]
    fn batch_status_executed() {
        let status = BatchStatus::Executed {
            state_root: nexus_primitives::Blake3Digest([0xAA; 32]),
        };
        match status {
            BatchStatus::Executed { state_root } => {
                assert_eq!(state_root, nexus_primitives::Blake3Digest([0xAA; 32]));
            }
            other => panic!("expected Executed, got {other:?}"),
        }
    }

    // -- Serialization roundtrip tests --

    #[test]
    fn bitset_serde_roundtrip() {
        let mut bs = ValidatorBitset::new(50);
        bs.set(ValidatorIndex(0));
        bs.set(ValidatorIndex(49));
        let json = serde_json::to_string(&bs).expect("serialize");
        let bs2: ValidatorBitset = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(bs, bs2);
    }

    #[test]
    fn reputation_serde_roundtrip() {
        let r = ReputationScore::from_f32(0.42);
        let json = serde_json::to_string(&r).expect("serialize");
        let r2: ReputationScore = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(r, r2);
    }

    #[test]
    fn committed_batch_serde_roundtrip() {
        let cb = CommittedBatch {
            anchor: nexus_primitives::Blake3Digest::ZERO,
            certificates: vec![nexus_primitives::Blake3Digest::ZERO],
            sequence: CommitSequence(42),
            committed_at: TimestampMs(1_000_000),
        };
        let json = serde_json::to_string(&cb).expect("serialize");
        let cb2: CommittedBatch = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(cb.sequence, cb2.sequence);
        assert_eq!(cb.certificates.len(), cb2.certificates.len());
    }

    #[test]
    fn shoal_vote_and_anchor_serde_roundtrip() {
        let (sk, vk) = make_falcon_keypair();
        let vote = ShoalVote {
            voter: ValidatorIndex(1),
            round: RoundNumber(2),
            anchor_cert: nexus_primitives::Blake3Digest([0x11; 32]),
            signature: FalconSigner::sign(&sk, VOTE_DOMAIN, b"vote-payload"),
        };
        let anchor = ShoalAnchor {
            cert_digest: nexus_primitives::Blake3Digest([0x22; 32]),
            round: RoundNumber(4),
            leader: ValidatorIndex(0),
            reputation_score: ReputationScore::from_f32(0.9),
        };

        let anchor_json = serde_json::to_string(&anchor).expect("serialize anchor");
        let anchor2: ShoalAnchor = serde_json::from_str(&anchor_json).expect("deserialize anchor");
        assert_eq!(anchor.cert_digest, anchor2.cert_digest);
        assert_eq!(anchor.reputation_score, anchor2.reputation_score);

        let info = ValidatorInfo {
            index: ValidatorIndex(3),
            falcon_pub_key: vk,
            stake: nexus_primitives::Amount(50),
            reputation: ReputationScore::from_f32(0.7),
            is_slashed: false,
            shard_id: Some(ShardId(1)),
        };
        let committee = PersistentCommittee {
            epoch: EpochNumber(8),
            validators: vec![info.clone()],
        };
        let transition = EpochTransition {
            from_epoch: EpochNumber(8),
            to_epoch: EpochNumber(9),
            trigger: EpochTransitionTrigger::Manual,
            final_commit_count: 12,
            transitioned_at: TimestampMs(999),
        };

        let info_json = serde_json::to_string(&info).expect("serialize validator info");
        let info2: ValidatorInfo =
            serde_json::from_str(&info_json).expect("deserialize validator info");
        assert_eq!(info.index, info2.index);
        assert_eq!(info.stake, info2.stake);
        assert_eq!(info.shard_id, info2.shard_id);

        let committee_json = serde_json::to_string(&committee).expect("serialize committee");
        let committee2: PersistentCommittee =
            serde_json::from_str(&committee_json).expect("deserialize committee");
        assert_eq!(committee.epoch, committee2.epoch);
        assert_eq!(committee.validators.len(), committee2.validators.len());

        let transition_json = serde_json::to_string(&transition).expect("serialize transition");
        let transition2: EpochTransition =
            serde_json::from_str(&transition_json).expect("deserialize transition");
        assert_eq!(transition.from_epoch, transition2.from_epoch);
        assert_eq!(transition.trigger, transition2.trigger);
        assert_eq!(
            transition.final_commit_count,
            transition2.final_commit_count
        );

        let vote_json = serde_json::to_string(&vote).expect("serialize vote");
        let vote2: ShoalVote = serde_json::from_str(&vote_json).expect("deserialize vote");
        assert_eq!(vote.voter, vote2.voter);
        assert_eq!(vote.round, vote2.round);
        assert_eq!(vote.anchor_cert, vote2.anchor_cert);
    }

    #[test]
    fn reputation_raw_returns_inner_u16() {
        let r = ReputationScore::from_f32(0.5);
        // raw() should return the scaled u16 representation.
        let raw = r.raw();
        // round-trip: as_f32(raw) ~ 0.5
        let reconstructed = ReputationScore(raw).as_f32();
        assert!(
            (reconstructed - 0.5).abs() < 0.01,
            "raw round-trip: {reconstructed}"
        );
    }

    #[test]
    fn epoch_config_default_values() {
        let cfg = EpochConfig::default();
        assert_eq!(cfg.epoch_length_commits, 10_000);
        assert_eq!(cfg.epoch_length_seconds, 86_400);
        assert_eq!(cfg.min_epoch_commits, 100);
    }

    #[test]
    fn epoch_transition_trigger_all_variants_serde() {
        let triggers = [
            EpochTransitionTrigger::CommitThreshold,
            EpochTransitionTrigger::TimeElapsed,
            EpochTransitionTrigger::Manual,
        ];
        for t in &triggers {
            let json = serde_json::to_string(t).expect("serialize trigger");
            let t2: EpochTransitionTrigger =
                serde_json::from_str(&json).expect("deserialize trigger");
            assert_eq!(*t, t2);
        }
    }

    #[test]
    fn bitset_double_set_is_idempotent() {
        let mut bs = ValidatorBitset::new(8);
        // set() always returns true for in-range indices.
        assert!(bs.set(ValidatorIndex(2)));
        // Setting again still returns true (OR-assign keeps bit set).
        assert!(bs.set(ValidatorIndex(2)));
        // Count should still be 1.
        assert_eq!(bs.count(), 1);
    }

    #[test]
    fn batch_status_variants_debug() {
        // Drive Debug impls to improve region coverage.
        let s = format!("{:?}", BatchStatus::Pending);
        assert!(s.contains("Pending"));
        let s = format!(
            "{:?}",
            BatchStatus::Certified {
                cert_digest: nexus_primitives::Blake3Digest::ZERO
            }
        );
        assert!(s.contains("Certified"));
    }
}
