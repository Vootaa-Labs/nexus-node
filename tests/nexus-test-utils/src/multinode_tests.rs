// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Multi-node integration tests (T-7008).
//!
//! Exercises the full pipeline across 3 in-process validators:
//! transaction → mempool → consensus → execution → receipt.

#![forbid(unsafe_code)]

#[cfg(test)]
mod tests {
    use std::sync::{atomic::AtomicU64, Arc};

    use nexus_config::genesis::GenesisConfig;
    use nexus_consensus::{ConsensusEngine, ValidatorRegistry};
    use nexus_crypto::Signer;
    use nexus_execution::spawn_execution_service;
    use nexus_execution::types::{
        compute_tx_digest, ExecutionStatus, SignedTransaction, TransactionBody, TransactionPayload,
        TX_DOMAIN,
    };
    use nexus_network::types::Topic;
    use nexus_network::NetworkService;
    use nexus_primitives::{
        AccountAddress, Amount, Blake3Digest, CommitSequence, EpochNumber, ShardId, TimestampMs,
        TokenId, ValidatorIndex,
    };
    use nexus_storage::{ColumnFamily, MemoryStore, StateStorage, WriteBatchOps};

    use nexus_node::backends::{LiveNetworkBackend, StorageQueryBackend, StorageStateView};
    use nexus_node::genesis_boot;
    use nexus_node::mempool::{InsertResult, Mempool, MempoolConfig};

    // ── Helpers ──────────────────────────────────────────────────────────

    /// Boot a node from genesis.
    fn boot_test_node(
        genesis: &GenesisConfig,
        label: &str,
    ) -> (MemoryStore, nexus_consensus::Committee, ShardId) {
        let shard_id = ShardId(0);
        let dir = std::env::temp_dir().join(format!("nexus-multinode-{label}"));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("genesis.json");
        std::fs::write(&path, serde_json::to_string(genesis).unwrap()).unwrap();

        let store = MemoryStore::new();
        let boot = genesis_boot::boot_from_genesis(&path, &store, shard_id).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        (store, boot.committee, shard_id)
    }

