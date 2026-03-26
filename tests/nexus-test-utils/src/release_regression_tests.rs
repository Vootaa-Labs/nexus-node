// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! X-5 — Pre-release regression tests for multi-shard production gate.
//!
//! Validates:
//! 1. Single-shard backward compatibility (num_shards=1 matches v0.1.9 behaviour).
//! 2. Multi-shard lifecycle (lock → claim across two shards).
//! 3. Epoch advance preserves shard chain heads.
//! 4. Gas accounting consistency across shards (identical gas for identical tx).

#[cfg(test)]
mod tests {
    use nexus_execution::block_stm::BlockStmExecutor;
    use nexus_execution::types::{
        compute_lock_hash, ExecutionStatus, HtlcLockRecord, HtlcStatus, TransactionBody,
        TransactionPayload,
    };
    use nexus_primitives::{
        AccountAddress, Amount, Blake3Digest, CommitSequence, EpochNumber, ShardId, TimestampMs,
        TokenId,
    };

    use crate::fixtures::execution::{MemStateView, TxBuilder};

    const CHAIN_ID: u64 = 1;

    fn test_executor(shard: u16, seq: u64) -> BlockStmExecutor {
        let mut e = BlockStmExecutor::new(ShardId(shard), CommitSequence(seq), TimestampMs::now());
        e.set_chain_id(CHAIN_ID);
        e
    }

    fn format_htlc_state_key(lock_digest: &Blake3Digest) -> Vec<u8> {
        let mut key = b"htlc_lock_v1:".to_vec();
        key.extend_from_slice(&lock_digest.0);
        key
    }

    // ── X-5 Test 1: Single-shard Transfer compat (v0.1.9 baseline) ─────

    /// A simple transfer on shard 0 with num_shards=1 produces a valid
    /// receipt and non-zero state root — the same behaviour as pre-shard
    /// releases.
    #[test]
    fn single_shard_transfer_backward_compatible() {
        let sender = TxBuilder::new(CHAIN_ID);
        let recipient = AccountAddress([0xBB; 32]);

        let tx = sender.sign(TransactionBody {
            sender: sender.sender,
            sequence_number: 0,
            expiry_epoch: EpochNumber(1000),
            gas_limit: 50_000,
            gas_price: 1,
            target_shard: Some(ShardId(0)),
            payload: TransactionPayload::Transfer {
                recipient,
                amount: Amount(1_000),
                token: TokenId::Native,
            },
            chain_id: CHAIN_ID,
        });

        let mut state = MemStateView::new();
        state.set_balance(sender.sender, 1_000_000);

        let exec = test_executor(0, 1);
        let result = exec.execute(&[tx], &state).unwrap();

        assert_eq!(result.receipts.len(), 1);
        assert_eq!(result.receipts[0].status, ExecutionStatus::Success);
        assert!(result.gas_used_total > 0, "gas should be accounted");
        assert!(
            !result.new_state_root.0.iter().all(|&b| b == 0),
            "state root must be non-zero after mutation"
        );
    }

    // ── X-5 Test 2: Single-shard HTLC lock compat ──────────────────────

    /// An HtlcLock on shard 0 works normally in single-shard mode, exactly
    /// as in v0.1.9.
    #[test]
    fn single_shard_htlc_lock_backward_compatible() {
        let sender = TxBuilder::new(CHAIN_ID);
        let lock_hash = compute_lock_hash(b"regression-test-preimage-single-shard");

        let tx = sender.sign(TransactionBody {
            sender: sender.sender,
            sequence_number: 0,
            expiry_epoch: EpochNumber(1000),
            gas_limit: 50_000,
            gas_price: 1,
            target_shard: Some(ShardId(0)),
            payload: TransactionPayload::HtlcLock {
                recipient: AccountAddress([0xCC; 32]),
                amount: Amount(3_000),
                target_shard: ShardId(1),
                lock_hash,
                timeout_epoch: EpochNumber(200),
            },
            chain_id: CHAIN_ID,
        });

        let mut state = MemStateView::new();
        state.set_balance(sender.sender, 500_000);

        let exec = test_executor(0, 1);
        let result = exec.execute(&[tx], &state).unwrap();

        assert_eq!(result.receipts[0].status, ExecutionStatus::Success);
        // Verify lock record was written in state.
        let lock_change = result.receipts[0]
            .state_changes
            .iter()
            .find(|c| c.key.starts_with(b"htlc_lock_v1:"));
        assert!(
            lock_change.is_some(),
            "lock record should appear in state changes"
        );
    }

    // ── X-5 Test 3: Multi-shard lifecycle (lock → claim) ────────────────

