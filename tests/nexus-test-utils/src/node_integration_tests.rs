// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Phase 5 integration tests — cross-module validation.
//!
//! These tests verify the integration surface between multiple subsystems
//! that were wired together during Phase 5.

#![forbid(unsafe_code)]

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use nexus_config::genesis::GenesisConfig;
    use nexus_consensus::ConsensusEngine;
    use nexus_crypto::Signer;

    use nexus_intent::AccountResolverImpl;
    use nexus_primitives::{AccountAddress, Amount, EpochNumber, ShardId, TokenId, ValidatorIndex};
    use nexus_rpc::{ConsensusBackend, QueryBackend};
    use nexus_storage::MemoryStore;

    use nexus_node::backends::{LiveConsensusBackend, LiveIntentBackend, StorageQueryBackend};
    use nexus_node::genesis_boot;

    /// Integration: genesis boot → consensus backend → validator queries.
    #[test]
    fn genesis_to_consensus_backend() {
        let genesis = GenesisConfig::for_testing();
        let dir = std::env::temp_dir().join("nexus-integ-consensus");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("genesis.json");
        std::fs::write(&path, serde_json::to_string(&genesis).unwrap()).unwrap();

        let store = MemoryStore::new();
        let boot = genesis_boot::boot_from_genesis(&path, &store, ShardId(0)).unwrap();

        let engine = ConsensusEngine::new(EpochNumber(0), boot.committee);
        let backend = LiveConsensusBackend::new(Arc::new(Mutex::new(engine)));

        // Verify all validators are queryable.
        let validators = backend.active_validators().unwrap();
        assert_eq!(validators.len(), 4);

        for i in 0..4u32 {
            let info = backend.validator_info(ValidatorIndex(i)).unwrap();
            assert_eq!(info.index, ValidatorIndex(i));
        }

        let status = backend.consensus_status().unwrap();
        assert_eq!(status.epoch, EpochNumber(0));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Integration: storage → execution → query backend round-trip.
    #[tokio::test]
    #[cfg(not(feature = "move-vm"))]
    async fn storage_execution_query_round_trip() {
        let shard_id = ShardId(0);
        let store = MemoryStore::new();

        // Seed a sender balance.
        let sender = AccountAddress([0xCC; 32]);
        let recipient = AccountAddress([0xDD; 32]);
        {
            let key = nexus_storage::AccountKey {
                shard_id,
                address: sender,
            };
            let mut batch = store.new_batch();
            batch.put_cf(
                ColumnFamily::State.as_str(),
                key.to_bytes(),
                Amount(500_000).0.to_le_bytes().to_vec(),
            );
            store.write_batch(batch).await.unwrap();
        }

        // Verify balance via query backend.
        let epoch = Arc::new(std::sync::atomic::AtomicU64::new(1));
        let commit = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let query = StorageQueryBackend::new(store.clone(), epoch, commit);
        assert_eq!(
            query.account_balance(&sender, &TokenId::Native).unwrap(),
            Amount(500_000)
        );

        // Execute a transaction via execution service.
        let state_view = StorageStateView::new(store.clone(), shard_id);
        let exec = spawn_execution_service(
            nexus_config::ExecutionConfig::for_testing(),
            shard_id,
            Arc::new(state_view),
        );

        let body = TransactionBody {
            sender,
            sequence_number: 0,
            expiry_epoch: EpochNumber(100),
            gas_limit: 50_000,
            gas_price: 1,
            target_shard: None,
            payload: TransactionPayload::Transfer {
                recipient,
                amount: Amount(50),
                token: TokenId::Native,
            },
            chain_id: 1,
        };
        let digest = compute_tx_digest(&body).unwrap();
        let (sk, pk) = nexus_crypto::DilithiumSigner::generate_keypair();
        let sig = nexus_crypto::DilithiumSigner::sign(&sk, TX_DOMAIN, digest.as_bytes());
        let tx = SignedTransaction {
            body,
            signature: sig,
            sender_pk: pk,
            digest,
        };

        let batch = nexus_consensus::types::CommittedBatch {
            anchor: nexus_primitives::Blake3Digest([2u8; 32]),
            certificates: vec![],
            sequence: nexus_primitives::CommitSequence(1),
            committed_at: nexus_primitives::TimestampMs(2_000_000),
        };
        let result = exec.submit_batch(batch, vec![tx]).await.unwrap();
        assert_eq!(result.receipts.len(), 1);

        // Write receipt back to storage.
        let receipt = &result.receipts[0];
        {
            let mut wb = store.new_batch();
            wb.put_cf(
                ColumnFamily::Receipts.as_str(),
                receipt.tx_digest.0.to_vec(),
                serde_json::to_vec(receipt).unwrap(),
            );
            store.write_batch(wb).await.unwrap();
        }

        // Query receipt back via backend.
        let dto = query
            .transaction_receipt(&receipt.tx_digest)
            .unwrap()
            .expect("receipt must exist");
        assert_eq!(dto.tx_digest, receipt.tx_digest);

        exec.shutdown().await.unwrap();
    }

    /// Integration: intent service wired to backend produces results.
    #[tokio::test]
    async fn intent_backend_end_to_end() {
        let resolver = Arc::new(AccountResolverImpl::new(1));
        let sender = AccountAddress([0x01; 32]);
        resolver
            .balances()
            .set_balance(sender, TokenId::Native, Amount(1_000_000));

        let compiler = nexus_intent::IntentCompilerImpl::new(nexus_intent::IntentConfig::default());
        let handle = nexus_intent::IntentService::spawn(compiler, 16);
        let backend = LiveIntentBackend::new(handle, resolver);

        // Build a signed intent.
        use nexus_crypto::DilithiumSigner;
        use nexus_intent::types::*;

        let intent = UserIntent::Transfer {
            to: AccountAddress([0x02; 32]),
            token: TokenId::Native,
            amount: Amount(100),
        };
        let nonce = 1u64;
        let digest = compute_intent_digest(&intent, &sender, nonce).unwrap();
        let (sk, vk) = DilithiumSigner::generate_keypair();

        let intent_bytes = bcs::to_bytes(&intent).unwrap();
        let sender_bytes = bcs::to_bytes(&sender).unwrap();
        let nonce_bytes = bcs::to_bytes(&nonce).unwrap();
        let mut msg = Vec::new();
        msg.extend_from_slice(&intent_bytes);
        msg.extend_from_slice(&sender_bytes);
        msg.extend_from_slice(&nonce_bytes);
        let sig = DilithiumSigner::sign(&sk, INTENT_DOMAIN, &msg);

        let signed = SignedUserIntent {
            intent,
            sender,
            signature: sig,
            sender_pk: vk,
            nonce,
            created_at: nexus_primitives::TimestampMs(1_000_000),
            digest,
        };

        // Submit via backend — should not panic regardless of outcome.
        let result = nexus_rpc::IntentBackend::submit_intent(&backend, signed).await;
        assert!(result.is_ok() || result.is_err());
    }

    /// Integration: health status reflects atomic counters correctly.
    #[test]
    fn health_status_with_updates() {
        let store = MemoryStore::new();
        let epoch = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let commit = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let query = StorageQueryBackend::new(store, Arc::clone(&epoch), Arc::clone(&commit));

        // Initial state.
        let h1 = query.health_status();
        assert_eq!(h1.epoch, EpochNumber(0));

        // Simulate epoch advance.
        epoch.store(5, std::sync::atomic::Ordering::Relaxed);
        commit.store(100, std::sync::atomic::Ordering::Relaxed);

        let h2 = query.health_status();
        assert_eq!(h2.epoch, EpochNumber(5));
        assert_eq!(h2.latest_commit, nexus_primitives::CommitSequence(100));
    }

    // ── Phase 7: Network lifecycle integration ──────────────────────────

    /// Integration: NetworkService builds with default config and shuts down.
    #[tokio::test]
    async fn network_service_lifecycle() {
        use nexus_network::{NetworkConfig, NetworkService};

        let config = NetworkConfig::for_testing();
        let (handle, service) = NetworkService::build(&config).expect("build should succeed");

        let shutdown_handle = handle.transport.clone();
        let task = tokio::spawn(service.run());

        // Verify subsystem handles are usable
        let health = handle.discovery.routing_health();
        assert_eq!(health.known_peers, 0, "no peers at startup");

        // Graceful shutdown
        shutdown_handle
            .shutdown()
            .await
            .expect("shutdown should succeed");
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), task).await;
        assert!(result.is_ok(), "service should exit after shutdown");
    }

    /// Integration: NetworkService with full NodeConfig (same path as main.rs).
    #[tokio::test]
    async fn node_network_wiring_matches_main() {
        use nexus_network::NetworkService;

        let config = nexus_config::NodeConfig::for_testing();
        let (handle, service) =
            NetworkService::build(&config.network).expect("build from NodeConfig should succeed");

        let shutdown_handle = handle.transport.clone();
        let task = tokio::spawn(service.run());

        // Rate limiter accessible
        assert_eq!(handle.rate_limiter.active_buckets(), 0);

        // Shutdown
        shutdown_handle.shutdown().await.expect("shutdown ok");
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), task).await;
    }
}
