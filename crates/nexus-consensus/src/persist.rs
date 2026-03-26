// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! DAG persistence — write-through and cold-restart recovery for consensus
//! certificates.
//!
//! [`DagPersistence`] bridges `InMemoryDag` to `cf_certificates` in
//! `nexus-storage`.  It is intentionally kept separate from the DAG data
//! structure so that `InMemoryDag` stays a pure in-memory type (tests can
//! use it without a storage backend), while the engine injects persistence
//! as an optional layer.
//!
//! # Key format
//!
//! Key = `CertDigest` raw 32 bytes.
//! Value = `NarwhalCertificate` BCS-serialized.
//!
//! # Recovery
//!
//! On cold restart [`DagPersistence::restore_certificates`] scans the full
//! `cf_certificates` column family, BCS-deserialises each certificate, and
//! returns them sorted by `(round, origin)` so the caller can replay them
//! into `InMemoryDag` in causal order.

use nexus_primitives::{Blake3Digest, CertDigest};
use nexus_storage::traits::{StateStorage, WriteBatchOps};

use crate::types::NarwhalCertificate;

/// Column family name constant for certificates.
const CF_CERTS: &str = "cf_certificates";

// ── Trait for sync persistence (engine uses this) ────────────────────────────

/// Synchronous persistence operations that the consensus engine calls on
/// every certificate insertion.  This trait is object-safe so the engine
/// can hold `Option<Box<dyn DagPersistSync>>` without becoming generic.
pub trait DagPersistSync: Send + Sync {
    /// Persist a single verified certificate.
    fn persist_sync(&self, cert: &NarwhalCertificate) -> Result<(), PersistError>;

    /// Delete a set of certificates by digest (epoch pruning).
    fn delete_sync(&self, digests: &[CertDigest]) -> Result<(), PersistError>;

    /// Delete all persisted certificates belonging to a specific epoch.
    ///
    /// Scans `cf_certificates`, deserializes each cert, and deletes those
    /// whose `epoch` field matches `target_epoch`.  Returns the count of
    /// deleted certificates.
    fn purge_by_epoch(
        &self,
        target_epoch: nexus_primitives::EpochNumber,
    ) -> Result<usize, PersistError>;
}

/// Provides write-through persistence and cold-restart recovery for DAG
/// certificates.
///
/// Generic over `S: StateStorage` so that tests can use `MemoryStore`.
#[derive(Clone)]
pub struct DagPersistence<S: StateStorage> {
    store: S,
}

impl<S: StateStorage> DagPersistence<S> {
    /// Wrap an existing storage backend.
    pub fn new(store: S) -> Self {
        Self { store }
    }

    /// Persist a single certificate to `cf_certificates` (write-through).
    ///
    /// Called immediately after the certificate passes validation and is
    /// inserted into `InMemoryDag`.
    pub async fn persist_certificate(&self, cert: &NarwhalCertificate) -> Result<(), PersistError> {
        let key = cert.cert_digest.0.to_vec();
        let value = bcs::to_bytes(cert).map_err(|e| PersistError::Codec(e.to_string()))?;

        let mut batch = self.store.new_batch();
        batch.put_cf(CF_CERTS, key, value);
        self.store
            .write_batch(batch)
            .await
            .map_err(|e| PersistError::Storage(e.to_string()))?;
        Ok(())
    }

    /// Persist a certificate synchronously (for non-async callers).
    pub fn persist_certificate_sync(&self, cert: &NarwhalCertificate) -> Result<(), PersistError> {
        let key = cert.cert_digest.0.to_vec();
        let value = bcs::to_bytes(cert).map_err(|e| PersistError::Codec(e.to_string()))?;

        self.store
            .put_sync(CF_CERTS, key, value)
            .map_err(|e| PersistError::Storage(e.to_string()))?;
        Ok(())
    }

