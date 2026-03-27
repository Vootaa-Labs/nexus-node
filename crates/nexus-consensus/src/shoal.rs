// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Shoal++ BFT total-order finalization.
//!
//! [`ShoalOrderer`] implements [`BftOrderer`], selecting anchors every 2 rounds
//! and committing sub-DAGs when the leader's certificate is present.
//!
//! # Algorithm overview
//!
//! 1. **Anchor round**: even DAG rounds (0, 2, 4, …).
//! 2. **Leader selection**: validator with highest reputation at each anchor round.
//! 3. **Commit rule**: if the leader has a certificate at the anchor round and
//!    the DAG has advanced at least one round beyond, commit the leader's
//!    causal history as the next sub-DAG.
//! 4. **Reputation update**: exponential moving average based on certificate
//!    propagation latency.

use std::collections::HashMap;

use crate::error::ConsensusError;
use crate::traits::{BftOrderer, CertificateDag};
use crate::types::{CommittedBatch, ReputationScore, ShoalAnchor};
use nexus_primitives::{CommitSequence, RoundNumber, TimestampMs, ValidatorIndex};

/// Default reputation decay factor (λ in EMA).
const DEFAULT_REPUTATION_DECAY: f32 = 0.99;

/// Target latency (ms) used to normalize the quality signal.
/// Latencies ≤ this value yield quality = 1.0.
const TARGET_LATENCY_MS: f32 = 200.0;

/// Maximum latency (ms) beyond which quality = 0.0.
const MAX_LATENCY_MS: f32 = 5000.0;

/// Maximum number of DAG rounds ahead of a missing anchor before giving up
/// on recovery and permanently skipping (C-4 / SEC-H10).
const MAX_ANCHOR_RECOVERY_WINDOW: u64 = 6;

/// Shoal++ BFT orderer.
///
/// Maintains reputation scores and commit state. Each call to
/// [`try_commit`](BftOrderer::try_commit) checks whether the next anchor
/// round's leader certificate is available and commits if so.
#[derive(Debug, Clone)]
pub struct ShoalOrderer {
    /// Reputation scores per validator.
    reputations: HashMap<ValidatorIndex, ReputationScore>,
    /// All validator indices in the committee.
    validators: Vec<ValidatorIndex>,
    /// Next anchor round to evaluate.
    next_anchor_round: RoundNumber,
    /// The last anchor round that was committed (None if nothing committed yet).
    last_committed_round: Option<RoundNumber>,
    /// Next monotonic sequence number.
    next_sequence: u64,
    /// Reputation decay factor for EMA.
    reputation_decay: f32,
    /// History of committed anchors (for introspection / debugging).
    committed_anchors: Vec<ShoalAnchor>,
}

impl ShoalOrderer {
    /// Create a new orderer for the given validator set.
    ///
    /// All validators start with `ReputationScore::MAX` (perfect reputation).
    pub fn new(validators: Vec<ValidatorIndex>) -> Self {
        let reputations = validators
            .iter()
            .map(|&v| (v, ReputationScore::MAX))
            .collect();
        Self {
            reputations,
            validators,
            next_anchor_round: RoundNumber(0),
            last_committed_round: None,
            next_sequence: 0,
            reputation_decay: DEFAULT_REPUTATION_DECAY,
            committed_anchors: Vec::new(),
        }
    }

    /// The last committed anchor round, if any.
    pub fn last_committed_round(&self) -> Option<RoundNumber> {
        self.last_committed_round
    }

    /// Number of sub-DAGs committed so far.
    pub fn commit_count(&self) -> u64 {
        self.next_sequence
    }

    /// History of committed anchors.
    pub fn committed_anchors(&self) -> &[ShoalAnchor] {
        &self.committed_anchors
    }

    /// Determine the next anchor round to check.
    fn next_anchor_round(&self) -> RoundNumber {
        self.next_anchor_round
    }

    fn advance_anchor_round(&mut self) {
        self.next_anchor_round = RoundNumber(self.next_anchor_round.0.saturating_add(2));
    }

