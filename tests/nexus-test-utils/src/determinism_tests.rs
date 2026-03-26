//! F-5: Multi-node deterministic replay / state root consistency tests.
//!
//! Verifies that independently constructed executors produce identical
//! state roots for the same transaction batch, which is the fundamental
//! guarantee required for honest nodes to agree on ledger state.

#![forbid(unsafe_code)]

#[cfg(test)]
mod tests {
    use nexus_config::genesis::GenesisConfig;
    use nexus_crypto::{DilithiumSigner, Signer};
    use nexus_execution::block_stm::BlockStmExecutor;
    use nexus_execution::types::{
        compute_tx_digest, SignedTransaction, TransactionBody, TransactionPayload, TX_DOMAIN,
    };
    use nexus_node::backends::StorageStateView;
    use nexus_node::genesis_boot;
    use nexus_primitives::{
        AccountAddress, Amount, CommitSequence, EpochNumber, ShardId, TimestampMs, TokenId,
    };
    use nexus_storage::{ColumnFamily, MemoryStore, StateStorage, WriteBatchOps};

    // ── Helpers ──────────────────────────────────────────────────────

    fn build_signed_transfer(
        recipient: AccountAddress,
        amount: Amount,
        nonce: u64,
    ) -> SignedTransaction {
        let (sk, pk) = DilithiumSigner::generate_keypair();
        let sender = AccountAddress::from_dilithium_pubkey(pk.as_bytes());

        let body = TransactionBody {
            sender,
            sequence_number: nonce,
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
        let sig = DilithiumSigner::sign(&sk, TX_DOMAIN, digest.as_bytes());

        SignedTransaction {
            body,
            signature: sig,
            sender_pk: pk,
            digest,
        }
    }

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

    fn boot_node(genesis: &GenesisConfig, label: &str) -> MemoryStore {
        let shard_id = ShardId(0);
        let dir = std::env::temp_dir().join(format!("nexus-consistency-{label}"));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("genesis.json");
        std::fs::write(&path, serde_json::to_string(genesis).unwrap()).unwrap();

        let store = MemoryStore::new();
        let _boot = genesis_boot::boot_from_genesis(&path, &store, shard_id).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        store
    }

    // ── F-5 Test: State root consistency across nodes ────────────────

    /// Two independently bootstrapped execution engines must produce the
    /// exact same state root, receipts, and gas totals when executing the
    /// same ordered batch of transactions. This is the core determinism
    /// property: all honest validators converge on the same ledger state.
    #[tokio::test]
    async fn state_root_consistency_across_nodes() {
        let genesis = GenesisConfig::for_testing();
        let shard_id = ShardId(0);

        // Boot 3 independent nodes from the same genesis.
        let store_a = boot_node(&genesis, "node-a");
        let store_b = boot_node(&genesis, "node-b");
        let store_c = boot_node(&genesis, "node-c");

        // Build a batch of transactions — use the same keypairs.
        let recipient = AccountAddress([0xBB; 32]);
        let txs: Vec<SignedTransaction> = (0..5)
            .map(|i| build_signed_transfer(recipient, Amount(100 + i), 0))
            .collect();

        // Seed balances identically on all 3 nodes.
        for tx in &txs {
            seed_balance(&store_a, tx.body.sender, Amount(1_000_000)).await;
            seed_balance(&store_b, tx.body.sender, Amount(1_000_000)).await;
            seed_balance(&store_c, tx.body.sender, Amount(1_000_000)).await;
        }

        // Create 3 independent executor instances.
        let state_a = StorageStateView::new(store_a, shard_id);
        let state_b = StorageStateView::new(store_b, shard_id);
        let state_c = StorageStateView::new(store_c, shard_id);

        let exec_a =
            BlockStmExecutor::with_config(shard_id, CommitSequence(1), TimestampMs(1000), 5, 4);
        let exec_b =
            BlockStmExecutor::with_config(shard_id, CommitSequence(1), TimestampMs(1000), 5, 2);
        let exec_c =
            BlockStmExecutor::with_config(shard_id, CommitSequence(1), TimestampMs(1000), 5, 1);

        // Execute the same batch on all 3 nodes.
        let result_a = exec_a.execute(&txs, &state_a).unwrap();
        let result_b = exec_b.execute(&txs, &state_b).unwrap();
        let result_c = exec_c.execute(&txs, &state_c).unwrap();

        // All state roots must be identical.
        assert_eq!(
            result_a.new_state_root, result_b.new_state_root,
            "node A and B must agree on state root"
        );
        assert_eq!(
            result_b.new_state_root, result_c.new_state_root,
            "node B and C must agree on state root"
        );

        // Gas totals must be identical.
        assert_eq!(result_a.gas_used_total, result_b.gas_used_total);
        assert_eq!(result_b.gas_used_total, result_c.gas_used_total);

        // Receipt count and per-tx status must be identical.
        assert_eq!(result_a.receipts.len(), txs.len());
        for i in 0..txs.len() {
            assert_eq!(
                result_a.receipts[i].status, result_b.receipts[i].status,
                "receipt {i}: A vs B status mismatch"
            );
            assert_eq!(
                result_b.receipts[i].status, result_c.receipts[i].status,
                "receipt {i}: B vs C status mismatch"
            );
            assert_eq!(
                result_a.receipts[i].state_changes, result_b.receipts[i].state_changes,
                "receipt {i}: A vs B state changes differ"
            );
            assert_eq!(
                result_b.receipts[i].state_changes, result_c.receipts[i].state_changes,
                "receipt {i}: B vs C state changes differ"
            );
        }
    }

    /// Serial + parallel replay on identically-bootstrapped nodes must
    /// produce the same state root, verifying that the OCC pipeline
    /// preserves determinism across execution modes.
    #[tokio::test]
    async fn serial_and_parallel_replay_state_roots_match_across_nodes() {
        let genesis = GenesisConfig::for_testing();
        let shard_id = ShardId(0);

        let store_par = boot_node(&genesis, "node-par");
        let store_ser = boot_node(&genesis, "node-ser");

        let recipient = AccountAddress([0xCC; 32]);

        // Generate conflicting transactions (same sender, sequential nonces).
        let (sk, pk) = DilithiumSigner::generate_keypair();
        let sender = AccountAddress::from_dilithium_pubkey(pk.as_bytes());

        let txs: Vec<SignedTransaction> = (0..4)
            .map(|nonce| {
                let body = TransactionBody {
                    sender,
                    sequence_number: nonce,
                    expiry_epoch: EpochNumber(1000),
                    gas_limit: 50_000,
                    gas_price: 1,
                    target_shard: None,
                    payload: TransactionPayload::Transfer {
                        recipient,
                        amount: Amount(50 + nonce * 10),
                        token: TokenId::Native,
                    },
                    chain_id: 1,
                };
                let digest = compute_tx_digest(&body).unwrap();
                let sig = DilithiumSigner::sign(&sk, TX_DOMAIN, digest.as_bytes());
                SignedTransaction {
                    body,
                    signature: sig,
                    sender_pk: pk.clone(),
                    digest,
                }
            })
            .collect();

        // Seed identical state on both nodes.
        seed_balance(&store_par, sender, Amount(10_000_000)).await;
        seed_balance(&store_ser, sender, Amount(10_000_000)).await;

        let state_par = StorageStateView::new(store_par, shard_id);
        let state_ser = StorageStateView::new(store_ser, shard_id);

        let exec_par =
            BlockStmExecutor::with_config(shard_id, CommitSequence(1), TimestampMs(2000), 5, 4);
        let exec_ser =
            BlockStmExecutor::with_config(shard_id, CommitSequence(1), TimestampMs(2000), 5, 1);

        let result_par = exec_par.execute(&txs, &state_par).unwrap();
        let result_ser = exec_ser.execute_serial(&txs, &state_ser).unwrap();

        assert_eq!(
            result_par.new_state_root, result_ser.new_state_root,
            "parallel and serial replay must produce identical state root"
        );
        assert_eq!(result_par.gas_used_total, result_ser.gas_used_total);
        for i in 0..txs.len() {
            assert_eq!(
                result_par.receipts[i].status, result_ser.receipts[i].status,
                "receipt {i}: status mismatch between parallel and serial"
            );
        }
    }
}