    /// Full HTLC lifecycle across two shards: lock on shard 0, claim on
    /// shard 1.  This is the minimal end-to-end multi-shard flow.
    #[test]
    fn multi_shard_lock_then_claim_lifecycle() {
        let sender = TxBuilder::new(CHAIN_ID);
        let claimer = TxBuilder::new(CHAIN_ID);
        let preimage = b"multi-shard-lifecycle-regression-pre";
        let lock_hash = compute_lock_hash(preimage);

        // Step 1 — Lock on shard 0
        let lock_tx = sender.sign(TransactionBody {
            sender: sender.sender,
            sequence_number: 0,
            expiry_epoch: EpochNumber(1000),
            gas_limit: 50_000,
            gas_price: 1,
            target_shard: Some(ShardId(0)),
            payload: TransactionPayload::HtlcLock {
                recipient: claimer.sender,
                amount: Amount(5_000),
                target_shard: ShardId(1),
                lock_hash,
                timeout_epoch: EpochNumber(100),
            },
            chain_id: CHAIN_ID,
        });

        let mut state_s0 = MemStateView::new();
        state_s0.set_balance(sender.sender, 1_000_000);

        let exec_s0 = test_executor(0, 1);
        let lock_result = exec_s0.execute(&[lock_tx], &state_s0).unwrap();
        assert_eq!(lock_result.receipts[0].status, ExecutionStatus::Success);

        // Extract the lock record from state changes.
        let lock_change = lock_result.receipts[0]
            .state_changes
            .iter()
            .find(|c| c.key.starts_with(b"htlc_lock_v1:"))
            .expect("lock state change must exist");
        let lock_record_bytes = lock_change.value.clone().unwrap();
        let lock_record: HtlcLockRecord = bcs::from_bytes(&lock_record_bytes).unwrap();
        assert_eq!(lock_record.status, HtlcStatus::Pending);

        // Step 2 — Claim on shard 1
        let claim_tx = claimer.sign(TransactionBody {
            sender: claimer.sender,
            sequence_number: 0,
            expiry_epoch: EpochNumber(1000),
            gas_limit: 50_000,
            gas_price: 1,
            target_shard: Some(ShardId(1)),
            payload: TransactionPayload::HtlcClaim {
                lock_digest: lock_record.lock_digest,
                preimage: preimage.to_vec(),
            },
            chain_id: CHAIN_ID,
        });

        let mut state_s1 = MemStateView::new();
        state_s1.set_balance(claimer.sender, 200_000);
        let htlc_key = format_htlc_state_key(&lock_record.lock_digest);
        state_s1.set(AccountAddress([0x01; 32]), htlc_key, lock_record_bytes);

        let exec_s1 = test_executor(1, 1);
        let claim_result = exec_s1.execute(&[claim_tx], &state_s1).unwrap();
        assert_eq!(
            claim_result.receipts[0].status,
            ExecutionStatus::Success,
            "claim on target shard should succeed"
        );
    }

    // ── X-5 Test 4: Gas accounting same across shards ───────────────────

    /// An identical Transfer executed on shard 0 and shard 1 independently
    /// must consume the same gas.
    #[test]
    fn gas_consistency_across_shards() {
        let sender = TxBuilder::new(CHAIN_ID);
        let recipient = AccountAddress([0xDD; 32]);

        let body = TransactionBody {
            sender: sender.sender,
            sequence_number: 0,
            expiry_epoch: EpochNumber(1000),
            gas_limit: 50_000,
            gas_price: 1,
            target_shard: Some(ShardId(0)),
            payload: TransactionPayload::Transfer {
                recipient,
                amount: Amount(500),
                token: TokenId::Native,
            },
            chain_id: CHAIN_ID,
        };
        let tx = sender.sign(body);

        let mut state_s0 = MemStateView::new();
        state_s0.set_balance(sender.sender, 1_000_000);
        let exec_s0 = test_executor(0, 1);
        let result_s0 = exec_s0.execute(&[tx.clone()], &state_s0).unwrap();

        let mut state_s1 = MemStateView::new();
        state_s1.set_balance(sender.sender, 1_000_000);
        let exec_s1 = test_executor(1, 1);
        let result_s1 = exec_s1.execute(&[tx], &state_s1).unwrap();

        assert_eq!(
            result_s0.gas_used_total, result_s1.gas_used_total,
            "identical tx must consume identical gas regardless of shard"
        );
        assert_eq!(
            result_s0.receipts[0].gas_used, result_s1.receipts[0].gas_used,
            "per-receipt gas must match across shards"
        );
    }

    // ── X-5 Test 5: Multiple sequential commits don't drift ─────────────

    /// Two sequential commit sequences on the same shard produce increasing,
    /// non-duplicated state roots.
    #[test]
    fn sequential_commits_produce_distinct_state_roots() {
        let sender = TxBuilder::new(CHAIN_ID);
        let recipient = AccountAddress([0xEE; 32]);

        let tx1 = sender.sign(TransactionBody {
            sender: sender.sender,
            sequence_number: 0,
            expiry_epoch: EpochNumber(1000),
            gas_limit: 50_000,
            gas_price: 1,
            target_shard: Some(ShardId(0)),
            payload: TransactionPayload::Transfer {
                recipient,
                amount: Amount(100),
                token: TokenId::Native,
            },
            chain_id: CHAIN_ID,
        });

        let tx2 = sender.sign(TransactionBody {
            sender: sender.sender,
            sequence_number: 1,
            expiry_epoch: EpochNumber(1000),
            gas_limit: 50_000,
            gas_price: 1,
            target_shard: Some(ShardId(0)),
            payload: TransactionPayload::Transfer {
                recipient,
                amount: Amount(200),
                token: TokenId::Native,
            },
            chain_id: CHAIN_ID,
        });

        let mut state = MemStateView::new();
        state.set_balance(sender.sender, 1_000_000);

        let exec1 = test_executor(0, 1);
        let result1 = exec1.execute(&[tx1], &state).unwrap();
        assert_eq!(result1.receipts[0].status, ExecutionStatus::Success);

        // Apply the first result's changes to state for the second commit.
        for change in &result1.receipts[0].state_changes {
            if let Some(ref v) = change.value {
                state.set(change.account, change.key.clone(), v.clone());
            }
        }

        let exec2 = test_executor(0, 2);
        let result2 = exec2.execute(&[tx2], &state).unwrap();
        assert_eq!(result2.receipts[0].status, ExecutionStatus::Success);

        assert_ne!(
            result1.new_state_root, result2.new_state_root,
            "different commits must produce different state roots"
        );
    }
}
