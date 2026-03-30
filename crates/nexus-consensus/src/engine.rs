// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Consensus engine — async actor that orchestrates the consensus pipeline.
//!
//! [`ConsensusEngine`] ties together:
//! - [`InMemoryDag`](crate::dag::InMemoryDag) — certificate storage
//! - [`ShoalOrderer`](crate::shoal::ShoalOrderer) — BFT ordering
//! - [`Committee`](crate::validator::Committee) — PoS committee
//! - [`CertificateVerifier`](crate::certificate::CertificateVerifier) — signature verification
//!
//! The engine exposes a simple interface:
//! - `process_certificate` — verify + insert + attempt commit
//! - `take_committed` — drain committed sub-DAGs for execution

use crate::certificate::CertificateVerifier;
use crate::dag::InMemoryDag;
use crate::error::ConsensusResult;
use crate::persist::DagPersistSync;
use crate::shoal::ShoalOrderer;
use crate::traits::{BftOrderer, CertificateDag, ValidatorRegistry};
use crate::types::{CommittedBatch, NarwhalCertificate};
use crate::validator::Committee;
use nexus_primitives::{EpochNumber, TimestampMs};

/// Maximum number of committed sub-DAGs buffered before the execution
/// layer drains them (SEC-M7).  If the buffer is full the engine refuses
/// to commit more, applying back-pressure to the consensus pipeline.
const MAX_COMMITTED_BUFFER: usize = 1024;

/// Orchestrates the full consensus pipeline for a single epoch.
///
/// Processes incoming certificates, verifies them against the committee,
/// inserts them into the DAG, and attempts BFT ordering after each insertion.
/// Committed sub-DAGs accumulate in an internal buffer for the execution
/// layer to drain.
pub struct ConsensusEngine {
    /// Current epoch.
    epoch: EpochNumber,
    /// PoS committee for this epoch.
    committee: Committee,
    /// Narwhal DAG.
    dag: InMemoryDag,
    /// Shoal++ BFT orderer.
    orderer: ShoalOrderer,
    /// Buffer of committed sub-DAGs awaiting execution.
    committed: Vec<CommittedBatch>,
    /// Optional disk persistence for every inserted certificate.
    persist: Option<Box<dyn DagPersistSync>>,
    /// Number of past epochs whose certificates are retained on disk.
    /// `0` means delete current epoch certs on advance (legacy behavior).
    epoch_retention_count: u64,
}

impl std::fmt::Debug for ConsensusEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConsensusEngine")
            .field("epoch", &self.epoch)
            .field("dag_size", &self.dag.len())
            .field("committed", &self.committed.len())
            .field(
                "persist",
                &if self.persist.is_some() {
                    "Some(..)"
                } else {
                    "None"
                },
            )
            .finish()
    }
}

impl ConsensusEngine {
    /// Create a new consensus engine for the given epoch and committee.
    pub fn new(epoch: EpochNumber, committee: Committee) -> Self {
        let validators: Vec<_> = committee
            .active_validators()
            .iter()
            .map(|v| v.index)
            .collect();
        let orderer = ShoalOrderer::new(validators);

        Self {
            epoch,
            committee,
            dag: InMemoryDag::new(),
            orderer,
            committed: Vec::new(),
            persist: None,
            epoch_retention_count: 0,
        }
    }

    /// Create a new consensus engine with disk persistence enabled.
    ///
    /// Every certificate inserted via `process_certificate` or
    /// `insert_verified_certificate` will be written through to
    /// `cf_certificates`.
    pub fn new_with_persistence(
        epoch: EpochNumber,
        committee: Committee,
        persist: Box<dyn DagPersistSync>,
    ) -> Self {
        Self::new_with_persistence_and_retention(epoch, committee, persist, 0)
    }