    /// Delete certificates from `cf_certificates` by digest (sync).
    ///
    /// Uses `tokio::task::block_in_place` + `Handle::block_on` to drive the
    /// async `write_batch` from a synchronous call site.  Safe because the
    /// consensus engine always runs inside a multi-threaded tokio runtime.
    pub fn delete_certificates_sync(&self, digests: &[CertDigest]) -> Result<(), PersistError> {
        if digests.is_empty() {
            return Ok(());
        }
        let mut batch = self.store.new_batch();
        for d in digests {
            batch.delete_cf(CF_CERTS, d.0.to_vec());
        }
        // Drive the async write_batch from a sync context.
        let handle = tokio::runtime::Handle::current();
        tokio::task::block_in_place(|| {
            handle.block_on(async {
                self.store
                    .write_batch(batch)
                    .await
                    .map_err(|e| PersistError::Storage(e.to_string()))
            })
        })?;
        Ok(())
    }

    /// Delete certificates by digest (async, uses write_batch for atomicity).
    pub async fn delete_certificates(&self, digests: &[CertDigest]) -> Result<(), PersistError> {
        if digests.is_empty() {
            return Ok(());
        }
        let mut batch = self.store.new_batch();
        for d in digests {
            batch.delete_cf(CF_CERTS, d.0.to_vec());
        }
        self.store
            .write_batch(batch)
            .await
            .map_err(|e| PersistError::Storage(e.to_string()))?;
        Ok(())
    }

    /// Scan `cf_certificates` and return all persisted certificates sorted
    /// by `(round, origin)` for deterministic DAG replay.
    ///
    /// On an empty database this returns an empty `Vec`.
    pub fn restore_certificates(&self) -> Result<Vec<NarwhalCertificate>, PersistError> {
        // Scan the full key range of cf_certificates.
        // Key space is 32-byte BLAKE3 digests (0x00..00 to 0xFF..FF).
        let start = [0u8; 32];
        let end = [0xFFu8; 32];
        let raw = self
            .store
            .scan(CF_CERTS, &start, &end)
            .map_err(|e| PersistError::Storage(e.to_string()))?;

        let mut certs = Vec::with_capacity(raw.len());
        for (_key, value) in raw {
            let cert: NarwhalCertificate =
                bcs::from_bytes(&value).map_err(|e| PersistError::Codec(e.to_string()))?;
            certs.push(cert);
        }

        // Sort by (round, origin) for deterministic causal replay.
        certs.sort_by_key(|c| (c.round, c.origin));
        Ok(certs)
    }

    /// Delete all persisted certificates that belong to a specific epoch.
    ///
    /// Scans the full key range, deserializes each cert, collects digests
    /// whose `epoch` matches `target_epoch`, and batch-deletes them.
    /// Returns the count of deleted certificates.
    pub fn purge_by_epoch(
        &self,
        target_epoch: nexus_primitives::EpochNumber,
    ) -> Result<usize, PersistError> {
        let start = [0u8; 32];
        let end = [0xFFu8; 32];
        let raw = self
            .store
            .scan(CF_CERTS, &start, &end)
            .map_err(|e| PersistError::Storage(e.to_string()))?;

        let mut to_delete = Vec::new();
        for (key, value) in &raw {
            let cert: NarwhalCertificate =
                bcs::from_bytes(value).map_err(|e| PersistError::Codec(e.to_string()))?;
            if cert.epoch == target_epoch && key.len() == 32 {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(key);
                to_delete.push(Blake3Digest(arr));
            }
        }

        let count = to_delete.len();
        if !to_delete.is_empty() {
            self.delete_certificates_sync(&to_delete)?;
        }
        Ok(count)
    }

    /// Reference to the underlying store (for epoch-store or other layers).
    pub fn store(&self) -> &S {
        &self.store
    }
}

// ── DagPersistSync blanket impl for DagPersistence<S> ────────────────────────