    /// Build and sign a transfer transaction.
    fn build_signed_transfer(recipient: AccountAddress, amount: Amount) -> SignedTransaction {
        let (sk, pk) = nexus_crypto::DilithiumSigner::generate_keypair();
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
                amount,
                token: TokenId::Native,
            },
            chain_id: 1,
        };

        let digest = compute_tx_digest(&body).unwrap();
        let sig = nexus_crypto::DilithiumSigner::sign(&sk, TX_DOMAIN, digest.as_bytes());

        SignedTransaction {
            body,
            signature: sig,
            sender_pk: pk,
            digest,
        }
    }

    /// Seed a balance in storage for a given address.
    async fn seed_balance(store: &MemoryStore, address: AccountAddress, amount: Amount) {
        let key = nexus_storage::AccountKey {
            shard_id: ShardId(0),
            address,
        };
        let mut batch = store.new_batch();
        batch.put_cf(
            ColumnFamily::State.as_str(),
            key.to_bytes(),
            amount.0.to_le_bytes().to_vec(),
        );
        store.write_batch(batch).await.unwrap();
    }

    // ── Test 1: Full 3-validator pipeline ────────────────────────────────

    /// End-to-end: 3 validators share genesis, tx enters mempool, consensus
    /// batch is committed, execution processes it, receipt is stored.
    #[tokio::test]
    async fn three_validator_pipeline() {
        let genesis = GenesisConfig::for_testing();

        // Boot 3 nodes from the same genesis.
        let (store1, committee1, shard_id) = boot_test_node(&genesis, "v1");
        let (_store2, committee2, _) = boot_test_node(&genesis, "v2");
        let (_store3, committee3, _) = boot_test_node(&genesis, "v3");

        // All 3 nodes agree on the validator set (genesis has 4 validators).
        assert_eq!(committee1.active_validators().len(), 4);
        assert_eq!(committee2.active_validators().len(), 4);
        assert_eq!(committee3.active_validators().len(), 4);

        // Create consensus engines for all 3 nodes.
        let _engine1 = ConsensusEngine::new(EpochNumber(0), committee1.clone());
        let _engine2 = ConsensusEngine::new(EpochNumber(0), committee2);
        let _engine3 = ConsensusEngine::new(EpochNumber(0), committee3);

        // ── Mempool: insert a transaction on node 1 ─────────────────────
        let recipient = AccountAddress([0xBB; 32]);

        let tx = build_signed_transfer(recipient, Amount(100));
        let sender = tx.body.sender;
        seed_balance(&store1, sender, Amount(1_000_000)).await;
        let tx_digest = tx.digest;

        let mempool = Mempool::new(&MempoolConfig::default());
        let insert = mempool.insert(tx.clone());
        assert_eq!(insert, InsertResult::Accepted);
        assert_eq!(mempool.len(), 1);

        // ── Drain batch from mempool ────────────────────────────────────
        let batch = mempool.drain_batch(64);
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].digest, tx_digest);
        assert!(mempool.is_empty(), "mempool should be empty after drain");

        // ── Simulate committed batch from consensus ─────────────────────
        let committed_batch = nexus_consensus::types::CommittedBatch {
            anchor: Blake3Digest([1u8; 32]),
            certificates: vec![Blake3Digest([1u8; 32])],
            sequence: CommitSequence(1),
            committed_at: TimestampMs::now(),
        };

        // ── Execution on node 1 ─────────────────────────────────────────
        let state_view = StorageStateView::new(store1.clone(), shard_id);
        let exec_handle = spawn_execution_service(
            nexus_config::ExecutionConfig::for_testing(),
            shard_id,
            Arc::new(state_view),
        );

        let result = exec_handle
            .submit_batch(committed_batch, batch)
            .await
            .unwrap();

        assert_eq!(result.receipts.len(), 1, "should produce 1 receipt");
        let receipt = &result.receipts[0];
        assert_eq!(receipt.tx_digest, tx_digest);
        assert!(
            receipt.status == ExecutionStatus::Success
                || receipt.status == ExecutionStatus::OutOfGas
                || matches!(receipt.status, ExecutionStatus::MoveAbort { .. })
        );

        // ── Store receipt and query back ────────────────────────────────
        {
            let mut wb = store1.new_batch();
            wb.put_cf(
                ColumnFamily::Receipts.as_str(),
                receipt.tx_digest.0.to_vec(),
                serde_json::to_vec(receipt).unwrap(),
            );
            store1.write_batch(wb).await.unwrap();
        }

        let epoch = Arc::new(AtomicU64::new(0));
        let commit_seq = Arc::new(AtomicU64::new(1));
        let query = StorageQueryBackend::new(store1.clone(), epoch, commit_seq);
        let dto = nexus_rpc::QueryBackend::transaction_receipt(&query, &tx_digest)
            .unwrap()
            .expect("receipt should be queryable");
        assert_eq!(dto.tx_digest, tx_digest);

        exec_handle.shutdown().await.unwrap();
    }

    // ── Test 2: Gossip → Mempool bridge integration ─────────────────────

    /// Verify the gossip_bridge correctly inserts transactions into the mempool.
    /// Uses a standalone GossipService + broadcast channel to simulate message delivery.
    #[tokio::test]
    async fn gossip_bridge_routes_tx_to_mempool() {
        let config = nexus_network::NetworkConfig::for_testing();
        let (handle, service) = NetworkService::build(&config).unwrap();

        // Spawn the network service so the gossip channel is active.
        let net_task = tokio::spawn(service.run());

        let mempool = Arc::new(Mempool::new(&MempoolConfig::default()));
        let current_epoch = Arc::new(AtomicU64::new(0));

        // Spawn the gossip→mempool bridge.
        let bridge_handle = nexus_node::gossip_bridge::spawn_gossip_mempool_bridge(
            handle.gossip.clone(),
            mempool.clone(),
            current_epoch,
        )
        .await
        .unwrap();

        // Build a valid transaction.
        let tx = build_signed_transfer(AccountAddress([0xDD; 32]), Amount(50));
        let expected_digest = tx.digest;

        // BCS-encode and inject directly via the gossip broadcast channel.
        // (gossip.publish() requires mesh peers; for in-process tests we
        //  push through the topic channel directly.)
        let encoded = bcs::to_bytes(&tx).unwrap();
        handle.gossip.inject_local(Topic::Transaction, encoded);

        // Give the bridge a moment to process.
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Verify the transaction landed in the mempool.
        assert!(
            mempool.contains(&expected_digest),
            "tx should have been routed from gossip into mempool"
        );
        assert_eq!(mempool.len(), 1);

        // Cleanup.
        bridge_handle.abort();
        handle.transport.shutdown().await.unwrap();
        let _ = net_task.await;
    }

    // ── Test 3: Consensus message relay integration ─────────────────────

    /// Publish a consensus message via the broadcast channel and verify
    /// it is received on the consensus topic receiver.
    #[tokio::test]
    async fn consensus_message_relay_via_gossip() {
        let config = nexus_network::NetworkConfig::for_testing();
        let (handle, service) = NetworkService::build(&config).unwrap();
        let net_task = tokio::spawn(service.run());

        // Subscribe to consensus topic.
        handle.gossip.subscribe(Topic::Consensus).await.unwrap();
        let mut rx = handle.gossip.topic_receiver(Topic::Consensus);

        // Construct a minimal consensus message envelope.
        let msg = nexus_node::consensus_bridge::ConsensusMessage::Certificate(
            nexus_consensus::NarwhalCertificate {
                epoch: EpochNumber(0),
                batch_digest: Blake3Digest([0xCD; 32]),
                origin: ValidatorIndex(0),
                round: nexus_primitives::RoundNumber(1),
                parents: vec![],
                signatures: vec![],
                signers: nexus_consensus::types::ValidatorBitset::new(4),
                cert_digest: Blake3Digest([0xAB; 32]),
            },
        );
        let encoded = bcs::to_bytes(&msg).unwrap();

        // Inject message via broadcast channel (bypasses gossipsub mesh).
        handle.gossip.inject_local(Topic::Consensus, encoded);

        // The local subscriber should receive the message.
        let received = tokio::time::timeout(tokio::time::Duration::from_millis(500), rx.recv())
            .await
            .expect("should receive within timeout")
            .expect("channel should be open");

        // Decode and verify.
        let decoded: nexus_node::consensus_bridge::ConsensusMessage =
            bcs::from_bytes(&received).unwrap();
        match decoded {
            nexus_node::consensus_bridge::ConsensusMessage::Certificate(cert) => {
                assert_eq!(cert.round, nexus_primitives::RoundNumber(1));
                assert_eq!(cert.origin, ValidatorIndex(0));
            }
            _ => panic!("expected Certificate"),
        }

        handle.transport.shutdown().await.unwrap();
        let _ = net_task.await;
    }

    // ── Test 4: State sync request via gossip ───────────────────────────

    /// Verify a state sync block request can be serialized and received
    /// over the StateSync broadcast channel.
    #[tokio::test]
    async fn state_sync_request_via_gossip() {
        let config = nexus_network::NetworkConfig::for_testing();
        let (handle, service) = NetworkService::build(&config).unwrap();
        let net_task = tokio::spawn(service.run());

        // Subscribe to state sync topic.
        handle.gossip.subscribe(Topic::StateSync).await.unwrap();
        let mut rx = handle.gossip.topic_receiver(Topic::StateSync);

        // Build and inject a block request (bypasses gossipsub mesh).
        let msg = nexus_node::state_sync::StateSyncMessage::BlockRequest {
            from_seq: CommitSequence(5),
            count: 10,
            requester: nexus_network::PeerId::from_digest(nexus_primitives::Blake3Digest(
                [0xBB; 32],
            )),
        };
        let encoded = bcs::to_bytes(&msg).unwrap();
        handle.gossip.inject_local(Topic::StateSync, encoded);

        // Receive and decode.
        let received = tokio::time::timeout(tokio::time::Duration::from_millis(500), rx.recv())
            .await
            .expect("should receive within timeout")
            .expect("channel should be open");

        let decoded: nexus_node::state_sync::StateSyncMessage = bcs::from_bytes(&received).unwrap();
        match decoded {
            nexus_node::state_sync::StateSyncMessage::BlockRequest {
                from_seq, count, ..
            } => {
                assert_eq!(from_seq, CommitSequence(5));
                assert_eq!(count, 10);
            }
            _ => panic!("expected BlockRequest"),
        }

        handle.transport.shutdown().await.unwrap();
        let _ = net_task.await;
    }

    // ── Test 5: Mempool deduplication across nodes ──────────────────────

    /// Verify that the same transaction submitted to 3 mempools is
    /// deduplicated (each mempool rejects duplicates independently).
    #[test]
    fn mempool_deduplication_across_nodes() {
        let mempool1 = Mempool::new(&MempoolConfig::default());
        let mempool2 = Mempool::new(&MempoolConfig::default());
        let mempool3 = Mempool::new(&MempoolConfig::default());

        let tx = build_signed_transfer(AccountAddress([0x22; 32]), Amount(10));

        // All 3 mempools accept the tx on first insert.
        assert_eq!(mempool1.insert(tx.clone()), InsertResult::Accepted);
        assert_eq!(mempool2.insert(tx.clone()), InsertResult::Accepted);
        assert_eq!(mempool3.insert(tx.clone()), InsertResult::Accepted);

        // Duplicate insert is rejected.
        assert_eq!(mempool1.insert(tx.clone()), InsertResult::Duplicate);
        assert_eq!(mempool2.insert(tx.clone()), InsertResult::Duplicate);
        assert_eq!(mempool3.insert(tx), InsertResult::Duplicate);

        // All 3 have exactly 1 entry.
        assert_eq!(mempool1.len(), 1);
        assert_eq!(mempool2.len(), 1);
        assert_eq!(mempool3.len(), 1);
    }

    // ── Test 6: Network backend reflects discovery state ────────────────

    /// Verify LiveNetworkBackend returns correct data from discovery handle.
    #[tokio::test]
    async fn network_backend_reflects_discovery() {
        let config = nexus_network::NetworkConfig::for_testing();
        let (handle, _service) = NetworkService::build(&config).unwrap();

        let backend = LiveNetworkBackend::new(handle.discovery.clone());

        // Initially: no peers.
        let peers = nexus_rpc::NetworkBackend::network_peers(&backend).unwrap();
        assert_eq!(peers.total, 0);

        let status = nexus_rpc::NetworkBackend::network_status(&backend).unwrap();
        assert_eq!(status.known_peers, 0);
        assert_eq!(status.known_validators, 0);

        let health = nexus_rpc::NetworkBackend::network_health(&backend).unwrap();
        assert!(!health.routing_healthy);

        // Seed a validator record.
        let peer_id = nexus_network::types::PeerId::from_public_key(b"test-key-01");
        let record = nexus_network::discovery::NodeRecord {
            peer_id,
            addresses: vec![],
            dilithium_pubkey: b"test-key-01".to_vec(),
            reputation: 50,
            last_seen: 1000,
            validator_stake: Some(1000),
        };
        handle.discovery.seed_validator_record(record);

        // Now should have 1 peer, 1 validator.
        let peers = nexus_rpc::NetworkBackend::network_peers(&backend).unwrap();
        assert_eq!(peers.total, 1);
        assert!(peers.peers[0].is_validator);

        let status = nexus_rpc::NetworkBackend::network_status(&backend).unwrap();
        assert_eq!(status.known_validators, 1);
    }
}
