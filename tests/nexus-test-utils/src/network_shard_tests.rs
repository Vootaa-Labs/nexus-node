// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! W-7 — Network shard integration tests.
//!
//! Acceptance criteria:
//! 1. Multi-shard config: node only receives own-shard gossip messages.
//! 2. Committee/epoch change auto-updates topic subscriptions.
//! 3. State sync rejects blocks with invalid committee signatures.
//! 4. RPC endpoints return correct shard topology and HTLC status.

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    // Test 1: Gossip shard isolation
    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

    /// With 2 shards, messages injected into shard-0's broadcast channel
    /// arrive only on the shard-0 receiver, and vice-versa for shard-1.
    #[tokio::test]
    async fn gossip_shard_isolation_only_own_shard() {
        use nexus_network::gossip::GossipService;
        use nexus_network::{NetworkConfig, Topic};

        let config = NetworkConfig::for_testing();
        let (handle, _service) = GossipService::new_with_shards(&config, 2);

        let mut rx0 = handle.topic_receiver(Topic::ShardedTransaction(0));
        let mut rx1 = handle.topic_receiver(Topic::ShardedTransaction(1));

        handle.inject_local(Topic::ShardedTransaction(0), b"shard0_tx".to_vec());
        handle.inject_local(Topic::ShardedTransaction(1), b"shard1_tx".to_vec());

        let msg0 = rx0.try_recv().expect("shard 0 should receive its message");
        assert_eq!(msg0, b"shard0_tx");

        let msg1 = rx1.try_recv().expect("shard 1 should receive its message");
        assert_eq!(msg1, b"shard1_tx");

        // No cross-shard leakage
        assert!(
            rx0.try_recv().is_err(),
            "shard 0 receiver must not see shard 1 messages"
        );
        assert!(
            rx1.try_recv().is_err(),
            "shard 1 receiver must not see shard 0 messages"
        );
    }

    /// Topic strings for different shards are distinct, preventing
    /// accidental cross-shard routing.
    #[test]
    fn shard_topic_strings_are_distinct() {
        use nexus_network::Topic;

        let t0 = Topic::ShardedTransaction(0);
        let t1 = Topic::ShardedTransaction(1);
        let c0 = Topic::ShardedCertificate(0);
        let c1 = Topic::ShardedCertificate(1);

        assert_ne!(t0.topic_string(), t1.topic_string());
        assert_ne!(c0.topic_string(), c1.topic_string());
        assert_ne!(t0.topic_string(), c0.topic_string());

        assert_eq!(t0.shard_id(), Some(0));
        assert_eq!(t1.shard_id(), Some(1));
        assert_eq!(c0.shard_id(), Some(0));
        assert!(t0.is_sharded());
        assert!(!Topic::Transaction.is_sharded());
    }

    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    // Test 2: Epoch change auto-updates subscriptions
    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

    /// When an epoch change assigns new shards, the bridge subscribes to
    /// the new shard topics (verify via publish/receive on handle).
    #[tokio::test]
    async fn epoch_change_updates_shard_subscriptions() {
        use nexus_network::{NetworkConfig, NetworkService, Topic};
        use nexus_node::epoch_network_bridge::{
            epoch_change_channel, spawn_epoch_network_bridge, EpochChangeEvent,
        };

        let config = NetworkConfig::for_testing();
        let (net_handle, service) = NetworkService::build(&config).expect("build");
        let shutdown = net_handle.transport.clone();
        let net_task = tokio::spawn(service.run());

        let (epoch_tx, epoch_rx) = epoch_change_channel();
        let bridge = spawn_epoch_network_bridge(net_handle.gossip.clone(), epoch_rx, None);

        // Epoch 1: assigned shards [0, 1]
        epoch_tx
            .send(EpochChangeEvent {
                epoch: nexus_primitives::EpochNumber(1),
                num_shards: 4,
                assigned_shards: vec![0, 1],
            })
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(80)).await;

        // Shard 0 should be subscribed after the bridge processed the event
        let mut rx0 = net_handle
            .gossip
            .topic_receiver(Topic::ShardedTransaction(0));
        net_handle
            .gossip
            .inject_local(Topic::ShardedTransaction(0), b"s0".to_vec());
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(rx0.try_recv().is_ok(), "shard 0 should be subscribed");

        // Epoch 2: reassigned to shards [2, 3]
        epoch_tx
            .send(EpochChangeEvent {
                epoch: nexus_primitives::EpochNumber(2),
                num_shards: 4,
                assigned_shards: vec![2, 3],
            })
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(80)).await;

        // Shard 2 should now be subscribed
        let mut rx2 = net_handle
            .gossip
            .topic_receiver(Topic::ShardedTransaction(2));
        net_handle
            .gossip
            .inject_local(Topic::ShardedTransaction(2), b"s2".to_vec());
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(
            rx2.try_recv().is_ok(),
            "shard 2 should be subscribed after epoch 2"
        );

        // Cleanup
        bridge.abort();
        let _ = bridge.await;
        drop(net_handle);
        shutdown.shutdown().await.expect("shutdown");
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), net_task).await;
    }

    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    // Test 3: State sync rejects blocks with invalid committee
    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

    /// A CommitteeValidator that only knows a specific cert digest rejects
    /// blocks containing unknown certificate digests.
    #[test]
    fn state_sync_rejects_invalid_committee_block() {
        use nexus_consensus::types::CommittedBatch;
        use nexus_node::state_sync::{validate_block_committee, CommitteeValidator};
        use nexus_primitives::{Blake3Digest, CommitSequence, EpochNumber, TimestampMs};
        use std::collections::HashSet;

        struct StrictValidator;
        impl CommitteeValidator for StrictValidator {
            fn known_cert_digests_for_epoch(
                &self,
                _epoch: EpochNumber,
            ) -> Option<HashSet<Blake3Digest>> {
                Some(HashSet::from([Blake3Digest([0xAA; 32])]))
            }
            fn epoch_for_sequence(&self, _seq: CommitSequence) -> Option<EpochNumber> {
                Some(EpochNumber(1))
            }
            fn latest_known_epoch(&self) -> EpochNumber {
                EpochNumber(1)
            }
        }

        let known_cert = Blake3Digest([0xAA; 32]);
        let unknown_cert = Blake3Digest([0xBB; 32]);

        // Block with known cert → accepted
        let good_block = CommittedBatch {
            anchor: known_cert,
            certificates: vec![known_cert],
            sequence: CommitSequence(1),
            committed_at: TimestampMs::now(),
        };
        assert!(
            validate_block_committee(&good_block, &StrictValidator),
            "block with known cert should be accepted"
        );

        // Block with unknown cert → rejected
        let bad_block = CommittedBatch {
            anchor: unknown_cert,
            certificates: vec![unknown_cert],
            sequence: CommitSequence(2),
            committed_at: TimestampMs::now(),
        };
        assert!(
            !validate_block_committee(&bad_block, &StrictValidator),
            "block with unknown cert should be rejected"
        );

        // Block mixing known and unknown → rejected
        let mixed_block = CommittedBatch {
            anchor: known_cert,
            certificates: vec![known_cert, unknown_cert],
            sequence: CommitSequence(3),
            committed_at: TimestampMs::now(),
        };
        assert!(
            !validate_block_committee(&mixed_block, &StrictValidator),
            "block with any unknown cert should be rejected"
        );
    }

    /// NoOpCommitteeValidator accepts any block unconditionally.
    #[test]
    fn state_sync_noop_validator_accepts_all() {
        use nexus_consensus::types::CommittedBatch;
        use nexus_node::state_sync::{validate_block_committee, NoOpCommitteeValidator};
        use nexus_primitives::{Blake3Digest, CommitSequence, TimestampMs};

        let block = CommittedBatch {
            anchor: Blake3Digest([0xFF; 32]),
            certificates: vec![Blake3Digest([0xFF; 32])],
            sequence: CommitSequence(1),
            committed_at: TimestampMs::now(),
        };

        assert!(
            validate_block_committee(&block, &NoOpCommitteeValidator),
            "NoOp validator should accept any block"
        );
    }

    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    // Minimal stub for QueryBackend (needed by RPC router)
    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

    struct StubQueryBackend;

    impl nexus_rpc::QueryBackend for StubQueryBackend {
        fn account_balance(
            &self,
            _address: &nexus_primitives::AccountAddress,
            _token: &nexus_primitives::TokenId,
        ) -> Result<nexus_primitives::Amount, nexus_rpc::RpcError> {
            Err(nexus_rpc::RpcError::NotFound("stub".into()))
        }
        fn transaction_receipt(
            &self,
            _digest: &nexus_primitives::TxDigest,
        ) -> Result<Option<nexus_rpc::TransactionReceiptDto>, nexus_rpc::RpcError> {
            Ok(None)
        }
        fn health_status(&self) -> nexus_rpc::HealthResponse {
            nexus_rpc::HealthResponse {
                status: "healthy",
                version: env!("CARGO_PKG_VERSION"),
                peers: 0,
                epoch: nexus_primitives::EpochNumber(0),
                latest_commit: nexus_primitives::CommitSequence(0),
                uptime_seconds: 0,
                subsystems: Vec::new(),
                reason: None,
            }
        }
        fn contract_query(
            &self,
            _request: &nexus_rpc::dto::ContractQueryRequest,
        ) -> Result<nexus_rpc::dto::ContractQueryResponse, nexus_rpc::RpcError> {
            Err(nexus_rpc::RpcError::Unavailable("stub".into()))
        }
    }

    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    // Test 4: RPC shard topology & HTLC endpoints with live backends
    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

    /// Shard topology endpoint returns correct data with a consensus
    /// backend that exposes shard info.
    #[tokio::test]
    async fn rpc_shard_topology_with_live_backend() {
        use axum::body::Body;
        use http::{Request, StatusCode};
        use nexus_rpc::dto::{ShardChainHeadDto, ShardInfoDto, ShardTopologyDto};
        use tower::ServiceExt;

        struct ShardConsensus;
        impl nexus_rpc::ConsensusBackend for ShardConsensus {
            fn active_validators(
                &self,
            ) -> Result<Vec<nexus_rpc::dto::ValidatorInfoDto>, nexus_rpc::RpcError> {
                Ok(vec![])
            }
            fn validator_info(
                &self,
                _index: nexus_primitives::ValidatorIndex,
            ) -> Result<nexus_rpc::dto::ValidatorInfoDto, nexus_rpc::RpcError> {
                Err(nexus_rpc::RpcError::NotFound("not found".into()))
            }
            fn consensus_status(
                &self,
            ) -> Result<nexus_rpc::dto::ConsensusStatusDto, nexus_rpc::RpcError> {
                Err(nexus_rpc::RpcError::Unavailable("n/a".into()))
            }
            fn shard_topology(&self) -> Result<ShardTopologyDto, nexus_rpc::RpcError> {
                Ok(ShardTopologyDto {
                    num_shards: 2,
                    shards: vec![
                        ShardInfoDto {
                            shard_id: 0,
                            validators: vec![0, 1, 2],
                        },
                        ShardInfoDto {
                            shard_id: 1,
                            validators: vec![3, 4, 5],
                        },
                    ],
                })
            }
            fn shard_chain_head(
                &self,
                shard_id: u16,
            ) -> Result<ShardChainHeadDto, nexus_rpc::RpcError> {
                Ok(ShardChainHeadDto {
                    shard_id,
                    sequence: 42,
                    anchor_digest: "aa".repeat(32),
                    epoch: 3,
                })
            }
        }

        let state = Arc::new(nexus_rpc::AppState {
            query: Arc::new(StubQueryBackend),
            intent: None,
            consensus: Some(Arc::new(ShardConsensus)),
            network: None,
            broadcaster: None,
            events: None,
            rate_limiter: None,
            faucet_addr_limiter: None,
            metrics_handle: None,
            faucet_enabled: false,
            faucet_amount: 0,
            max_ws_connections: 100,
            ws_connection_count: std::sync::atomic::AtomicUsize::new(0),
            intent_tracker: None,
            session_provenance: None,
            state_proof: None,
            mcp_dispatcher: None,
            mcp_call_index: std::sync::atomic::AtomicU64::new(0),
            quota_manager: None,
            query_gas_budget: 10_000_000,
            query_timeout_ms: 5_000,
            num_shards: 2,
            tx_lifecycle: None,
            htlc: None,
            block: None,
            event_backend: None,
        });

        let app = nexus_rpc::rest::rest_router(state);

        // GET /v2/shards → 200 with topology
        let req = Request::builder()
            .uri("/v2/shards")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
            .await
            .unwrap();
        let topo: ShardTopologyDto = serde_json::from_slice(&body).unwrap();
        assert_eq!(topo.num_shards, 2);
        assert_eq!(topo.shards.len(), 2);
        assert_eq!(topo.shards[0].shard_id, 0);
        assert_eq!(topo.shards[0].validators, vec![0, 1, 2]);
        assert_eq!(topo.shards[1].shard_id, 1);

        // GET /v2/shards/0/head → 200
        let req = Request::builder()
            .uri("/v2/shards/0/head")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
            .await
            .unwrap();
        let head: ShardChainHeadDto = serde_json::from_slice(&body).unwrap();
        assert_eq!(head.shard_id, 0);
        assert_eq!(head.sequence, 42);
        assert_eq!(head.epoch, 3);

        // GET /v2/shards/99/head → 404 (shard does not exist)
        let req = Request::builder()
            .uri("/v2/shards/99/head")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "non-existent shard_id should return 404"
        );
    }

    /// HTLC endpoints return correct lock data.
    #[tokio::test]
    async fn rpc_htlc_endpoints_with_live_backend() {
        use axum::body::Body;
        use http::{Request, StatusCode};
        use nexus_rpc::dto::{HtlcLockDto, HtlcPendingListDto, HtlcStatusDto};
        use tower::ServiceExt;

        let lock_digest_hex = "ab".repeat(32);

        struct MockHtlc {
            lock_digest_hex: String,
        }
        impl nexus_rpc::HtlcBackend for MockHtlc {
            fn get_htlc_lock(
                &self,
                _digest: &nexus_primitives::Blake3Digest,
            ) -> Result<Option<HtlcLockDto>, nexus_rpc::RpcError> {
                Ok(Some(HtlcLockDto {
                    lock_digest: self.lock_digest_hex.clone(),
                    sender: "00".repeat(32),
                    recipient: "11".repeat(32),
                    amount: 1_000_000,
                    target_shard: 1,
                    timeout_epoch: 10,
                    status: HtlcStatusDto::Pending,
                }))
            }

            fn list_pending_htlc_locks(
                &self,
                _limit: u32,
            ) -> Result<HtlcPendingListDto, nexus_rpc::RpcError> {
                Ok(HtlcPendingListDto {
                    locks: vec![HtlcLockDto {
                        lock_digest: self.lock_digest_hex.clone(),
                        sender: "00".repeat(32),
                        recipient: "11".repeat(32),
                        amount: 1_000_000,
                        target_shard: 1,
                        timeout_epoch: 10,
                        status: HtlcStatusDto::Pending,
                    }],
                    total: 1,
                })
            }
        }

        let state = Arc::new(nexus_rpc::AppState {
            query: Arc::new(StubQueryBackend),
            intent: None,
            consensus: None,
            network: None,
            broadcaster: None,
            events: None,
            rate_limiter: None,
            faucet_addr_limiter: None,
            metrics_handle: None,
            faucet_enabled: false,
            faucet_amount: 0,
            max_ws_connections: 100,
            ws_connection_count: std::sync::atomic::AtomicUsize::new(0),
            intent_tracker: None,
            session_provenance: None,
            state_proof: None,
            mcp_dispatcher: None,
            mcp_call_index: std::sync::atomic::AtomicU64::new(0),
            quota_manager: None,
            query_gas_budget: 10_000_000,
            query_timeout_ms: 5_000,
            num_shards: 2,
            tx_lifecycle: None,
            htlc: Some(Arc::new(MockHtlc {
                lock_digest_hex: lock_digest_hex.clone(),
            })),
            block: None,
            event_backend: None,
        });

        let app = nexus_rpc::rest::rest_router(state);

        // GET /v2/htlc/pending → 200
        let req = Request::builder()
            .uri("/v2/htlc/pending")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
            .await
            .unwrap();
        let pending: HtlcPendingListDto = serde_json::from_slice(&body).unwrap();
        assert_eq!(pending.total, 1);
        assert_eq!(pending.locks[0].amount, 1_000_000);
        assert_eq!(pending.locks[0].status, HtlcStatusDto::Pending);

        // GET /v2/htlc/{lock_digest} → 200
        let req = Request::builder()
            .uri(format!("/v2/htlc/{lock_digest_hex}"))
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
            .await
            .unwrap();
        let lock: HtlcLockDto = serde_json::from_slice(&body).unwrap();
        assert_eq!(lock.target_shard, 1);
        assert_eq!(lock.timeout_epoch, 10);
        assert_eq!(lock.status, HtlcStatusDto::Pending);

        // GET /v2/htlc/{bad_hex} → 400
        let req = Request::builder()
            .uri("/v2/htlc/not_valid_hex_zz")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "invalid hex should return 400"
        );
    }
}