    /// Create a new consensus engine with disk persistence and epoch retention.
    ///
    /// `epoch_retention_count` controls how many past epochs' certificates
    /// are kept on disk after an epoch advance.  `0` means the current
    /// epoch's certificates are deleted immediately on advance.
    pub fn new_with_persistence_and_retention(
        epoch: EpochNumber,
        committee: Committee,
        persist: Box<dyn DagPersistSync>,
        epoch_retention_count: u64,
    ) -> Self {
        let validators: Vec<_> = committee
            .active_validators()
            .iter()
            .map(|v| v.index)
            .collect();
        let orderer = ShoalOrderer::new(validators);

        Self {
            epoch,
            committee,
            dag: InMemoryDag::new(),
            orderer,
            committed: Vec::new(),
            persist: Some(persist),
            epoch_retention_count,
        }
    }

    /// Process an incoming certificate through the full pipeline.
    ///
    /// 1. Verify the certificate's signatures against the committee.
    /// 2. Insert into the DAG (with causality checks).
    /// 3. Attempt BFT commit.
    ///
    /// Returns `Ok(true)` if a new sub-DAG was committed, `Ok(false)` otherwise.
    ///
    /// # Errors
    ///
    /// Returns the first error encountered (verification, insertion, or ordering).
    pub fn process_certificate(&mut self, cert: NarwhalCertificate) -> ConsensusResult<bool> {
        // Back-pressure: refuse if execution has not drained committed buffer.
        if self.committed.len() >= MAX_COMMITTED_BUFFER {
            return Err(crate::error::ConsensusError::CommittedBufferFull {
                len: self.committed.len(),
                max: MAX_COMMITTED_BUFFER,
            });
        }

        // 1. Verify signatures.
        CertificateVerifier::verify(&cert, &self.committee, self.epoch)?;

        // 2. Insert into DAG.
        self.dag.insert_certificate(cert.clone())?;

        // 2b. Write-through to disk (if persistence enabled).
        if let Some(ref p) = self.persist {
            if let Err(e) = p.persist_sync(&cert) {
                tracing::error!("DAG persist failed for cert {}: {e}", cert.cert_digest);
            }
        }

        // 3. Attempt commit.
        match self.orderer.try_commit(&self.dag)? {
            Some(mut batch) => {
                batch.committed_at = TimestampMs::now();
                self.committed.push(batch);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Insert a pre-verified certificate (skips signature verification).
    ///
    /// Use this **only** for genesis certificates or locally constructed certs
    /// that have already been verified through a trusted internal path.
    ///
    /// Although signature verification is skipped, the method still enforces:
    /// - Epoch must match the engine's current epoch.
    /// - The `cert_digest` must match the recomputed header digest.
    ///
    /// Returns `Ok(true)` if a new sub-DAG was committed.
    pub fn insert_verified_certificate(
        &mut self,
        cert: NarwhalCertificate,
    ) -> ConsensusResult<bool> {
        // Back-pressure: refuse if execution has not drained committed buffer.
        if self.committed.len() >= MAX_COMMITTED_BUFFER {
            return Err(crate::error::ConsensusError::CommittedBufferFull {
                len: self.committed.len(),
                max: MAX_COMMITTED_BUFFER,
            });
        }

        // Epoch check: prevent stale or future-epoch certs entering the DAG.
        if cert.epoch != self.epoch {
            return Err(crate::error::ConsensusError::EpochMismatch {
                expected: self.epoch,
                got: cert.epoch,
            });
        }

        // Digest integrity: recompute and compare.
        let expected_digest = crate::certificate::compute_cert_digest(
            cert.epoch,
            &cert.batch_digest,
            cert.origin,
            cert.round,
            &cert.parents,
        )?;
        if expected_digest != cert.cert_digest {
            return Err(crate::error::ConsensusError::Codec(
                "certificate digest mismatch in insert_verified_certificate".to_string(),
            ));
        }

        self.dag.insert_certificate(cert.clone())?;

        // Write-through to disk (if persistence enabled).
        if let Some(ref p) = self.persist {
            if let Err(e) = p.persist_sync(&cert) {
                tracing::error!("DAG persist failed for cert {}: {e}", cert.cert_digest);
            }
        }

        match self.orderer.try_commit(&self.dag)? {
            Some(mut batch) => {
                batch.committed_at = TimestampMs::now();
                self.committed.push(batch);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Drain all committed sub-DAGs from the internal buffer.
    ///
    /// After calling this, the buffer is empty. The caller (execution layer)
    /// is responsible for processing the returned batches in order.
    pub fn take_committed(&mut self) -> Vec<CommittedBatch> {
        std::mem::take(&mut self.committed)
    }

    /// Number of committed sub-DAGs pending execution.
    pub fn pending_commits(&self) -> usize {
        self.committed.len()
    }

    /// The current epoch.
    pub fn epoch(&self) -> EpochNumber {
        self.epoch
    }

    /// Read-only access to the DAG.
    pub fn dag(&self) -> &InMemoryDag {
        &self.dag
    }

    /// Read-only access to the committee.
    pub fn committee(&self) -> &Committee {
        &self.committee
    }

    /// Read-only access to the orderer.
    pub fn orderer(&self) -> &ShoalOrderer {
        &self.orderer
    }

    /// Number of certificates in the DAG.
    pub fn dag_size(&self) -> usize {
        self.dag.len()
    }

    /// Total number of committed sub-DAGs (lifetime of this engine).
    pub fn total_commits(&self) -> u64 {
        self.orderer.commit_count()
    }

    /// Mutable access to the committee (for slashing / reputation updates).
    pub fn committee_mut(&mut self) -> &mut Committee {
        &mut self.committee
    }

    /// Advance the engine to a new epoch with a new committee.
    ///
    /// Drains any remaining committed batches and resets the DAG
    /// and orderer for the new epoch. The caller must persist the
    /// transition metadata *before* calling this method.
    ///
    /// Returns an [`EpochTransition`](crate::types::EpochTransition)
    /// record and any un-drained committed batches from the old epoch.
    pub fn advance_epoch(
        &mut self,
        next_committee: Committee,
        trigger: crate::types::EpochTransitionTrigger,
    ) -> (crate::types::EpochTransition, Vec<CommittedBatch>) {
        let remaining = self.take_committed();
        let final_commit_count = self.orderer.commit_count();
        let old_epoch = self.epoch;
        let new_epoch = EpochNumber(old_epoch.0 + 1);

        let transition = crate::types::EpochTransition {
            from_epoch: old_epoch,
            to_epoch: new_epoch,
            trigger,
            final_commit_count,
            transitioned_at: TimestampMs::now(),
        };

        // Reset engine state for the new epoch.
        let validators: Vec<_> = next_committee
            .active_validators()
            .iter()
            .map(|v| v.index)
            .collect();

        // Prune persisted certificates based on epoch retention policy.
        if let Some(ref p) = self.persist {
            if self.epoch_retention_count == 0 {
                // Legacy: delete current epoch's certificates immediately.
                let old_digests = self.dag.all_digests();
                if !old_digests.is_empty() {
                    if let Err(e) = p.delete_sync(&old_digests) {
                        tracing::error!("epoch pruning failed for epoch {old_epoch}: {e}");
                    }
                }
            } else {
                // Retention-aware: purge the epoch that just fell outside
                // the retention window.  E.g. if retention == 2 and we are
                // advancing from epoch 5 → 6, purge epoch 3 (5 - 2).
                let cutoff = old_epoch.0.saturating_sub(self.epoch_retention_count);
                if old_epoch.0 >= self.epoch_retention_count {
                    match p.purge_by_epoch(EpochNumber(cutoff)) {
                        Ok(n) if n > 0 => {
                            tracing::info!(
                                purged = n,
                                epoch = cutoff,
                                "pruned expired epoch certificates"
                            );
                        }
                        Ok(_) => {}
                        Err(e) => {
                            tracing::error!("epoch retention purge failed for epoch {cutoff}: {e}");
                        }
                    }
                }
            }
        }

        self.epoch = new_epoch;
        self.committee = next_committee;
        self.dag = InMemoryDag::new();
        self.orderer = ShoalOrderer::new(validators);
        self.committed = Vec::new();

        (transition, remaining)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::certificate::{cert_signing_payload, CertificateBuilder};
    use crate::error::ConsensusError;
    use crate::persist::{DagPersistSync, PersistError};
    use crate::types::EpochTransitionTrigger;
    use crate::types::{ReputationScore, ValidatorBitset, ValidatorInfo, CERT_DOMAIN};
    use nexus_crypto::{FalconSigner, FalconSigningKey, FalconVerifyKey, Signer};
    use nexus_primitives::{Amount, Blake3Digest, CommitSequence, RoundNumber, ValidatorIndex};
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct PersistRecorder {
        persisted: Mutex<Vec<nexus_primitives::CertDigest>>,
        deleted: Mutex<Vec<Vec<nexus_primitives::CertDigest>>>,
        purged_epochs: Mutex<Vec<EpochNumber>>,
    }

    impl DagPersistSync for Arc<PersistRecorder> {
        fn persist_sync(&self, cert: &NarwhalCertificate) -> Result<(), PersistError> {
            self.persisted.lock().unwrap().push(cert.cert_digest);
            Ok(())
        }

        fn delete_sync(
            &self,
            digests: &[nexus_primitives::CertDigest],
        ) -> Result<(), PersistError> {
            self.deleted.lock().unwrap().push(digests.to_vec());
            Ok(())
        }

        fn purge_by_epoch(&self, target_epoch: EpochNumber) -> Result<usize, PersistError> {
            self.purged_epochs.lock().unwrap().push(target_epoch);
            Ok(1)
        }
    }

    struct TestHarness {
        engine: ConsensusEngine,
        keys: Vec<(FalconSigningKey, FalconVerifyKey)>,
    }

    impl TestHarness {
        fn new(n: u32) -> Self {
            let mut keys = Vec::new();
            let mut validators = Vec::new();
            for i in 0..n {
                let (sk, vk) = FalconSigner::generate_keypair();
                validators.push(ValidatorInfo {
                    index: ValidatorIndex(i),
                    falcon_pub_key: vk.clone(),
                    stake: Amount(100),
                    reputation: ReputationScore::MAX,
                    is_slashed: false,
                    shard_id: None,
                });
                keys.push((sk, vk));
            }
            let committee = Committee::new(EpochNumber(1), validators).expect("test committee");
            let engine = ConsensusEngine::new(EpochNumber(1), committee);
            Self { engine, keys }
        }

        /// Build a properly signed certificate.
        fn build_cert(
            &self,
            origin: u32,
            round: u64,
            parents: Vec<nexus_primitives::CertDigest>,
            batch_seed: u8,
        ) -> NarwhalCertificate {
            let epoch = EpochNumber(1);
            let batch_digest = Blake3Digest([batch_seed; 32]);
            let origin_idx = ValidatorIndex(origin);
            let round_num = RoundNumber(round);

            let mut builder = CertificateBuilder::new(
                epoch,
                batch_digest,
                origin_idx,
                round_num,
                parents.clone(),
                self.keys.len() as u32,
            );

            let payload =
                cert_signing_payload(epoch, &batch_digest, origin_idx, round_num, &parents)
                    .unwrap();

            // Sign with all validators (always meets stake-weighted quorum).
            for (i, (sk, _)) in self.keys.iter().enumerate() {
                let sig = FalconSigner::sign(sk, CERT_DOMAIN, &payload);
                builder.add_signature(ValidatorIndex(i as u32), sig);
            }

            builder.build(self.engine.committee()).unwrap()
        }

        /// Build a simple genesis cert (round 0, no parents, pre-verified).
        fn genesis_cert(&self, origin: u32, seed: u8) -> NarwhalCertificate {
            let epoch = EpochNumber(1);
            let batch_digest = Blake3Digest([seed; 32]);
            let origin_idx = ValidatorIndex(origin);
            let round = RoundNumber(0);
            let parents = vec![];
            let cert_digest = crate::certificate::compute_cert_digest(
                epoch,
                &batch_digest,
                origin_idx,
                round,
                &parents,
            )
            .unwrap();
            NarwhalCertificate {
                epoch,
                batch_digest,
                origin: origin_idx,
                round,
                parents,
                signatures: vec![],
                signers: ValidatorBitset::new(self.keys.len() as u32),
                cert_digest,
            }
        }

        fn committee_for_epoch(&self, epoch: EpochNumber) -> Committee {
            let validators: Vec<_> = self.engine.committee().all_validators().to_vec();
            Committee::new(epoch, validators).expect("test committee")
        }
    }

    // ── Basic engine tests ──────────────────────────────────────────────

    #[test]
    fn engine_creation() {
        let h = TestHarness::new(4);
        assert_eq!(h.engine.epoch(), EpochNumber(1));
        assert_eq!(h.engine.dag_size(), 0);
        assert_eq!(h.engine.total_commits(), 0);
        assert_eq!(h.engine.pending_commits(), 0);
    }

    #[test]
    fn insert_verified_genesis_certs() {
        let mut h = TestHarness::new(4);
        let g0 = h.genesis_cert(0, 10);
        let g1 = h.genesis_cert(1, 11);

        assert!(!h.engine.insert_verified_certificate(g0).unwrap());
        assert!(!h.engine.insert_verified_certificate(g1).unwrap());
        assert_eq!(h.engine.dag_size(), 2);
    }

    // ── Full pipeline test ──────────────────────────────────────────────

    #[test]
    fn full_pipeline_genesis_to_commit() {
        let mut h = TestHarness::new(4);

        // Round 0: insert genesis certs (pre-verified).
        let g0 = h.genesis_cert(0, 10);
        let g1 = h.genesis_cert(1, 11);
        let d0 = g0.cert_digest;
        let d1 = g1.cert_digest;
        h.engine.insert_verified_certificate(g0).unwrap();
        h.engine.insert_verified_certificate(g1).unwrap();

        // Round 1: properly signed cert referencing genesis.
        let r1 = h.build_cert(0, 1, vec![d0, d1], 20);
        let committed = h.engine.process_certificate(r1).unwrap();
        // Anchor round 0, leader=0, leader has cert at round 0.
        // DAG now at round 1 > anchor round 0 → commit should occur.
        assert!(committed);
        assert_eq!(h.engine.total_commits(), 1);
        assert_eq!(h.engine.pending_commits(), 1);

        let batches = h.engine.take_committed();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].sequence, CommitSequence(0));
        assert_eq!(h.engine.pending_commits(), 0);
    }

    #[test]
    fn process_certificate_rejects_invalid_signature() {
        let mut h = TestHarness::new(4);
        // Build a "cert" with no signatures → will fail quorum check.
        let bad_cert = h.genesis_cert(0, 10);
        // genesis_cert has empty signatures and no signers set,
        // so process_certificate should fail on quorum check.
        let result = h.engine.process_certificate(bad_cert);
        assert!(result.is_err());
    }

    #[test]
    fn duplicate_cert_rejected_in_pipeline() {
        let mut h = TestHarness::new(4);
        let g0 = h.genesis_cert(0, 10);
        h.engine.insert_verified_certificate(g0.clone()).unwrap();
        let result = h.engine.insert_verified_certificate(g0);
        assert!(matches!(
            result,
            Err(ConsensusError::DuplicateCertificate { .. })
        ));
    }

    #[test]
    fn take_committed_drains_buffer() {
        let mut h = TestHarness::new(4);

        let g0 = h.genesis_cert(0, 10);
        let g1 = h.genesis_cert(1, 11);
        let d0 = g0.cert_digest;
        let d1 = g1.cert_digest;
        h.engine.insert_verified_certificate(g0).unwrap();
        h.engine.insert_verified_certificate(g1).unwrap();

        let r1 = h.build_cert(0, 1, vec![d0, d1], 20);
        h.engine.process_certificate(r1).unwrap();

        assert_eq!(h.engine.pending_commits(), 1);
        let first = h.engine.take_committed();
        assert_eq!(first.len(), 1);
        assert_eq!(h.engine.pending_commits(), 0);

        let second = h.engine.take_committed();
        assert!(second.is_empty());
    }

    #[test]
    fn multi_round_commits() {
        let mut h = TestHarness::new(4);

        // Round 0: genesis.
        let g0 = h.genesis_cert(0, 10);
        let g1 = h.genesis_cert(1, 11);
        let d_g0 = g0.cert_digest;
        let d_g1 = g1.cert_digest;
        h.engine.insert_verified_certificate(g0).unwrap();
        h.engine.insert_verified_certificate(g1).unwrap();

        // Round 1: advance past anchor 0 (leader V0).
        let r1a = h.build_cert(0, 1, vec![d_g0, d_g1], 20);
        let r1b = h.build_cert(1, 1, vec![d_g0, d_g1], 21);
        let d_r1a = r1a.cert_digest;
        let d_r1b = r1b.cert_digest;
        h.engine.process_certificate(r1a).unwrap();
        h.engine.process_certificate(r1b).unwrap();

        // First commit at anchor 0.
        let first = h.engine.take_committed();
        assert_eq!(first.len(), 1);

        // Round 2: next anchor (leader V1 due to rotation).
        let r2 = h.build_cert(1, 2, vec![d_r1a, d_r1b], 30);
        let d_r2 = r2.cert_digest;
        h.engine.process_certificate(r2).unwrap();

        // Round 3: advance past anchor 2.
        let r3 = h.build_cert(0, 3, vec![d_r2], 40);
        h.engine.process_certificate(r3).unwrap();

        // Second commit at anchor 2.
        let second = h.engine.take_committed();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].sequence, CommitSequence(1));
        assert_eq!(h.engine.total_commits(), 2);
    }

    // ── Phase B acceptance tests ─────────────────────────────────────

    #[test]
    fn committed_buffer_backpressure() {
        // B-4 / SEC-M7: the committed buffer should refuse new commits when
        // the execution layer has not drained it.
        let mut h = TestHarness::new(4);

        // Manually fill the committed buffer to the limit.
        for i in 0..MAX_COMMITTED_BUFFER {
            h.engine.committed.push(CommittedBatch {
                anchor: nexus_primitives::Blake3Digest::ZERO,
                sequence: CommitSequence(i as u64),
                certificates: vec![],
                committed_at: TimestampMs(0),
            });
        }
        assert_eq!(h.engine.pending_commits(), MAX_COMMITTED_BUFFER);

        // Inserting a pre-verified certificate should fail with CommittedBufferFull.
        let g = h.genesis_cert(0, 10);
        let err = h.engine.insert_verified_certificate(g).unwrap_err();
        match err {
            ConsensusError::CommittedBufferFull { len, max } => {
                assert_eq!(len, MAX_COMMITTED_BUFFER);
                assert_eq!(max, MAX_COMMITTED_BUFFER);
            }
            other => panic!("expected CommittedBufferFull, got: {other:?}"),
        }

        // After draining, should work again.
        h.engine.take_committed();
        let g = h.genesis_cert(1, 11);
        assert!(h.engine.insert_verified_certificate(g).is_ok());
    }

    // ── Phase C acceptance tests ─────────────────────────────────────

    #[test]
    fn insert_verified_certificate_should_reject_external_untrusted_input() {
        // C-3 / SEC-H9: insert_verified_certificate must enforce epoch + digest
        // integrity even though it skips signature verification.
        let mut h = TestHarness::new(4);

        // 1. Wrong epoch: engine is epoch 1, cert claims epoch 99.
        let mut bad_epoch_cert = h.genesis_cert(0, 10);
        bad_epoch_cert.epoch = EpochNumber(99);
        let err = h
            .engine
            .insert_verified_certificate(bad_epoch_cert)
            .unwrap_err();
        assert!(
            matches!(err, ConsensusError::EpochMismatch { expected, got }
                if expected == EpochNumber(1) && got == EpochNumber(99)),
            "expected EpochMismatch, got: {err:?}"
        );

        // 2. Tampered digest: correct epoch but digest doesn't match header.
        let mut bad_digest_cert = h.genesis_cert(0, 10);
        bad_digest_cert.cert_digest = Blake3Digest([0xFF; 32]);
        let err = h
            .engine
            .insert_verified_certificate(bad_digest_cert)
            .unwrap_err();
        assert!(
            matches!(err, ConsensusError::Codec(_)),
            "expected Codec (digest mismatch), got: {err:?}"
        );

        // 3. Valid cert should still work.
        let good = h.genesis_cert(0, 10);
        assert!(h.engine.insert_verified_certificate(good).is_ok());
    }

    #[test]
    fn engine_debug_reports_key_runtime_state() {
        let persist = Arc::new(PersistRecorder::default());
        let h = TestHarness::new(4);
        let engine = ConsensusEngine::new_with_persistence(
            EpochNumber(1),
            h.committee_for_epoch(EpochNumber(1)),
            Box::new(persist),
        );

        let debug = format!("{engine:?}");
        assert!(debug.contains("ConsensusEngine"));
        assert!(debug.contains("epoch"));
        assert!(debug.contains("dag_size"));
        assert!(debug.contains("Some(..)"));
    }

    #[test]
    fn advance_epoch_resets_runtime_state_and_returns_remaining_batches() {
        let mut h = TestHarness::new(4);

        let g0 = h.genesis_cert(0, 10);
        let g1 = h.genesis_cert(1, 11);
        let d0 = g0.cert_digest;
        let d1 = g1.cert_digest;
        h.engine.insert_verified_certificate(g0).unwrap();
        h.engine.insert_verified_certificate(g1).unwrap();
        let r1 = h.build_cert(0, 1, vec![d0, d1], 20);
        assert!(h.engine.process_certificate(r1).unwrap());

        let next_committee = h.committee_for_epoch(EpochNumber(2));
        let (transition, remaining) = h
            .engine
            .advance_epoch(next_committee, EpochTransitionTrigger::Manual);

        assert_eq!(transition.from_epoch, EpochNumber(1));
        assert_eq!(transition.to_epoch, EpochNumber(2));
        assert_eq!(transition.trigger, EpochTransitionTrigger::Manual);
        assert_eq!(transition.final_commit_count, 1);
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].sequence, CommitSequence(0));
        assert_eq!(h.engine.epoch(), EpochNumber(2));
        assert_eq!(h.engine.dag_size(), 0);
        assert_eq!(h.engine.pending_commits(), 0);
        assert_eq!(h.engine.committee().epoch(), EpochNumber(2));
        assert_eq!(h.engine.total_commits(), 0);
    }

    #[test]
    fn advance_epoch_with_zero_retention_deletes_current_epoch_digests() {
        let persist = Arc::new(PersistRecorder::default());
        let mut h = TestHarness::new(4);
        h.engine = ConsensusEngine::new_with_persistence(
            EpochNumber(1),
            h.committee_for_epoch(EpochNumber(1)),
            Box::new(persist.clone()),
        );

        let g0 = h.genesis_cert(0, 10);
        let g1 = h.genesis_cert(1, 11);
        let expected = vec![g0.cert_digest, g1.cert_digest];
        h.engine.insert_verified_certificate(g0).unwrap();
        h.engine.insert_verified_certificate(g1).unwrap();

        let next_committee = h.committee_for_epoch(EpochNumber(2));
        let _ = h
            .engine
            .advance_epoch(next_committee, EpochTransitionTrigger::CommitThreshold);

        let deleted = persist.deleted.lock().unwrap();
        assert_eq!(deleted.len(), 1);
        // Compare as sets: HashMap iteration order for the DAG is non-deterministic.
        let actual_set: std::collections::HashSet<_> = deleted[0].iter().copied().collect();
        let expected_set: std::collections::HashSet<_> = expected.into_iter().collect();
        assert_eq!(actual_set, expected_set);
        assert_eq!(persist.purged_epochs.lock().unwrap().len(), 0);
    }

    #[test]
    fn advance_epoch_with_retention_purges_cutoff_epoch() {
        let persist = Arc::new(PersistRecorder::default());
        let h = TestHarness::new(4);
        let mut engine = ConsensusEngine::new_with_persistence_and_retention(
            EpochNumber(5),
            h.committee_for_epoch(EpochNumber(5)),
            Box::new(persist.clone()),
            2,
        );

        let next_committee = h.committee_for_epoch(EpochNumber(6));
        let _ = engine.advance_epoch(next_committee, EpochTransitionTrigger::TimeElapsed);

        let purged_epochs = persist.purged_epochs.lock().unwrap();
        assert_eq!(*purged_epochs, vec![EpochNumber(3)]);
        assert_eq!(persist.deleted.lock().unwrap().len(), 0);
    }
}
