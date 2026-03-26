// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Batch persistence — write-through and cold-restart recovery for the
//! batch store (`cf_batches`).
//!
//! Mirrors the approach used by `nexus_consensus::persist` for DAG
//! certificates.  A small object-safe trait [`BatchPersistOps`] keeps
//! `BatchStore` non-generic while still allowing disk persistence.

use nexus_execution::types::SignedTransaction;
use nexus_primitives::{BatchDigest, Blake3Digest};
use nexus_storage::traits::{StateStorage, WriteBatchOps};

/// Column family name for batch persistence.
const CF_BATCHES: &str = "cf_batches";

/// Object-safe operations the [`BatchStore`](super::batch_store::BatchStore)
/// calls on every mutation to keep DashMap and disk in lock-step.
pub trait BatchPersistOps: Send + Sync {
    /// Persist a batch of transactions to disk.
    fn put_batch(
        &self,
        digest: &BatchDigest,
        txs: &[SignedTransaction],
    ) -> Result<(), BatchPersistError>;

    /// Retrieve a batch from disk (fallback when DashMap misses).
    fn get_batch(
        &self,
        digest: &BatchDigest,
    ) -> Result<Option<Vec<SignedTransaction>>, BatchPersistError>;

    /// Delete a batch from disk.
    fn delete_batch(&self, digest: &BatchDigest) -> Result<(), BatchPersistError>;

    /// Scan all batches for cold-restart recovery.
    fn restore_batches(
        &self,
    ) -> Result<Vec<(BatchDigest, Vec<SignedTransaction>)>, BatchPersistError>;
}

/// Concrete persistence layer backed by any [`StateStorage`] implementor.
#[derive(Clone)]
pub struct BatchPersistence<S: StateStorage> {
    store: S,
}

impl<S: StateStorage> BatchPersistence<S> {
    /// Wrap an existing storage backend.
    pub fn new(store: S) -> Self {
        Self { store }
    }
}

impl<S: StateStorage + Send + Sync + 'static> BatchPersistOps for BatchPersistence<S> {
    fn put_batch(
        &self,
        digest: &BatchDigest,
        txs: &[SignedTransaction],
    ) -> Result<(), BatchPersistError> {
        let key = digest.0.to_vec();
        let value = bcs::to_bytes(txs).map_err(|e| BatchPersistError::Codec(e.to_string()))?;
        self.store
            .put_sync(CF_BATCHES, key, value)
            .map_err(|e| BatchPersistError::Storage(e.to_string()))?;
        Ok(())
    }

    fn get_batch(
        &self,
        digest: &BatchDigest,
    ) -> Result<Option<Vec<SignedTransaction>>, BatchPersistError> {
        let key = digest.0.to_vec();
        match self
            .store
            .get_sync(CF_BATCHES, &key)
            .map_err(|e| BatchPersistError::Storage(e.to_string()))?
        {
            Some(value) => {
                let txs: Vec<SignedTransaction> =
                    bcs::from_bytes(&value).map_err(|e| BatchPersistError::Codec(e.to_string()))?;
                Ok(Some(txs))
            }
            None => Ok(None),
        }
    }

    fn delete_batch(&self, digest: &BatchDigest) -> Result<(), BatchPersistError> {
        let key = digest.0.to_vec();
        let mut batch = self.store.new_batch();
        batch.delete_cf(CF_BATCHES, key);
        // Drive async write_batch from sync context.
        let handle = tokio::runtime::Handle::current();
        tokio::task::block_in_place(|| {
            handle.block_on(async {
                self.store
                    .write_batch(batch)
                    .await
                    .map_err(|e| BatchPersistError::Storage(e.to_string()))
            })
        })?;
        Ok(())
    }

    fn restore_batches(
        &self,
    ) -> Result<Vec<(BatchDigest, Vec<SignedTransaction>)>, BatchPersistError> {
        let start = [0u8; 32];
        let end = [0xFFu8; 32];
        let raw = self
            .store
            .scan(CF_BATCHES, &start, &end)
            .map_err(|e| BatchPersistError::Storage(e.to_string()))?;

        let mut result = Vec::with_capacity(raw.len());
        for (key, value) in raw {
            if key.len() != 32 {
                continue; // skip malformed keys
            }
            let mut digest_bytes = [0u8; 32];
            digest_bytes.copy_from_slice(&key);
            let digest = Blake3Digest(digest_bytes);
            let txs: Vec<SignedTransaction> =
                bcs::from_bytes(&value).map_err(|e| BatchPersistError::Codec(e.to_string()))?;
            result.push((digest, txs));
        }
        Ok(result)
    }
}

/// Errors from batch persistence operations.
#[derive(Debug, thiserror::Error)]
pub enum BatchPersistError {
    /// BCS serialization / deserialization failure.
    #[error("codec error: {0}")]
    Codec(String),
    /// Storage backend error.
    #[error("storage error: {0}")]
    Storage(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_crypto::{DilithiumSigner, Signer};
    use nexus_execution::types::{
        compute_tx_digest, TransactionBody, TransactionPayload, TX_DOMAIN,
    };
    use nexus_primitives::{AccountAddress, Amount, Blake3Digest, EpochNumber, ShardId, TokenId};
    use nexus_storage::MemoryStore;

    fn make_tx(seq: u64) -> SignedTransaction {
        let (sk, vk) = DilithiumSigner::generate_keypair();
        let body = TransactionBody {
            sender: AccountAddress([1u8; 32]),
            sequence_number: seq,
            expiry_epoch: EpochNumber(100),
            gas_limit: 10_000,
            gas_price: 1,
            target_shard: Some(ShardId(0)),
            payload: TransactionPayload::Transfer {
                recipient: AccountAddress([2u8; 32]),
                amount: Amount(100),
                token: TokenId::Native,
            },
            chain_id: 1,
        };
        let digest = compute_tx_digest(&body).expect("digest");
        let body_bytes = bcs::to_bytes(&body).expect("bcs");
        let sig = DilithiumSigner::sign(&sk, TX_DOMAIN, &body_bytes);
        SignedTransaction {
            body,
            signature: sig,
            sender_pk: vk,
            digest,
        }
    }

    #[tokio::test]
    async fn put_and_get_roundtrip() {
        let store = MemoryStore::new();
        let persist = BatchPersistence::new(store);
        let digest = Blake3Digest([42u8; 32]);
        let txs = vec![make_tx(1), make_tx(2)];

        persist.put_batch(&digest, &txs).unwrap();
        let got = persist.get_batch(&digest).unwrap().unwrap();
        assert_eq!(got.len(), 2);
    }

    #[tokio::test]
    async fn get_missing_returns_none() {
        let store = MemoryStore::new();
        let persist = BatchPersistence::new(store);
        let digest = Blake3Digest([99u8; 32]);
        assert!(persist.get_batch(&digest).unwrap().is_none());
    }

    #[tokio::test]
    async fn restore_empty() {
        let store = MemoryStore::new();
        let persist = BatchPersistence::new(store);
        let batches = persist.restore_batches().unwrap();
        assert!(batches.is_empty());
    }

    #[tokio::test]
    async fn restore_roundtrip() {
        let store = MemoryStore::new();
        let persist = BatchPersistence::new(store);

        let d1 = Blake3Digest([1u8; 32]);
        let d2 = Blake3Digest([2u8; 32]);
        persist.put_batch(&d1, &[make_tx(1)]).unwrap();
        persist.put_batch(&d2, &[make_tx(2), make_tx(3)]).unwrap();

        let batches = persist.restore_batches().unwrap();
        assert_eq!(batches.len(), 2);
    }
}
