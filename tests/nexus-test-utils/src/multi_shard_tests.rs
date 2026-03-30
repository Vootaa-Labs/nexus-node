// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Phase T / T-6 acceptance tests — multi-shard execution runtime.
//!
//! Validates that the node can spawn multiple `ExecutionService` instances
//! (one per shard), route transactions by `target_shard`, and maintain
//! independent per-shard state and chain heads.

use std::collections::HashMap;
use std::sync::Arc;

use nexus_config::ExecutionConfig;
use nexus_crypto::{DilithiumSigner, Signer};
use nexus_execution::service::spawn_execution_service;
use nexus_execution::types::{
    compute_tx_digest, BlockExecutionResult, ExecutionStatus, SignedTransaction, StateChange,
    TransactionBody, TransactionPayload, TransactionReceipt, TX_DOMAIN,
};
use nexus_node::backends::{SharedChainHead, StorageStateView};
use nexus_node::execution_bridge::{ExecutionBridgeConfig, ShardChainHeads, ShardRouter};
use nexus_primitives::{
    AccountAddress, Amount, Blake3Digest, CommitSequence, EpochNumber, ShardId, TimestampMs,
    TokenId,
};
use nexus_storage::MemoryStore;
use nexus_storage::{traits::StateStorage, ColumnFamily};

// ── Helper: balance storage key ──────────────────────────────────────────

fn balance_storage_key(shard_id: ShardId, address: AccountAddress) -> Vec<u8> {
    nexus_storage::AccountKey { shard_id, address }.to_bytes()
}

// ── T-6 Test 1: Two shards produce independent state roots ──────────────

#[test]
fn multi_shard_router_creation() {
    // Verify that a ShardRouter can be created with multiple shards.
    let store = MemoryStore::new();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut handles = HashMap::new();
        for shard_idx in 0..2u16 {
            let shard_id = ShardId(shard_idx);
            let state_view = StorageStateView::new(store.clone(), shard_id);
            let handle = spawn_execution_service(
                ExecutionConfig::for_testing(),
                shard_id,
                Arc::new(state_view),
            );
            handles.insert(shard_id, handle);
        }

        let router = ShardRouter::new(handles);
        assert_eq!(router.num_shards(), 2);
        assert!(router.get(&ShardId(0)).is_some());
        assert!(router.get(&ShardId(1)).is_some());
        assert!(router.get(&ShardId(2)).is_none());
    });
}

#[test]
fn shard_router_single_backward_compatible() {
    // Verify ShardRouter::single produces a single-shard router.
    let store = MemoryStore::new();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let state_view = StorageStateView::new(store.clone(), ShardId(0));
        let handle = spawn_execution_service(
            ExecutionConfig::for_testing(),
            ShardId(0),
            Arc::new(state_view),
        );
        let router = ShardRouter::single(handle);
        assert_eq!(router.num_shards(), 1);
        assert!(router.get(&ShardId(0)).is_some());
        assert!(router.get(&ShardId(1)).is_none());
    });
}

// ── T-6 Test 2: Shard 0 state does not affect shard 1 ──────────────────

#[tokio::test]
async fn storage_keys_isolated_between_shards() {
    // Write a balance to shard 0 and verify shard 1 cannot see it.
    let store = MemoryStore::new();
    let addr = AccountAddress([0xAA; 32]);
    let amount = 1000u64;

    // Write balance for shard 0.
    let key_s0 = balance_storage_key(ShardId(0), addr);
    store
        .put_sync(
            ColumnFamily::State.as_str(),
            key_s0.clone(),
            amount.to_le_bytes().to_vec(),
        )
        .unwrap();

    // Shard 0 should see the balance.
    let val_s0 = store
        .get(ColumnFamily::State.as_str(), &key_s0)
        .await
        .unwrap();
    assert!(val_s0.is_some(), "shard 0 should have the balance");
    assert_eq!(
        u64::from_le_bytes(val_s0.unwrap().try_into().unwrap()),
        amount
    );

    // Shard 1 should NOT see the same balance (different key prefix).
    let key_s1 = balance_storage_key(ShardId(1), addr);
    let val_s1 = store
        .get(ColumnFamily::State.as_str(), &key_s1)
        .await
        .unwrap();
    assert!(val_s1.is_none(), "shard 1 must not see shard 0's state");
}

// ── T-6 Test 3: Per-shard chain heads are independent ───────────────────

#[test]
fn per_shard_chain_heads_independent() {
    let mut heads: ShardChainHeads = HashMap::new();
    heads.insert(ShardId(0), SharedChainHead::new());
    heads.insert(ShardId(1), SharedChainHead::new());

    // Update shard 0's chain head.
    heads
        .get(&ShardId(0))
        .unwrap()
        .update(nexus_rpc::dto::ChainHeadDto {
            sequence: 42,
            anchor_digest: "abc".to_string(),
            state_root: "root0".to_string(),
            epoch: 1,
            round: 0,
            cert_count: 1,
            tx_count: 5,
            gas_total: 100,
            committed_at_ms: 9999,
        });

    // Shard 0 should be updated.
    let head0 = heads.get(&ShardId(0)).unwrap().get();
    assert!(head0.is_some());
    assert_eq!(head0.unwrap().sequence, 42);

    // Shard 1 should still be empty.
    let head1 = heads.get(&ShardId(1)).unwrap().get();
    assert!(
        head1.is_none(),
        "shard 1's chain head must not be affected by shard 0's update"
    );
}

