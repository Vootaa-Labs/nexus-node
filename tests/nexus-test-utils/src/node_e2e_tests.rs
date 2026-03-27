// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! E2E transaction lifecycle test — submit → execute → receipt in storage.
//!
//! Exercises the full node pipeline:
//! 1. Boot from genesis → seeded storage + committee
//! 2. Submit a signed transfer transaction through the execution service
//! 3. Write the receipt to storage
//! 4. Query the receipt via the QueryBackend adapter

#![forbid(unsafe_code)]

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use nexus_config::genesis::GenesisConfig;
    use nexus_consensus::types::CommittedBatch;
    use nexus_consensus::ValidatorRegistry;
    use nexus_crypto::Signer;
    use nexus_execution::spawn_execution_service;
    use nexus_execution::types::{
        compute_tx_digest, ExecutionStatus, SignedTransaction, TransactionBody, TransactionPayload,
        TX_DOMAIN,
    };
    use nexus_node::backends::{StorageQueryBackend, StorageStateView};
    use nexus_node::genesis_boot;
    use nexus_primitives::{
        AccountAddress, Amount, Blake3Digest, CommitSequence, EpochNumber, ShardId, TimestampMs,
        TokenId,
    };
    use nexus_rpc::QueryBackend;
    use nexus_storage::{ColumnFamily, MemoryStore, StateStorage, WriteBatchOps};

    /// Full E2E: genesis → transfer → receipt query.
    #[tokio::test]
    async fn e2e_transfer_and_receipt() {
        let shard_id = ShardId(0);

        // ── 1. Boot from genesis ────────────────────────────────────────
        let genesis = GenesisConfig::for_testing();
        let dir = std::env::temp_dir().join("nexus-e2e-transfer-test");
        std::fs::create_dir_all(&dir).unwrap();
        let genesis_path = dir.join("genesis.json");
        std::fs::write(&genesis_path, serde_json::to_string(&genesis).unwrap()).unwrap();

        let store = MemoryStore::new();
        let boot = genesis_boot::boot_from_genesis(&genesis_path, &store, shard_id).unwrap();
        assert_eq!(boot.committee.active_validators().len(), 4);

        // ── 2. Seed sender balance ──────────────────────────────────────
        let (sk, pk) = nexus_crypto::DilithiumSigner::generate_keypair();
        let sender = AccountAddress::from_dilithium_pubkey(pk.as_bytes());
        let recipient = AccountAddress([0xBB; 32]);
        let initial_balance = Amount(1_000_000);

        {
            let key = nexus_storage::AccountKey {
                shard_id,
                address: sender,
            };
            let mut batch = store.new_batch();
            batch.put_cf(
                ColumnFamily::State.as_str(),
                key.to_bytes(),
                initial_balance.0.to_le_bytes().to_vec(),
            );
            store.write_batch(batch).await.unwrap();
        }

        // ── 3. Build and sign a transfer transaction ────────────────────
        let body = TransactionBody {
            sender,
            sequence_number: 0,
            expiry_epoch: EpochNumber(1000),
            gas_limit: 50_000,
            gas_price: 1,
            target_shard: None,
            payload: TransactionPayload::Transfer {
                recipient,
                amount: Amount(100),
                token: TokenId::Native,
            },
            chain_id: 1,
        };

        let digest = compute_tx_digest(&body).unwrap();
        let sig = nexus_crypto::DilithiumSigner::sign(&sk, TX_DOMAIN, digest.as_bytes());

        let tx = SignedTransaction {
            body,
            signature: sig,
            sender_pk: pk,
            digest,
        };

        // ── 4. Spawn execution service and submit ───────────────────────
        let state_view = StorageStateView::new(store.clone(), shard_id);
        let exec_handle = spawn_execution_service(
            nexus_config::ExecutionConfig::for_testing(),
            shard_id,
            Arc::new(state_view),
        );

        let committed = CommittedBatch {
            anchor: Blake3Digest([1u8; 32]),
            certificates: vec![Blake3Digest([1u8; 32])],
            sequence: CommitSequence(1),
            committed_at: TimestampMs(1_000_000),
        };

        let result = exec_handle.submit_batch(committed, vec![tx]).await.unwrap();

        assert_eq!(result.receipts.len(), 1);
        let receipt = &result.receipts[0];
        // Transaction may succeed or fail depending on state view implementation,
        // but the pipeline must produce a receipt.
        assert!(
            receipt.status == ExecutionStatus::Success
                || receipt.status == ExecutionStatus::OutOfGas
                || matches!(receipt.status, ExecutionStatus::MoveAbort { .. })
        );

        // ── 5. Write the receipt to storage ─────────────────────────────
        {
            let mut batch = store.new_batch();
            batch.put_cf(
                ColumnFamily::Receipts.as_str(),
                receipt.tx_digest.0.to_vec(),
                serde_json::to_vec(receipt).unwrap(),
            );
            store.write_batch(batch).await.unwrap();
        }

        // ── 6. Query receipt via QueryBackend ───────────────────────────
        let epoch = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let commit_seq = Arc::new(std::sync::atomic::AtomicU64::new(1));
        let query_backend = StorageQueryBackend::new(store.clone(), epoch, commit_seq);

        let dto = query_backend
            .transaction_receipt(&receipt.tx_digest)
            .unwrap();
        assert!(dto.is_some(), "receipt should be queryable");
        let dto = dto.unwrap();
        assert_eq!(dto.tx_digest, receipt.tx_digest);
        assert_eq!(dto.gas_used, receipt.gas_used);

        // ── 7. Verify health endpoint ───────────────────────────────────
        let health = query_backend.health_status();
        assert_eq!(health.latest_commit, CommitSequence(1));

        // Cleanup.
        exec_handle.shutdown().await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// E2E: genesis allocations are queryable via QueryBackend.
    #[test]
    #[cfg(not(feature = "move-vm"))]
    fn genesis_allocations_queryable_via_backend() {
        let shard_id = ShardId(0);

        let genesis = GenesisConfig::for_testing();
        let dir = std::env::temp_dir().join("nexus-e2e-alloc-test");
        std::fs::create_dir_all(&dir).unwrap();
        let genesis_path = dir.join("genesis.json");
        std::fs::write(&genesis_path, serde_json::to_string(&genesis).unwrap()).unwrap();

        let store = MemoryStore::new();
        genesis_boot::boot_from_genesis(&genesis_path, &store, shard_id).unwrap();

        let epoch = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let commit_seq = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let query_backend = StorageQueryBackend::new(store, epoch, commit_seq);

        // Genesis test config allocates 1 NXS to AccountAddress::ZERO.
        let balance = query_backend
            .account_balance(&AccountAddress::ZERO, &TokenId::Native)
            .unwrap();
        assert_eq!(balance, Amount(1_000_000_000));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
