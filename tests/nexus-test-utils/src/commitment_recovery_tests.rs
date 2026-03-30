// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Commitment persistence recovery tests over real RocksDB paths.
//!
//! These tests isolate commitment-specific cold-start behaviour from the
//! broader snapshot/prune/reopen recovery suite.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use nexus_config::ExecutionConfig;
use nexus_consensus::ConsensusEngine;
use nexus_crypto::{DilithiumSigner, Signer};
use nexus_execution::spawn_execution_service;
use nexus_execution::types::{
    compute_tx_digest, SignedTransaction, TransactionBody, TransactionPayload, TX_DOMAIN,
};
use nexus_node::backends::{
    LiveStateProofBackend, SharedChainHead, StorageQueryBackend, StorageStateView,
};
use nexus_node::batch_store::BatchStore;
use nexus_node::commitment_tracker::{
    new_shared_tracker_with_persistence, CommitmentPersistSync, CommitmentTracker,
    PersistentCommitmentBackend, SharedCommitmentTracker, StateChangeEntry,
};
use nexus_node::execution_bridge::{
    spawn_execution_bridge, BridgeContext, EpochContext, ExecutionBridgeConfig, ShardRouter,
};
use nexus_node::readiness::NodeReadiness;
use nexus_primitives::{
    AccountAddress, Amount, Blake3Digest, CommitSequence, EpochNumber, RoundNumber, ShardId,
    TokenId, ValidatorIndex,
};
use nexus_rpc::{QueryBackend, StateProofBackend};
use nexus_storage::commitment::Blake3SmtCommitment;
use nexus_storage::commitment_persist::CommitmentMetaRecord;
use nexus_storage::config::StorageConfig;
use nexus_storage::rocks::RocksStore;
use nexus_storage::traits::{StateCommitment, StateStorage, WriteBatchOps};
use nexus_storage::{ColumnFamily, StorageError};
use tempfile::TempDir;

use crate::fixtures::consensus::TestCommittee;

fn balance_storage_key(shard_id: ShardId, address: AccountAddress) -> Vec<u8> {
    let mut key = nexus_storage::AccountKey { shard_id, address }.to_bytes();
    key.extend_from_slice(b"balance");
    key
}

fn build_transfer(recipient: AccountAddress, amount: u64) -> (SignedTransaction, AccountAddress) {
    let (sk, pk) = DilithiumSigner::generate_keypair();
    let sender = AccountAddress::from_dilithium_pubkey(pk.as_bytes());
    let body = TransactionBody {
        sender,
        sequence_number: 0,
        expiry_epoch: EpochNumber(1000),
        gas_limit: 50_000,
        gas_price: 1,
        target_shard: None,
        payload: TransactionPayload::Transfer {
            recipient,
            amount: Amount(amount),
            token: TokenId::Native,
        },
        chain_id: 1,
    };
    let digest = compute_tx_digest(&body).unwrap();
    let sig = DilithiumSigner::sign(&sk, TX_DOMAIN, digest.as_bytes());
    (
        SignedTransaction {
            body,
            signature: sig,
            sender_pk: pk,
            digest,
        },
        sender,
    )
}

async fn seed_balance<S: StateStorage>(
    store: &S,
    shard_id: ShardId,
    address: AccountAddress,
    amount: Amount,
) {
    let mut batch = store.new_batch();
    batch.put_cf(
        ColumnFamily::State.as_str(),
        balance_storage_key(shard_id, address),
        amount.0.to_le_bytes().to_vec(),
    );
    store.write_batch(batch).await.unwrap();
}