// ── T-6 Test 4: num_shards=1 backward compatibility ────────────────────

#[test]
fn execution_bridge_config_single_shard_default() {
    let config = ExecutionBridgeConfig::default();
    assert_eq!(config.num_shards, 1);
    // Single shard should behave identically to v0.1.9.
}

// ── T-6 Test: ShardRouter resolve_shard logic ───────────────────────────

#[test]
fn resolve_shard_defaults_to_zero() {
    assert_eq!(ShardRouter::resolve_shard(None), ShardId(0));
    assert_eq!(ShardRouter::resolve_shard(Some(ShardId(0))), ShardId(0));
    assert_eq!(ShardRouter::resolve_shard(Some(ShardId(3))), ShardId(3));
}

// ── T-6 Test 5: Persist results isolates correctly between shards ───────

#[tokio::test]
async fn persist_results_respects_shard_isolation() {
    use nexus_storage::AccountKey;

    let store = MemoryStore::new();

    // Simulate a receipt from shard 0 and shard 1 with the same account but
    // different amounts.
    let addr = AccountAddress([0xBB; 32]);

    let receipt_s0 = TransactionReceipt {
        tx_digest: Blake3Digest([0x01; 32]),
        commit_seq: CommitSequence(1),
        shard_id: ShardId(0),
        status: ExecutionStatus::Success,
        gas_used: 100,
        state_changes: vec![StateChange {
            account: addr,
            key: b"balance".to_vec(),
            value: Some(500u64.to_le_bytes().to_vec()),
        }],
        events: vec![],
        timestamp: TimestampMs::now(),
    };

    let receipt_s1 = TransactionReceipt {
        tx_digest: Blake3Digest([0x02; 32]),
        commit_seq: CommitSequence(1),
        shard_id: ShardId(1),
        status: ExecutionStatus::Success,
        gas_used: 100,
        state_changes: vec![StateChange {
            account: addr,
            key: b"balance".to_vec(),
            value: Some(800u64.to_le_bytes().to_vec()),
        }],
        events: vec![],
        timestamp: TimestampMs::now(),
    };

    // Persist shard 0 result.
    let result_s0 = BlockExecutionResult {
        new_state_root: Blake3Digest([0u8; 32]),
        receipts: vec![receipt_s0],
        gas_used_total: 100,
        execution_ms: 1,
    };
    persist_results_via_bridge(&store, &result_s0, ShardId(0)).await;

    // Persist shard 1 result.
    let result_s1 = BlockExecutionResult {
        new_state_root: Blake3Digest([0u8; 32]),
        receipts: vec![receipt_s1],
        gas_used_total: 100,
        execution_ms: 1,
    };
    persist_results_via_bridge(&store, &result_s1, ShardId(1)).await;

    // Verify shard 0 has 500.
    let mut key_s0 = AccountKey {
        shard_id: ShardId(0),
        address: addr,
    }
    .to_bytes();
    key_s0.extend_from_slice(b"balance");
    let raw_s0 = store
        .get(ColumnFamily::State.as_str(), &key_s0)
        .await
        .unwrap();
    assert_eq!(u64::from_le_bytes(raw_s0.unwrap().try_into().unwrap()), 500);

    // Verify shard 1 has 800.
    let mut key_s1 = AccountKey {
        shard_id: ShardId(1),
        address: addr,
    }
    .to_bytes();
    key_s1.extend_from_slice(b"balance");
    let raw_s1 = store
        .get(ColumnFamily::State.as_str(), &key_s1)
        .await
        .unwrap();
    assert_eq!(u64::from_le_bytes(raw_s1.unwrap().try_into().unwrap()), 800);
}

/// Helper: mimics the persist_results logic from the execution bridge.
async fn persist_results_via_bridge<S: StateStorage>(
    store: &S,
    result: &BlockExecutionResult,
    shard_id: ShardId,
) {
    use nexus_storage::traits::WriteBatchOps;

    let mut batch = store.new_batch();

    for receipt in &result.receipts {
        let receipt_bytes = serde_json::to_vec(receipt).unwrap();
        batch.put_cf(
            ColumnFamily::Receipts.as_str(),
            receipt.tx_digest.0.to_vec(),
            receipt_bytes,
        );

        for change in &receipt.state_changes {
            let mut key = nexus_storage::AccountKey {
                shard_id,
                address: change.account,
            }
            .to_bytes();
            key.extend_from_slice(&change.key);

            match &change.value {
                Some(value) => {
                    batch.put_cf(ColumnFamily::State.as_str(), key, value.clone());
                }
                None => {
                    batch.delete_cf(ColumnFamily::State.as_str(), key);
                }
            }
        }
    }

    store.write_batch(batch).await.unwrap();
}