impl<S: StateStorage + Send + Sync + 'static> DagPersistSync for DagPersistence<S> {
    fn persist_sync(&self, cert: &NarwhalCertificate) -> Result<(), PersistError> {
        self.persist_certificate_sync(cert)
    }

    fn delete_sync(&self, digests: &[CertDigest]) -> Result<(), PersistError> {
        self.delete_certificates_sync(digests)
    }

    fn purge_by_epoch(
        &self,
        target_epoch: nexus_primitives::EpochNumber,
    ) -> Result<usize, PersistError> {
        self.purge_by_epoch(target_epoch)
    }
}

/// Errors from DAG persistence operations.
#[derive(Debug, thiserror::Error)]
pub enum PersistError {
    /// BCS serialization / deserialization failure.
    #[error("codec error: {0}")]
    Codec(String),
    /// Storage backend error.
    #[error("storage error: {0}")]
    Storage(String),
}

// Provide ColumnFamily constant for external use.
/// The column family name used for consensus certificate persistence.
pub const CERTIFICATES_CF: &str = CF_CERTS;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::certificate::compute_cert_digest;
    use crate::types::ValidatorBitset;
    use nexus_primitives::{Blake3Digest, EpochNumber, RoundNumber, ValidatorIndex};
    use nexus_storage::MemoryStore;

    fn make_test_cert(origin: u32, round: u64, seed: u8) -> NarwhalCertificate {
        let epoch = EpochNumber(1);
        let batch_digest = Blake3Digest([seed; 32]);
        let origin_idx = ValidatorIndex(origin);
        let round_num = RoundNumber(round);
        let parents = vec![];
        let cert_digest =
            compute_cert_digest(epoch, &batch_digest, origin_idx, round_num, &parents).unwrap();
        NarwhalCertificate {
            epoch,
            batch_digest,
            origin: origin_idx,
            round: round_num,
            parents,
            signatures: vec![],
            signers: ValidatorBitset::new(4),
            cert_digest,
        }
    }

    #[tokio::test]
    async fn persist_and_restore_roundtrip() {
        let store = MemoryStore::new();
        let persistence = DagPersistence::new(store);

        let c0 = make_test_cert(0, 0, 10);
        let c1 = make_test_cert(1, 0, 11);
        let c2 = make_test_cert(0, 1, 20);

        persistence.persist_certificate(&c0).await.unwrap();
        persistence.persist_certificate(&c1).await.unwrap();
        persistence.persist_certificate(&c2).await.unwrap();

        let restored = persistence.restore_certificates().unwrap();
        assert_eq!(restored.len(), 3);
        // Should be sorted by (round, origin).
        assert_eq!(restored[0].round, RoundNumber(0));
        assert_eq!(restored[0].origin, ValidatorIndex(0));
        assert_eq!(restored[1].round, RoundNumber(0));
        assert_eq!(restored[1].origin, ValidatorIndex(1));
        assert_eq!(restored[2].round, RoundNumber(1));
        assert_eq!(restored[2].origin, ValidatorIndex(0));
    }

    #[tokio::test]
    async fn restore_empty_database() {
        let store = MemoryStore::new();
        let persistence = DagPersistence::new(store);
        let restored = persistence.restore_certificates().unwrap();
        assert!(restored.is_empty());
    }

    #[tokio::test]
    async fn delete_certificates_removes_from_store() {
        let store = MemoryStore::new();
        let persistence = DagPersistence::new(store);

        let c0 = make_test_cert(0, 0, 10);
        let c1 = make_test_cert(1, 0, 11);
        persistence.persist_certificate(&c0).await.unwrap();
        persistence.persist_certificate(&c1).await.unwrap();

        persistence
            .delete_certificates(&[c0.cert_digest])
            .await
            .unwrap();

        let restored = persistence.restore_certificates().unwrap();
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].cert_digest, c1.cert_digest);
    }

    #[tokio::test]
    async fn persist_sync_roundtrip() {
        let store = MemoryStore::new();
        let persistence = DagPersistence::new(store);

        let c0 = make_test_cert(0, 0, 42);
        persistence.persist_certificate_sync(&c0).unwrap();

        let restored = persistence.restore_certificates().unwrap();
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].cert_digest, c0.cert_digest);
    }
}