async fn wait_for_commit(commit_seq: &AtomicU64, expected: u64) {
    for _ in 0..100 {
        if commit_seq.load(Ordering::Acquire) == expected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for commit sequence {expected}");
}

async fn wait_for_dead_letter<S: StateStorage>(store: &S, sequence: u64) -> serde_json::Value {
    let mut key = b"dead_letter:".to_vec();
    key.extend_from_slice(&sequence.to_be_bytes());

    for _ in 0..150 {
        if let Some(raw) = store.get_sync(ColumnFamily::Blocks.as_str(), &key).unwrap() {
            return serde_json::from_slice(&raw).unwrap();
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    panic!("timed out waiting for dead-letter record for sequence {sequence}");
}

async fn wait_for_receipt<S: StateStorage>(store: &S, tx_digest: &Blake3Digest) -> Vec<u8> {
    for _ in 0..150 {
        if let Some(raw) = store
            .get_sync(ColumnFamily::Receipts.as_str(), tx_digest.as_bytes())
            .unwrap()
        {
            return raw;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    panic!("timed out waiting for receipt {tx_digest}");
}

struct FailingPersistence;

impl CommitmentPersistSync for FailingPersistence {
    fn load_meta(&self) -> Result<Option<CommitmentMetaRecord>, StorageError> {
        Ok(None)
    }

    fn restore_entries(&self) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StorageError> {
        Ok(Vec::new())
    }

    fn apply_change_set(
        &self,
        _changes: &[(Vec<u8>, Option<Vec<u8>>)],
    ) -> Result<CommitmentMetaRecord, StorageError> {
        Err(StorageError::StateCommitment(
            "forced commitment persistence failure".into(),
        ))
    }
}

fn failing_shared_tracker() -> SharedCommitmentTracker {
    Arc::new(RwLock::new(
        CommitmentTracker::with_persistence(Box::new(FailingPersistence)).unwrap(),
    ))
}

#[test]
fn shared_tracker_with_persistence_restores_via_startup_path() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("db");
    let config = StorageConfig::for_testing(db_path.clone());

    let expected_root = {
        let store = RocksStore::open(&config).unwrap();
        let shared =
            new_shared_tracker_with_persistence(store, config.commitment_cache_size).unwrap();
        let mut tracker = shared.write().unwrap();
        tracker
            .try_apply_state_changes(&[
                StateChangeEntry {
                    key: b"alpha",
                    value: Some(b"one"),
                },
                StateChangeEntry {
                    key: b"beta",
                    value: Some(b"two"),
                },
            ])
            .unwrap();
        assert_eq!(tracker.persisted_tree_version(), Some(1));
        tracker.commitment_root()
    };

    let store = RocksStore::open(&config).unwrap();
    let restored =
        new_shared_tracker_with_persistence(store, config.commitment_cache_size).unwrap();
    let tracker = restored.read().unwrap();

    assert_eq!(tracker.commitment_root(), expected_root);
    assert_eq!(tracker.entry_count(), 2);
    assert_eq!(tracker.persisted_tree_version(), Some(1));

    let (value, proof) = tracker.prove_key(b"beta").unwrap();
    assert_eq!(value.as_deref(), Some(b"two".as_slice()));
    Blake3SmtCommitment::verify_proof(&expected_root, b"beta", Some(b"two"), &proof).unwrap();
}

#[test]
fn node_cold_start_wires_query_and_proof_backends_from_rocksstore() {
    let shard_id = ShardId(0);
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("db");
    let config = StorageConfig::for_testing(db_path.clone());

    let recipient = AccountAddress([0x44; 32]);
    let full_key = balance_storage_key(shard_id, recipient);
    let persisted_balance = Amount(777_000);
    let expected_root = {
        let store = RocksStore::open(&config).unwrap();
        store
            .put_sync(
                ColumnFamily::State.as_str(),
                full_key.clone(),
                persisted_balance.0.to_le_bytes().to_vec(),
            )
            .unwrap();
        let tracker =
            new_shared_tracker_with_persistence(store.clone(), config.commitment_cache_size)
                .unwrap();
        {
            let mut guard = tracker.write().unwrap();
            let balance_bytes = persisted_balance.0.to_le_bytes();
            guard
                .try_apply_state_changes(&[StateChangeEntry {
                    key: &full_key,
                    value: Some(&balance_bytes),
                }])
                .unwrap();
            guard.commitment_root()
        }
    };

    let store = RocksStore::open(&config).unwrap();
    let epoch = Arc::new(AtomicU64::new(3));
    let commit_seq = Arc::new(AtomicU64::new(7));
    let query_backend = StorageQueryBackend::new(store.clone(), epoch, commit_seq);
    let proof_backend = LiveStateProofBackend::new(
        new_shared_tracker_with_persistence(store, config.commitment_cache_size).unwrap(),
    );

    assert_eq!(
        query_backend
            .account_balance(&recipient, &TokenId::Native)
            .unwrap(),
        persisted_balance
    );

    let root = proof_backend.commitment_root().unwrap();
    assert_eq!(root, expected_root);
    let (value, proof) = proof_backend.prove_key(&full_key).unwrap();
    let expected_value = persisted_balance.0.to_le_bytes();
    assert_eq!(value.as_deref(), Some(expected_value.as_slice()));
    Blake3SmtCommitment::verify_proof(&root, &full_key, Some(&expected_value), &proof).unwrap();
}

#[test]
fn commitment_tracker_recovers_from_rocksstore_restart() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("db");
    let config = StorageConfig::for_testing(db_path.clone());
    let expected_root = {
        let store = RocksStore::open(&config).unwrap();
        let mut tracker = CommitmentTracker::with_persistence(Box::new(
            PersistentCommitmentBackend::new(store, config.commitment_cache_size),
        ))
        .unwrap();
        tracker
            .try_apply_state_changes(&[
                StateChangeEntry {
                    key: b"alpha",
                    value: Some(b"one"),
                },
                StateChangeEntry {
                    key: b"beta",
                    value: Some(b"two"),
                },
                StateChangeEntry {
                    key: b"gamma",
                    value: Some(b"three"),
                },
            ])
            .unwrap();
        assert_eq!(tracker.persisted_tree_version(), Some(1));
        tracker.commitment_root()
    };

    let store = RocksStore::open(&config).unwrap();
    let restored = CommitmentTracker::with_persistence(Box::new(PersistentCommitmentBackend::new(
        store,
        config.commitment_cache_size,
    )))
    .unwrap();

    assert_eq!(restored.commitment_root(), expected_root);
    assert_eq!(restored.entry_count(), 3);
    assert_eq!(restored.persisted_tree_version(), Some(1));

    let (value, proof) = restored.prove_key(b"beta").unwrap();
    assert_eq!(value.as_deref(), Some(b"two".as_slice()));
    Blake3SmtCommitment::verify_proof(&expected_root, b"beta", Some(b"two"), &proof).unwrap();
}

#[test]
fn commitment_tracker_multi_restart_preserves_versions_and_root() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("db");
    let config = StorageConfig::for_testing(db_path.clone());

    {
        let store = RocksStore::open(&config).unwrap();
        let mut tracker = CommitmentTracker::with_persistence(Box::new(
            PersistentCommitmentBackend::new(store, config.commitment_cache_size),
        ))
        .unwrap();
        tracker
            .try_apply_state_changes(&[
                StateChangeEntry {
                    key: b"a",
                    value: Some(b"1"),
                },
                StateChangeEntry {
                    key: b"c",
                    value: Some(b"3"),
                },
            ])
            .unwrap();
        assert_eq!(tracker.persisted_tree_version(), Some(1));
    }

    let expected_root = {
        let store = RocksStore::open(&config).unwrap();
        let mut tracker = CommitmentTracker::with_persistence(Box::new(
            PersistentCommitmentBackend::new(store, config.commitment_cache_size),
        ))
        .unwrap();
        assert_eq!(tracker.persisted_tree_version(), Some(1));
        tracker
            .try_apply_state_changes(&[
                StateChangeEntry {
                    key: b"b",
                    value: Some(b"2"),
                },
                StateChangeEntry {
                    key: b"c",
                    value: Some(b"30"),
                },
                StateChangeEntry {
                    key: b"a",
                    value: None,
                },
            ])
            .unwrap();
        assert_eq!(tracker.persisted_tree_version(), Some(2));
        tracker.commitment_root()
    };

    let store = RocksStore::open(&config).unwrap();
    let restored = CommitmentTracker::with_persistence(Box::new(PersistentCommitmentBackend::new(
        store,
        config.commitment_cache_size,
    )))
    .unwrap();

    let mut recomputed = Blake3SmtCommitment::new();
    recomputed.update(&[(b"b".as_slice(), b"2".as_slice()), (b"c", b"30")]);

    assert_eq!(restored.persisted_tree_version(), Some(2));
    assert_eq!(restored.entry_count(), 2);
    assert_eq!(restored.commitment_root(), expected_root);
    assert_eq!(restored.commitment_root(), recomputed.root_commitment());
}

#[test]
fn commitment_tracker_rocksstore_restart_restores_large_tree_with_tiny_cache() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("db");
    let mut config = StorageConfig::for_testing(db_path.clone());
    config.commitment_cache_size = 1;

    let expected_root = {
        let store = RocksStore::open(&config).unwrap();
        let mut tracker = CommitmentTracker::with_persistence(Box::new(
            PersistentCommitmentBackend::new(store, config.commitment_cache_size),
        ))
        .unwrap();

        let mut changes = Vec::new();
        for index in 0..16u8 {
            let key = vec![b'k', index];
            let value = vec![b'v', index];
            changes.push((key, value));
        }

        let state_changes: Vec<_> = changes
            .iter()
            .map(|(key, value)| StateChangeEntry {
                key: key.as_slice(),
                value: Some(value.as_slice()),
            })
            .collect();
        tracker.try_apply_state_changes(&state_changes).unwrap();
        tracker.commitment_root()
    };

    let store = RocksStore::open(&config).unwrap();
    let restored = CommitmentTracker::with_persistence(Box::new(PersistentCommitmentBackend::new(
        store,
        config.commitment_cache_size,
    )))
    .unwrap();

    assert_eq!(restored.entry_count(), 16);
    assert_eq!(restored.commitment_root(), expected_root);

    let expected_value = [b'v', 15];
    let (value, proof) = restored.prove_key(b"k\x0f").unwrap();
    assert_eq!(value.as_deref(), Some(expected_value.as_slice()));
    Blake3SmtCommitment::verify_proof(&expected_root, b"k\x0f", Some(&expected_value), &proof)
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execution_bridge_rocksstore_restart_restores_provable_commitment() {
    let shard_id = ShardId(0);
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("db");
    let config = StorageConfig::for_testing(db_path.clone());
    let store = RocksStore::open(&config).unwrap();

    let recipient = AccountAddress([0xBB; 32]);
    let (tx, sender) = build_transfer(recipient, 100);
    seed_balance(&store, shard_id, sender, Amount(1_000_000)).await;
    let tc = TestCommittee::new(1, EpochNumber(0));
    let engine = Arc::new(Mutex::new(ConsensusEngine::new(
        tc.epoch,
        tc.committee.clone(),
    )));
    let batch_store = Arc::new(BatchStore::new());

    let batch_digest_r0 = Blake3Digest([0x11; 32]);
    let cert_r0 = tc.build_cert(batch_digest_r0, ValidatorIndex(0), RoundNumber(0), vec![]);
    batch_store.insert(batch_digest_r0, vec![tx.clone()]);
    {
        let mut eng = engine.lock().unwrap();
        eng.insert_verified_certificate(cert_r0.clone()).unwrap();
    }

    let batch_digest_r1 = Blake3Digest([0x22; 32]);
    let cert_r1 = tc.build_cert(
        batch_digest_r1,
        ValidatorIndex(0),
        RoundNumber(1),
        vec![cert_r0.cert_digest],
    );
    {
        let mut eng = engine.lock().unwrap();
        let committed = eng.insert_verified_certificate(cert_r1).unwrap();
        assert!(committed, "round-1 cert should trigger a commit");
        assert!(
            eng.pending_commits() > 0,
            "bridge should have work to drain"
        );
    }

    let state_view = StorageStateView::new(store.clone(), shard_id);
    let exec_handle = spawn_execution_service(
        ExecutionConfig::for_testing(),
        shard_id,
        Arc::new(state_view),
    );
    let commit_seq = Arc::new(AtomicU64::new(u64::MAX));
    let chain_head = SharedChainHead::new();
    let tracker =
        new_shared_tracker_with_persistence(store.clone(), config.commitment_cache_size).unwrap();

    let mut shard_chain_heads = std::collections::HashMap::new();
    shard_chain_heads.insert(shard_id, chain_head.clone());
    let test_readiness = NodeReadiness::new();
    let bridge = spawn_execution_bridge(
        ExecutionBridgeConfig {
            poll_interval: Duration::from_millis(20),
            num_shards: 1,
        },
        engine,
        BridgeContext {
            shard_router: ShardRouter::single(exec_handle.clone()),
            batch_store: batch_store.clone(),
            store: store.clone(),
            commit_seq: commit_seq.clone(),
            events_tx: None,
            shard_chain_heads,
            provenance_store: None,
            commitment_tracker: Some(tracker.clone()),
        },
        EpochContext {
            epoch_manager: None,
            epoch_counter: None,
            rotation_policy: None,
            staking_snapshot_provider: None,
        },
        test_readiness.execution_handle(),
    );

    wait_for_commit(&commit_seq, 0).await;

    let recipient_key = balance_storage_key(shard_id, recipient);
    let persisted_value = store
        .get_sync(ColumnFamily::State.as_str(), &recipient_key)
        .unwrap()
        .expect("recipient balance must be written before restart");

    let expected_root = {
        let guard = tracker.read().unwrap();
        assert!(
            guard.entry_count() > 0,
            "commitment tracker should have entries after execution"
        );
        guard.commitment_root()
    };

    bridge.abort();
    let _ = bridge.await;
    exec_handle.shutdown().await.unwrap();
    drop(exec_handle);
    drop(tracker);
    drop(store);
    // Allow the actor task to be polled to completion so its Arc<StateView>
    // (and the RocksStore clone inside) is dropped before we reopen the DB.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let reopened_store = RocksStore::open(&config).unwrap();
    let restarted =
        new_shared_tracker_with_persistence(reopened_store.clone(), config.commitment_cache_size)
            .unwrap();
    let restarted_guard = restarted.read().unwrap();

    assert_eq!(restarted_guard.commitment_root(), expected_root);
    let (value, proof) = restarted_guard.prove_key(&recipient_key).unwrap();
    assert_eq!(value.as_deref(), Some(persisted_value.as_slice()));
    Blake3SmtCommitment::verify_proof(
        &expected_root,
        &recipient_key,
        Some(persisted_value.as_slice()),
        &proof,
    )
    .unwrap();

    let receipt = reopened_store
        .get_sync(ColumnFamily::Receipts.as_str(), tx.digest.as_bytes())
        .unwrap();
    assert!(
        receipt.is_some(),
        "receipt must survive restart on RocksStore"
    );

    let head = chain_head
        .get()
        .expect("chain head should be updated by execution bridge");
    assert_eq!(head.sequence, CommitSequence(0).0);
    assert_eq!(head.state_root, hex::encode(expected_root.0));
    assert!(head.committed_at_ms > 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execution_bridge_rejects_commit_seq_when_commitment_persistence_fails() {
    let shard_id = ShardId(0);
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("db");
    let config = StorageConfig::for_testing(db_path.clone());
    let store = RocksStore::open(&config).unwrap();

    let recipient = AccountAddress([0x55; 32]);
    let (tx, sender) = build_transfer(recipient, 100);
    seed_balance(&store, shard_id, sender, Amount(1_000_000)).await;

    let tc = TestCommittee::new(1, EpochNumber(0));
    let engine = Arc::new(Mutex::new(ConsensusEngine::new(
        tc.epoch,
        tc.committee.clone(),
    )));
    let batch_store = Arc::new(BatchStore::new());

    let batch_digest_r0 = Blake3Digest([0x31; 32]);
    let cert_r0 = tc.build_cert(batch_digest_r0, ValidatorIndex(0), RoundNumber(0), vec![]);
    batch_store.insert(batch_digest_r0, vec![tx.clone()]);
    {
        let mut eng = engine.lock().unwrap();
        eng.insert_verified_certificate(cert_r0.clone()).unwrap();
    }

    let batch_digest_r1 = Blake3Digest([0x32; 32]);
    let cert_r1 = tc.build_cert(
        batch_digest_r1,
        ValidatorIndex(0),
        RoundNumber(1),
        vec![cert_r0.cert_digest],
    );
    {
        let mut eng = engine.lock().unwrap();
        let committed = eng.insert_verified_certificate(cert_r1).unwrap();
        assert!(committed);
    }

    let exec_handle = spawn_execution_service(
        ExecutionConfig::for_testing(),
        shard_id,
        Arc::new(StorageStateView::new(store.clone(), shard_id)),
    );
    let commit_seq = Arc::new(AtomicU64::new(u64::MAX));
    let chain_head = SharedChainHead::new();
    let tracker = failing_shared_tracker();

    let mut shard_chain_heads2 = std::collections::HashMap::new();
    shard_chain_heads2.insert(shard_id, chain_head.clone());
    let test_readiness2 = NodeReadiness::new();
    let bridge = spawn_execution_bridge(
        ExecutionBridgeConfig {
            poll_interval: Duration::from_millis(20),
            num_shards: 1,
        },
        engine,
        BridgeContext {
            shard_router: ShardRouter::single(exec_handle.clone()),
            batch_store: batch_store.clone(),
            store: store.clone(),
            commit_seq: commit_seq.clone(),
            events_tx: None,
            shard_chain_heads: shard_chain_heads2,
            provenance_store: None,
            commitment_tracker: Some(tracker),
        },
        EpochContext {
            epoch_manager: None,
            epoch_counter: None,
            rotation_policy: None,
            staking_snapshot_provider: None,
        },
        test_readiness2.execution_handle(),
    );

    let dead_letter = wait_for_dead_letter(&store, 0).await;
    assert_eq!(dead_letter["sequence"], 0);
    let error = dead_letter["error"]
        .as_str()
        .expect("dead-letter error should be a string");
    assert!(
        error.contains("forced commitment persistence failure"),
        "dead-letter should preserve the commitment persistence failure reason"
    );

    assert_eq!(commit_seq.load(Ordering::Acquire), u64::MAX);
    assert!(
        chain_head.get().is_none(),
        "chain head should not advance on commitment failure"
    );
    assert!(
        batch_store.get(&batch_digest_r0).is_some(),
        "failed batch should remain available while it is retried"
    );

    let receipt = wait_for_receipt(&store, &tx.digest).await;
    assert!(
        !receipt.is_empty(),
        "receipts are persisted before commitment failure"
    );

    assert_eq!(commit_seq.load(Ordering::Acquire), u64::MAX);

    bridge.abort();
    let _ = bridge.await;
    exec_handle.shutdown().await.unwrap();
}