    /// Select the leader for a specific anchor round.
    ///
    /// Validators are ranked by reputation (descending). Among the top tier
    /// (within 10 % of the best score), the leader rotates based on anchor
    /// round to prevent long-term monopoly (C-5 / SEC-H10).
    fn select_leader(&self, anchor_round: RoundNumber) -> Option<ValidatorIndex> {
        if self.validators.is_empty() {
            return None;
        }

        // Sort by reputation descending, ties by index ascending.
        let mut candidates: Vec<_> = self
            .validators
            .iter()
            .map(|&v| {
                let rep = self
                    .reputations
                    .get(&v)
                    .copied()
                    .unwrap_or(ReputationScore::ZERO);
                (v, rep)
            })
            .collect();
        candidates.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0 .0.cmp(&b.0 .0)));

        // Determine the top reputation tier (within 10 % of the best).
        let best = candidates[0].1.as_f32();
        let tier_threshold = (best * 0.9).max(0.0);
        let top_tier_count = candidates
            .iter()
            .take_while(|(_, rep)| rep.as_f32() >= tier_threshold)
            .count()
            .max(1);

        // Rotate among the top tier based on anchor round.
        let rotation = (anchor_round.0 / 2) as usize % top_tier_count;
        Some(candidates[rotation].0)
    }

    /// Convert latency to a quality score in [0.0, 1.0].
    fn latency_to_quality(latency_ms: u32) -> f32 {
        if latency_ms as f32 <= TARGET_LATENCY_MS {
            1.0
        } else if latency_ms as f32 >= MAX_LATENCY_MS {
            0.0
        } else {
            1.0 - (latency_ms as f32 - TARGET_LATENCY_MS) / (MAX_LATENCY_MS - TARGET_LATENCY_MS)
        }
    }
}

impl BftOrderer for ShoalOrderer {
    fn try_commit(
        &mut self,
        dag: &dyn CertificateDag,
    ) -> Result<Option<CommittedBatch>, ConsensusError> {
        loop {
            let anchor_round = self.next_anchor_round();
            let dag_round = dag.current_round();

            // DAG must have advanced past the anchor round for commit safety.
            if dag_round.0 <= anchor_round.0 {
                return Ok(None);
            }

            // Find the leader for this anchor round.
            let leader = match self.select_leader(anchor_round) {
                Some(v) => v,
                None => return Ok(None),
            };

            // Check if the leader has a certificate at the anchor round.
            let leader_cert = match dag.get_certificate(leader, anchor_round) {
                Some(cert) => cert,
                None => {
                    // C-4: give the leader a grace period. Only permanently skip
                    // once the DAG has advanced beyond the recovery window.
                    let gap = dag_round.0.saturating_sub(anchor_round.0);
                    if gap > MAX_ANCHOR_RECOVERY_WINDOW {
                        self.advance_anchor_round();
                        continue; // Try the next anchor round instead of stalling.
                    }
                    return Ok(None);
                }
            };

            // Compute the causal history (topological order).
            let anchor_digest = leader_cert.cert_digest;
            let certificates = dag.causal_history(&anchor_digest);

            if certificates.is_empty() {
                return Ok(None);
            }

            // Produce the committed sub-DAG.
            let reputation = self
                .reputations
                .get(&leader)
                .copied()
                .unwrap_or(ReputationScore::ZERO);

            let anchor = ShoalAnchor {
                cert_digest: anchor_digest,
                round: anchor_round,
                leader,
                reputation_score: reputation,
            };
            self.committed_anchors.push(anchor);

            let sequence = CommitSequence(self.next_sequence);
            self.next_sequence += 1;
            self.last_committed_round = Some(anchor_round);
            self.advance_anchor_round();

            return Ok(Some(CommittedBatch {
                anchor: anchor_digest,
                certificates,
                sequence,
                committed_at: TimestampMs(0), // Caller sets real timestamp.
            }));
        }
    }

    fn update_reputation(&mut self, validator: ValidatorIndex, latency_ms: u32) {
        let quality = Self::latency_to_quality(latency_ms);
        let decay = self.reputation_decay;

        let entry = self
            .reputations
            .entry(validator)
            .or_insert(ReputationScore::MAX);

        // EMA: new = old * decay + (1 - decay) * quality
        let old = entry.as_f32();
        let updated = old * decay + (1.0 - decay) * quality;
        *entry = ReputationScore::from_f32(updated);
    }

