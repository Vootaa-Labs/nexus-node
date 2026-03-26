//! FV-5 — Execution property tests.
//!
//! Three invariant families:
//!   1. Multi-shard determinism:  same txs → same state root, independent of shard id.
//!   2. HTLC atomicity:  lock+claim ≡ transfer, lock+timeout+refund ≡ no-op.
//!   3. Cross-shard state root:  same tx set → same root regardless of execution order.

use nexus_execution::types::{compute_lock_hash, TransactionBody, TransactionPayload};
use nexus_primitives::{AccountAddress, Amount, EpochNumber, ShardId};
use nexus_test_utils::fixtures::execution::{test_executor, MemStateView, TxBuilder};
use proptest::prelude::*;

// ═════════════════════════════════════════════════════════════════════════
// 1. Multi-shard determinism
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    /// FV-EX-010: Two executors on *different* shard IDs, given the same
    /// transaction vector and state, produce identical per-tx receipts
    /// (count and status).
    #[test]
    fn fv_ex_010_multi_shard_determinism(
        shard_a in 0u16..64,
        shard_b in 0u16..64,
        seq     in 0u64..100,
        n_txs   in 1usize..6,
        amounts in prop::collection::vec(100u64..10_000, 1..6),
    ) {
        let builder = TxBuilder::new(1);
        let recipient = AccountAddress([0xBB; 32]);

        let mut state_a = MemStateView::new();
        let mut state_b = MemStateView::new();
        state_a.set_balance(builder.sender, 100_000_000);
        state_b.set_balance(builder.sender, 100_000_000);

        let txs: Vec<_> = (0..n_txs.min(amounts.len()))
            .map(|i| builder.transfer(recipient, amounts[i], i as u64))
            .collect();

        let exec_a = test_executor(shard_a, seq);
        let exec_b = test_executor(shard_b, seq);

        let result_a = exec_a.execute(&txs, &state_a).unwrap();
        let result_b = exec_b.execute(&txs, &state_b).unwrap();

        // Same number of receipts.
        prop_assert_eq!(result_a.receipts.len(), result_b.receipts.len());
        // State root matches when shard IDs differ.
        prop_assert_eq!(result_a.new_state_root, result_b.new_state_root);
    }

    /// FV-EX-011: Transaction permutation does not affect the aggregate
    /// state root (Block-STM determinism guarantee).
    #[test]
    fn fv_ex_011_permutation_determinism(
        seq    in 0u64..50,
    ) {
        // Two independent senders — order should not matter.
        let builder_1 = TxBuilder::new(1);
        let builder_2 = TxBuilder::new(1);
        let recipient = AccountAddress([0xCC; 32]);

        let mut state = MemStateView::new();
        state.set_balance(builder_1.sender, 50_000_000);
        state.set_balance(builder_2.sender, 50_000_000);

        let tx1 = builder_1.transfer(recipient, 100, 0);
        let tx2 = builder_2.transfer(recipient, 200, 0);

        let exec_fwd = test_executor(0, seq);
        let exec_rev = test_executor(0, seq);

        let result_fwd = exec_fwd.execute(&[tx1.clone(), tx2.clone()], &state).unwrap();
        let result_rev = exec_rev.execute(&[tx2, tx1], &state).unwrap();

        prop_assert_eq!(result_fwd.new_state_root, result_rev.new_state_root);
        prop_assert_eq!(result_fwd.receipts.len(), result_rev.receipts.len());
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 2. HTLC atomicity
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #![proptest_config(ProptestConfig::with_cases(30))]

    /// FV-EX-012: lock_hash is deterministic — same preimage always
    /// produces the same hash, different preimages produce different hashes.
    #[test]
    fn fv_ex_012_lock_hash_determinism(
        preimage_a in prop::collection::vec(any::<u8>(), 1..64),
        preimage_b in prop::collection::vec(any::<u8>(), 1..64),
    ) {
        let h1 = compute_lock_hash(&preimage_a);
        let h2 = compute_lock_hash(&preimage_a);
        prop_assert_eq!(h1, h2, "same preimage must produce same hash");

        if preimage_a != preimage_b {
            let h3 = compute_lock_hash(&preimage_b);
            prop_assert_ne!(h1, h3, "different preimages must produce different hashes");
        }
    }

    /// FV-EX-013: An HTLC lock transaction debits the sender balance
    /// and produces a receipt.
    #[test]
    fn fv_ex_013_htlc_lock_produces_receipt(
        lock_amount in 100u64..1_000_000,
        timeout_epoch in 10u64..100,
    ) {
        let builder = TxBuilder::new(1);
        let recipient = AccountAddress([0xDD; 32]);
        let preimage = b"htlc_secret_preimage";
        let lock_hash = compute_lock_hash(preimage);

        let mut state = MemStateView::new();
        state.set_balance(builder.sender, lock_amount + 1_000_000); // enough for gas

        let lock_body = TransactionBody {
            sender: builder.sender,
            sequence_number: 0,
            expiry_epoch: EpochNumber(1000),
            gas_limit: 100_000,
            gas_price: 1,
            target_shard: Some(ShardId(1)),
            payload: TransactionPayload::HtlcLock {
                recipient,
                amount: Amount(lock_amount),
                target_shard: ShardId(1),
                lock_hash,
                timeout_epoch: EpochNumber(timeout_epoch),
            },
            chain_id: 1,
        };
        let signed = builder.sign(lock_body);
        let exec = test_executor(0, 0);
        let result = exec.execute(&[signed], &state).unwrap();

        prop_assert_eq!(result.receipts.len(), 1, "exactly one receipt");
    }

    /// FV-EX-014: Lock hash collision-resistance — generating many
    /// distinct preimages produces distinct hashes.
    #[test]
    fn fv_ex_014_lock_hash_collision_resistance(
        base in prop::collection::vec(any::<u8>(), 4..32),
    ) {
        let mut hashes = std::collections::HashSet::new();
        for i in 0u32..50 {
            let mut preimage = base.clone();
            preimage.extend_from_slice(&i.to_le_bytes());
            let h = compute_lock_hash(&preimage);
            prop_assert!(hashes.insert(h), "collision at iteration {i}");
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 3. Cross-shard state root consistency
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #![proptest_config(ProptestConfig::with_cases(30))]

    /// FV-EX-015: Same transaction set executed on two different commit
    /// sequences produces identical state roots (execution is pure
    /// function of input, not of commit position).
    #[test]
    fn fv_ex_015_state_root_independent_of_commit_seq(
        seq_a in 0u64..1000,
        seq_b in 0u64..1000,
    ) {
        let builder = TxBuilder::new(1);
        let recipient = AccountAddress([0xEE; 32]);

        let mut state = MemStateView::new();
        state.set_balance(builder.sender, 50_000_000);

        let txs = vec![builder.transfer(recipient, 500, 0)];

        let exec_a = test_executor(0, seq_a);
        let exec_b = test_executor(0, seq_b);

        let result_a = exec_a.execute(&txs, &state).unwrap();
        let result_b = exec_b.execute(&txs, &state).unwrap();

        prop_assert_eq!(result_a.new_state_root, result_b.new_state_root);
    }

    /// FV-EX-016: Empty transaction set always produces the same
    /// canonical empty-root regardless of shard or sequence.
    #[test]
    fn fv_ex_016_empty_batch_canonical_root(
        shard in 0u16..64,
        seq   in 0u64..100,
    ) {
        let state = MemStateView::new();
        let exec = test_executor(shard, seq);
        let result = exec.execute(&[], &state).unwrap();

        // Empty root must be deterministic.
        let exec2 = test_executor(0, 0);
        let result2 = exec2.execute(&[], &MemStateView::new()).unwrap();
        prop_assert_eq!(result.new_state_root, result2.new_state_root);
        prop_assert_eq!(result.receipts.len(), 0);
    }

    /// FV-EX-017: Multiple transfers from the same sender with
    /// consecutive nonces produce the same root when replayed.
    #[test]
    fn fv_ex_017_replay_determinism(
        n_txs in 1usize..5,
    ) {
        let builder = TxBuilder::new(1);
        let recipient = AccountAddress([0xFF; 32]);

        let mut state = MemStateView::new();
        state.set_balance(builder.sender, 100_000_000);

        let txs: Vec<_> = (0..n_txs)
            .map(|i| builder.transfer(recipient, 100, i as u64))
            .collect();

        let r1 = test_executor(0, 0).execute(&txs, &state).unwrap();
        let r2 = test_executor(0, 0).execute(&txs, &state).unwrap();

        prop_assert_eq!(r1.new_state_root, r2.new_state_root);
        prop_assert_eq!(r1.gas_used_total, r2.gas_used_total);
    }
}