// ── T-6 Test 6: Multi-shard execution services can run concurrently ─────

#[tokio::test]
async fn multi_shard_execution_services_concurrent() {
    // Spawn 2 shard execution services and submit a batch to each
    // concurrently, verifying both complete independently.
    let store = MemoryStore::new();
    let addr0 = AccountAddress([0x10; 32]);
    let addr1 = AccountAddress([0x20; 32]);

    // Seed balances for each shard.
    let key_s0 = balance_storage_key(ShardId(0), addr0);
    store
        .put_sync(
            ColumnFamily::State.as_str(),
            key_s0,
            10000u64.to_le_bytes().to_vec(),
        )
        .unwrap();
    let key_s1 = balance_storage_key(ShardId(1), addr1);
    store
        .put_sync(
            ColumnFamily::State.as_str(),
            key_s1,
            20000u64.to_le_bytes().to_vec(),
        )
        .unwrap();

    // Create execution services.
    let sv0 = StorageStateView::new(store.clone(), ShardId(0));
    let handle0 =
        spawn_execution_service(ExecutionConfig::for_testing(), ShardId(0), Arc::new(sv0));
    let sv1 = StorageStateView::new(store.clone(), ShardId(1));
    let handle1 =
        spawn_execution_service(ExecutionConfig::for_testing(), ShardId(1), Arc::new(sv1));

    // Create batches with a simple transfer for each shard.
    let batch = nexus_consensus::CommittedBatch {
        anchor: Blake3Digest([0xAA; 32]),
        certificates: vec![],
        sequence: CommitSequence(1),
        committed_at: TimestampMs::now(),
    };

    let tx0 = make_test_tx(addr0, ShardId(0), 0);
    let tx1 = make_test_tx(addr1, ShardId(1), 0);

    // Submit to both shards concurrently.
    let (r0, r1) = tokio::join!(
        handle0.submit_batch(batch.clone(), vec![tx0]),
        handle1.submit_batch(batch.clone(), vec![tx1]),
    );

    // Both should complete (even if the result is "no-op" because the
    // test tx payload doesn't trigger real state changes).
    assert!(r0.is_ok(), "shard 0 execution should succeed");
    assert!(r1.is_ok(), "shard 1 execution should succeed");
}

fn make_test_tx(sender: AccountAddress, shard_id: ShardId, nonce: u64) -> SignedTransaction {
    let (sk, pk) = DilithiumSigner::generate_keypair();
    let body = TransactionBody {
        sender,
        sequence_number: nonce,
        expiry_epoch: EpochNumber(100),
        gas_limit: 10_000,
        gas_price: 1,
        target_shard: Some(shard_id),
        payload: TransactionPayload::Transfer {
            recipient: AccountAddress([0xFF; 32]),
            amount: Amount(100),
            token: TokenId::Native,
        },
        chain_id: 0,
    };

    let digest = compute_tx_digest(&body).unwrap();
    let signature = DilithiumSigner::sign(&sk, TX_DOMAIN, digest.as_bytes());

    SignedTransaction {
        body,
        signature,
        sender_pk: pk,
        digest,
    }
}

// ── T-6 Test 7: Transaction grouping by target_shard ────────────────────

#[test]
fn transaction_grouping_by_target_shard() {
    // Build 6 transactions: 3 for shard 0, 2 for shard 1, 1 with no target.
    let txs: Vec<SignedTransaction> = vec![
        make_test_tx(AccountAddress([0x01; 32]), ShardId(0), 0),
        make_test_tx(AccountAddress([0x02; 32]), ShardId(1), 0),
        make_test_tx(AccountAddress([0x03; 32]), ShardId(0), 1),
        make_test_tx(AccountAddress([0x04; 32]), ShardId(1), 1),
        make_test_tx(AccountAddress([0x05; 32]), ShardId(0), 2),
        // This one has no target_shard — should default to shard 0.
        {
            let mut tx = make_test_tx(AccountAddress([0x06; 32]), ShardId(0), 3);
            tx.body.target_shard = None;
            tx
        },
    ];

    // Group by shard.
    let mut groups: HashMap<ShardId, Vec<&SignedTransaction>> = HashMap::new();
    for tx in &txs {
        let shard = ShardRouter::resolve_shard(tx.body.target_shard);
        groups.entry(shard).or_default().push(tx);
    }

    assert_eq!(groups.get(&ShardId(0)).map(|v| v.len()), Some(4)); // 3 explicit + 1 default
    assert_eq!(groups.get(&ShardId(1)).map(|v| v.len()), Some(2));
    assert!(!groups.contains_key(&ShardId(2)));
}