    fn current_anchor_candidates(&self) -> Vec<ValidatorIndex> {
        let mut candidates: Vec<_> = self
            .validators
            .iter()
            .map(|&v| {
                let rep = self
                    .reputations
                    .get(&v)
                    .copied()
                    .unwrap_or(ReputationScore::ZERO);
                (v, rep)
            })
            .collect();

        // Sort by reputation descending, ties by lower index first.
        candidates.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0 .0.cmp(&b.0 .0)));
        candidates.into_iter().map(|(v, _)| v).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dag::InMemoryDag;
    use crate::traits::CertificateDag;
    use crate::types::{NarwhalCertificate, ValidatorBitset};
    use nexus_primitives::{Blake3Digest, EpochNumber};

    fn make_cert(
        origin: u32,
        round: u64,
        parents: Vec<nexus_primitives::CertDigest>,
        digest_seed: u8,
    ) -> NarwhalCertificate {
        NarwhalCertificate {
            epoch: EpochNumber(1),
            batch_digest: Blake3Digest([digest_seed; 32]),
            origin: ValidatorIndex(origin),
            round: RoundNumber(round),
            parents,
            signatures: vec![],
            signers: ValidatorBitset::new(4),
            cert_digest: Blake3Digest([digest_seed; 32]),
        }
    }

    fn validators(n: u32) -> Vec<ValidatorIndex> {
        (0..n).map(ValidatorIndex).collect()
    }

    // ── Anchor round calculation ────────────────────────────────────────

    #[test]
    fn next_anchor_starts_at_zero() {
        let orderer = ShoalOrderer::new(validators(4));
        assert_eq!(orderer.next_anchor_round(), RoundNumber(0));
    }

    // ── Leader selection ────────────────────────────────────────────────

    #[test]
    fn leader_is_highest_reputation() {
        let mut orderer = ShoalOrderer::new(validators(4));
        // Lower reputation for validator 0.
        orderer.update_reputation(ValidatorIndex(0), 4000);
        let candidates = orderer.current_anchor_candidates();
        // Validator 0 should be last (lowest reputation).
        assert_ne!(candidates[0], ValidatorIndex(0));
        assert_eq!(*candidates.last().unwrap(), ValidatorIndex(0));
    }

    #[test]
    fn leader_tiebreak_by_lowest_index() {
        let orderer = ShoalOrderer::new(validators(4));
        // All same reputation → lowest index wins.
        let candidates = orderer.current_anchor_candidates();
        assert_eq!(candidates[0], ValidatorIndex(0));
    }

    // ── Reputation update ───────────────────────────────────────────────

    #[test]
    fn low_latency_increases_reputation() {
        let mut orderer = ShoalOrderer::new(validators(4));
        // Start at MAX, update with very low latency.
        orderer.update_reputation(ValidatorIndex(0), 50);
        let rep = orderer.reputations[&ValidatorIndex(0)];
        // Should remain near MAX.
        assert!(rep.as_f32() > 0.99);
    }

    #[test]
    fn high_latency_decreases_reputation() {
        let mut orderer = ShoalOrderer::new(validators(4));
        // Multiple high-latency observations (decay 0.99, need ~70 for < 0.5).
        for _ in 0..100 {
            orderer.update_reputation(ValidatorIndex(0), 5000);
        }
        let rep = orderer.reputations[&ValidatorIndex(0)];
        assert!(rep.as_f32() < 0.5);
    }

    #[test]
    fn latency_to_quality_ranges() {
        // Below target → 1.0.
        assert_eq!(ShoalOrderer::latency_to_quality(100), 1.0);
        // At target → 1.0.
        assert_eq!(ShoalOrderer::latency_to_quality(200), 1.0);
        // Above max → 0.0.
        assert_eq!(ShoalOrderer::latency_to_quality(5000), 0.0);
        assert_eq!(ShoalOrderer::latency_to_quality(6000), 0.0);
        // Mid-range.
        let q = ShoalOrderer::latency_to_quality(2600);
        assert!(q > 0.4 && q < 0.6);
    }

    // ── try_commit ──────────────────────────────────────────────────────

    #[test]
    fn try_commit_returns_none_when_dag_not_advanced() {
        let mut orderer = ShoalOrderer::new(validators(4));
        let dag = InMemoryDag::new();
        // Empty DAG → no commit.
        let result = orderer.try_commit(&dag).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn try_commit_returns_none_when_leader_has_no_cert() {
        let mut orderer = ShoalOrderer::new(validators(4));
        let mut dag = InMemoryDag::new();

        // Insert certs at round 0 for validators 1,2,3 (not 0 who is leader at anchor 0).
        let c1 = make_cert(1, 0, vec![], 11);
        let c2 = make_cert(2, 0, vec![], 12);
        let c3 = make_cert(3, 0, vec![], 13);
        let d1 = c1.cert_digest;
        let d2 = c2.cert_digest;
        let d3 = c3.cert_digest;
        dag.insert_certificate(c1).unwrap();
        dag.insert_certificate(c2).unwrap();
        dag.insert_certificate(c3).unwrap();

        // Insert a round 1 cert referencing round 0 certs so DAG advances.
        let c4 = make_cert(1, 1, vec![d1, d2, d3], 21);
        dag.insert_certificate(c4).unwrap();

        let result = orderer.try_commit(&dag).unwrap();
        assert!(result.is_none());
        // C-4: within recovery window (gap = 1 ≤ 6), anchor does NOT advance.
        assert_eq!(orderer.next_anchor_round(), RoundNumber(0));
    }

    #[test]
    fn try_commit_skips_missing_anchor_after_recovery_window() {
        // C-4: after MAX_ANCHOR_RECOVERY_WINDOW rounds, permanently skip.
        // With the loop fix, try_commit now continues to the next anchor
        // after skipping, and commits if a valid leader is available.
        let mut orderer = ShoalOrderer::new(validators(4));
        let mut dag = InMemoryDag::new();

        // Build a DAG without leader (V0) cert at round 0,
        // but extending to round 8 (gap = 8 > MAX_ANCHOR_RECOVERY_WINDOW = 6).
        let c1 = make_cert(1, 0, vec![], 11);
        let c2 = make_cert(2, 0, vec![], 12);
        let d1 = c1.cert_digest;
        let d2 = c2.cert_digest;
        dag.insert_certificate(c1).unwrap();
        dag.insert_certificate(c2).unwrap();

        // Chain rounds 1..=8 to advance DAG.
        let mut prev = vec![d1, d2];
        for r in 1..=8u64 {
            let c = make_cert(1, r, prev.clone(), 20 + r as u8);
            prev = vec![c.cert_digest];
            dag.insert_certificate(c).unwrap();
        }
        assert_eq!(dag.current_round(), RoundNumber(8));

        // Anchor 0 → V0 missing → gap > 6 → skip to anchor 2.
        // Anchor 2 → leader V1 → V1 cert at round 2 exists → COMMIT.
        let result = orderer.try_commit(&dag).unwrap();
        assert!(result.is_some(), "loop should skip stale anchor and commit next");
        assert_eq!(result.unwrap().sequence, CommitSequence(0));
        assert_eq!(orderer.next_anchor_round(), RoundNumber(4));
    }

    #[test]
    fn try_commit_skips_missing_anchor_and_commits_next_available_one() {
        // With the loop fix, a single try_commit call skips the missing
        // anchor AND commits at the next one in one operation.
        let mut orderer = ShoalOrderer::new(validators(4));
        let mut dag = InMemoryDag::new();

        // Validator 0 (leader at anchor 0) missing at round 0.
        // Build DAG far enough ahead to trigger skip, then commit at anchor 2.
        let g1 = make_cert(1, 0, vec![], 11);
        let g2 = make_cert(2, 0, vec![], 12);
        let g3 = make_cert(3, 0, vec![], 13);
        let d1 = g1.cert_digest;
        let d2 = g2.cert_digest;
        let d3 = g3.cert_digest;
        dag.insert_certificate(g1).unwrap();
        dag.insert_certificate(g2).unwrap();
        dag.insert_certificate(g3).unwrap();

        // Build rounds 1..=8 with all needed certs.
        let mut prev = vec![d1, d2, d3];
        for r in 1..=8u64 {
            let c = make_cert(1, r, prev.clone(), 20 + r as u8);
            prev = vec![c.cert_digest];
            dag.insert_certificate(c).unwrap();
        }

        // Single call: anchor 0 → V0 missing → skip → anchor 2 → V1 → commit.
        let result = orderer.try_commit(&dag).unwrap();
        assert!(result.is_some(), "loop should skip and commit in one call");
        let committed = result.unwrap();
        assert_eq!(committed.sequence, CommitSequence(0));
        assert_eq!(orderer.next_anchor_round(), RoundNumber(4));
    }

    #[test]
    fn try_commit_succeeds_with_leader_cert() {
        let mut orderer = ShoalOrderer::new(validators(4));
        let mut dag = InMemoryDag::new();

        // Round 0: leader (validator 0) has a cert.
        let g0 = make_cert(0, 0, vec![], 10);
        let g1 = make_cert(1, 0, vec![], 11);
        let d0 = g0.cert_digest;
        let d1 = g1.cert_digest;
        dag.insert_certificate(g0).unwrap();
        dag.insert_certificate(g1).unwrap();

        // Round 1: advance the DAG.
        let r1 = make_cert(0, 1, vec![d0, d1], 20);
        dag.insert_certificate(r1).unwrap();

        let result = orderer.try_commit(&dag).unwrap();
        assert!(result.is_some());

        let batch = result.unwrap();
        assert_eq!(batch.sequence, CommitSequence(0));
        assert_eq!(batch.anchor, Blake3Digest([10; 32]));
        assert!(!batch.certificates.is_empty());
    }

    #[test]
    fn try_commit_advances_sequence() {
        let mut orderer = ShoalOrderer::new(validators(4));
        let mut dag = InMemoryDag::new();

        // Round 0 (anchor 0): leader = V0 (rotation = 0/2 = 0).
        let g0 = make_cert(0, 0, vec![], 10);
        let g1 = make_cert(1, 0, vec![], 11);
        let d0 = g0.cert_digest;
        let d1 = g1.cert_digest;
        dag.insert_certificate(g0).unwrap();
        dag.insert_certificate(g1).unwrap();

        // Round 1.
        let r1a = make_cert(0, 1, vec![d0, d1], 20);
        let r1b = make_cert(1, 1, vec![d0, d1], 21);
        let d_r1a = r1a.cert_digest;
        let d_r1b = r1b.cert_digest;
        dag.insert_certificate(r1a).unwrap();
        dag.insert_certificate(r1b).unwrap();

        // First commit at anchor round 0.
        let first = orderer.try_commit(&dag).unwrap().unwrap();
        assert_eq!(first.sequence, CommitSequence(0));

        // Round 2 (anchor 2): leader = V1 (rotation = 2/2 = 1).
        // Insert V1 cert at round 2.
        let r2 = make_cert(1, 2, vec![d_r1a, d_r1b], 30);
        let d_r2 = r2.cert_digest;
        dag.insert_certificate(r2).unwrap();

        // Round 3: advance past anchor round 2.
        let r3 = make_cert(0, 3, vec![d_r2], 40);
        dag.insert_certificate(r3).unwrap();

        // Second commit at anchor round 2.
        let second = orderer.try_commit(&dag).unwrap().unwrap();
        assert_eq!(second.sequence, CommitSequence(1));
        assert_eq!(orderer.commit_count(), 2);
    }

    #[test]
    fn try_commit_no_double_commit_same_round() {
        let mut orderer = ShoalOrderer::new(validators(4));
        let mut dag = InMemoryDag::new();

        let g0 = make_cert(0, 0, vec![], 10);
        let g1 = make_cert(1, 0, vec![], 11);
        let d0 = g0.cert_digest;
        let d1 = g1.cert_digest;
        dag.insert_certificate(g0).unwrap();
        dag.insert_certificate(g1).unwrap();

        let r1 = make_cert(0, 1, vec![d0, d1], 20);
        dag.insert_certificate(r1).unwrap();

        // First try_commit succeeds.
        assert!(orderer.try_commit(&dag).unwrap().is_some());
        // Second try_commit for same DAG state returns None
        // (DAG hasn't advanced past the next anchor round).
        assert!(orderer.try_commit(&dag).unwrap().is_none());
    }

    // ── Edge cases ──────────────────────────────────────────────────────

    #[test]
    fn empty_committee_returns_none() {
        let mut orderer = ShoalOrderer::new(vec![]);
        let dag = InMemoryDag::new();
        assert!(orderer.try_commit(&dag).unwrap().is_none());
    }

    #[test]
    fn committed_anchors_history() {
        let mut orderer = ShoalOrderer::new(validators(4));
        let mut dag = InMemoryDag::new();

        let g0 = make_cert(0, 0, vec![], 10);
        let g1 = make_cert(1, 0, vec![], 11);
        let d0 = g0.cert_digest;
        let d1 = g1.cert_digest;
        dag.insert_certificate(g0).unwrap();
        dag.insert_certificate(g1).unwrap();
        let r1 = make_cert(0, 1, vec![d0, d1], 20);
        dag.insert_certificate(r1).unwrap();

        orderer.try_commit(&dag).unwrap();
        assert_eq!(orderer.committed_anchors().len(), 1);
        assert_eq!(orderer.committed_anchors()[0].leader, ValidatorIndex(0));
        assert_eq!(orderer.committed_anchors()[0].round, RoundNumber(0));
    }

    // ── SEC-M-001: saturating anchor round at u64::MAX ──────────────────

    #[test]
    fn next_anchor_round_saturates_at_max() {
        let mut orderer = ShoalOrderer::new(validators(4));
        orderer.next_anchor_round = RoundNumber(u64::MAX - 1);
        // Should saturate to u64::MAX, not panic or wrap around.
        orderer.advance_anchor_round();
        assert_eq!(orderer.next_anchor_round(), RoundNumber(u64::MAX));
    }

    // ── Phase C acceptance tests ────────────────────────────────────────

    #[test]
    fn shoal_should_not_permanently_skip_late_anchor_without_recovery() {
        // C-4 / SEC-H10: a late-arriving leader cert within the recovery
        // window should still get committed.
        let mut orderer = ShoalOrderer::new(validators(4));
        let mut dag = InMemoryDag::new();

        // Round 0: leader V0 has no cert yet. V1, V2 have certs.
        let g1 = make_cert(1, 0, vec![], 11);
        let g2 = make_cert(2, 0, vec![], 12);
        let d1 = g1.cert_digest;
        let d2 = g2.cert_digest;
        dag.insert_certificate(g1).unwrap();
        dag.insert_certificate(g2).unwrap();

        // Round 1: DAG advances 1 past anchor 0.
        let r1 = make_cert(1, 1, vec![d1, d2], 20);
        let d_r1 = r1.cert_digest;
        dag.insert_certificate(r1).unwrap();

        // try_commit fails (leader V0 missing), but anchor stays at 0 (within window).
        assert!(orderer.try_commit(&dag).unwrap().is_none());
        assert_eq!(orderer.next_anchor_round(), RoundNumber(0));

        // Simulate late arrival: V0's cert at round 0 now inserted.
        let g0 = make_cert(0, 0, vec![], 10);
        dag.insert_certificate(g0).unwrap();

        // Round 2: advance DAG further to keep it ahead.
        let r2 = make_cert(1, 2, vec![d_r1], 30);
        dag.insert_certificate(r2).unwrap();

        // Now try_commit should succeed: V0 cert at anchor 0 exists.
        let batch = orderer.try_commit(&dag).unwrap();
        assert!(
            batch.is_some(),
            "late-arriving leader cert should still be committed within recovery window"
        );
        assert_eq!(batch.unwrap().sequence, CommitSequence(0));
    }

    #[test]
    fn leader_rotates_across_anchor_rounds() {
        // C-5 / SEC-H10: with equal reputations, different anchor rounds
        // should select different leaders (round-based rotation).
        let orderer = ShoalOrderer::new(validators(4));

        // Anchor round 0: rotation = 0/2 = 0 → V0
        let l0 = orderer.select_leader(RoundNumber(0)).unwrap();
        assert_eq!(l0, ValidatorIndex(0));

        // Anchor round 2: rotation = 2/2 = 1 → V1
        let l2 = orderer.select_leader(RoundNumber(2)).unwrap();
        assert_eq!(l2, ValidatorIndex(1));

        // Anchor round 4: rotation = 4/2 = 2 → V2
        let l4 = orderer.select_leader(RoundNumber(4)).unwrap();
        assert_eq!(l4, ValidatorIndex(2));

        // Anchor round 6: rotation = 6/2 = 3 → V3
        let l6 = orderer.select_leader(RoundNumber(6)).unwrap();
        assert_eq!(l6, ValidatorIndex(3));

        // Anchor round 8: wraps back → V0
        let l8 = orderer.select_leader(RoundNumber(8)).unwrap();
        assert_eq!(l8, ValidatorIndex(0));
    }

    #[test]
    fn leader_with_much_higher_reputation_dominates() {
        // C-5: if one validator's reputation is significantly higher (> 10% gap),
        // it stays as leader despite rotation.
        let mut orderer = ShoalOrderer::new(validators(4));

        // Severely penalize validators 1, 2, 3.
        for _ in 0..200 {
            orderer.update_reputation(ValidatorIndex(1), 5000);
            orderer.update_reputation(ValidatorIndex(2), 5000);
            orderer.update_reputation(ValidatorIndex(3), 5000);
        }

        // V0 still at MAX. Others < 0.5. Top tier is only V0.
        // All anchor rounds should pick V0.
        for anchor_round in (0..=8).step_by(2) {
            let leader = orderer.select_leader(RoundNumber(anchor_round)).unwrap();
            assert_eq!(
                leader,
                ValidatorIndex(0),
                "validator with clearly highest reputation should be leader at round {anchor_round}"
            );
        }
    }
}
