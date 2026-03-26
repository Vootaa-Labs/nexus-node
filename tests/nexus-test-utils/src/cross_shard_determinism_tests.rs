// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! X-3 — Cross-shard determinism tests.
//!
//! Validates that independently constructed multi-shard execution
//! environments produce identical final state when processing the
//! same ordered batch of cross-shard HTLC transactions.

#[cfg(test)]
mod tests {
    use nexus_execution::block_stm::BlockStmExecutor;
    use nexus_execution::types::{
        compute_lock_hash, ExecutionStatus, TransactionBody, TransactionPayload,
    };
    use nexus_primitives::{
        AccountAddress, Amount, CommitSequence, EpochNumber, ShardId, TimestampMs,
    };

    use crate::fixtures::execution::{MemStateView, TxBuilder};

    const CHAIN_ID: u64 = 1;
    const TEST_PREIMAGE: &[u8] = b"test-htlc-preimage-secret-value-32bytes!";

    fn test_executor(shard: u16, seq: u64) -> BlockStmExecutor {
        let mut e = BlockStmExecutor::new(ShardId(shard), CommitSequence(seq), TimestampMs::now());
        e.set_chain_id(CHAIN_ID);
        e
    }

    // ── X-3 Test 1: Cross-shard lock determinism ────────────────────

    /// Two independent executors executing the same HtlcLock batch on the
    /// same initial state must produce identical state roots.
    #[test]
    fn cross_shard_lock_produces_identical_state_root() {
        let sender = TxBuilder::new(CHAIN_ID);
        let recipient = AccountAddress([0xBB; 32]);
        let lock_hash = compute_lock_hash(TEST_PREIMAGE);

        let lock_body = TransactionBody {
            sender: sender.sender,
            sequence_number: 0,
            expiry_epoch: EpochNumber(1000),
            gas_limit: 50_000,
            gas_price: 1,
            target_shard: Some(ShardId(0)),
            payload: TransactionPayload::HtlcLock {
                recipient,
                amount: Amount(5_000),
                target_shard: ShardId(1),
                lock_hash,
                timeout_epoch: EpochNumber(100),
            },
            chain_id: CHAIN_ID,
        };
        let lock_tx = sender.sign(lock_body);

        // Node A — executor with 4 threads
        let mut state_a = MemStateView::new();
        state_a.set_balance(sender.sender, 1_000_000);
        let exec_a = test_executor(0, 1);
        let result_a = exec_a.execute(&[lock_tx.clone()], &state_a).unwrap();

        // Node B — executor with 1 thread
        let mut state_b = MemStateView::new();
        state_b.set_balance(sender.sender, 1_000_000);
        let exec_b =
            BlockStmExecutor::with_config(ShardId(0), CommitSequence(1), TimestampMs::now(), 5, 1);
        let mut exec_b = exec_b;
        exec_b.set_chain_id(CHAIN_ID);
        let result_b = exec_b.execute(&[lock_tx], &state_b).unwrap();

        assert_eq!(
            result_a.new_state_root, result_b.new_state_root,
            "cross-shard lock must produce identical state root across executors"
        );
        assert_eq!(result_a.gas_used_total, result_b.gas_used_total);
        assert_eq!(result_a.receipts.len(), result_b.receipts.len());
        for i in 0..result_a.receipts.len() {
            assert_eq!(
                result_a.receipts[i].status, result_b.receipts[i].status,
                "receipt {i}: status mismatch"
            );
            assert_eq!(
                result_a.receipts[i].state_changes, result_b.receipts[i].state_changes,
                "receipt {i}: state changes mismatch"
            );
        }
    }

    // ── X-3 Test 2: Cross-shard claim determinism ───────────────────

    /// Claim on the target shard produces deterministic results across two
    /// independently constructed executors.
    #[test]
    fn cross_shard_claim_produces_identical_state_root() {
        let claimer = TxBuilder::new(CHAIN_ID);
        let lock_hash = compute_lock_hash(TEST_PREIMAGE);

        // Pre-seed both states identically: pending lock record + claimer gas.
        let htlc_key = format_htlc_state_key(&lock_hash);
        let lock_record_bcs = bcs::to_bytes(&nexus_execution::types::HtlcLockRecord {
            lock_digest: nexus_primitives::Blake3Digest([0xAA; 32]),
            sender: AccountAddress([0xAA; 32]),
            recipient: claimer.sender,
            amount: Amount(5_000),
            source_shard: ShardId(0),
            target_shard: ShardId(1),
            lock_hash,
            timeout_epoch: EpochNumber(100),
            status: nexus_execution::types::HtlcStatus::Pending,
            created_epoch: EpochNumber(1),
        })
        .unwrap();

        let claim_body = TransactionBody {
            sender: claimer.sender,
            sequence_number: 0,
            expiry_epoch: EpochNumber(1000),
            gas_limit: 50_000,
            gas_price: 1,
            target_shard: Some(ShardId(1)),
            payload: TransactionPayload::HtlcClaim {
                lock_digest: lock_hash,
                preimage: TEST_PREIMAGE.to_vec(),
            },
            chain_id: CHAIN_ID,
        };
        let claim_tx = claimer.sign(claim_body);

        // Node A
        let mut state_a = MemStateView::new();
        state_a.set_balance(claimer.sender, 200_000);
        state_a.set(
            AccountAddress([0x01; 32]),
            htlc_key.clone(),
            lock_record_bcs.clone(),
        );
        let exec_a = test_executor(1, 1);
        let result_a = exec_a.execute(&[claim_tx.clone()], &state_a).unwrap();

        // Node B
        let mut state_b = MemStateView::new();
        state_b.set_balance(claimer.sender, 200_000);
        state_b.set(AccountAddress([0x01; 32]), htlc_key, lock_record_bcs);
        let exec_b = test_executor(1, 1);
        let result_b = exec_b.execute(&[claim_tx], &state_b).unwrap();

        assert_eq!(
            result_a.new_state_root, result_b.new_state_root,
            "cross-shard claim must produce identical state root"
        );
        assert_eq!(result_a.gas_used_total, result_b.gas_used_total);
        for i in 0..result_a.receipts.len() {
            assert_eq!(
                result_a.receipts[i].status, result_b.receipts[i].status,
                "receipt {i}: status mismatch on claim"
            );
        }
    }

    // ── X-3 Test 3: Mixed local + cross-shard batch determinism ─────

    /// A batch containing both local transfers and HTLC locks produces
    /// identical state root on independently constructed executors.
    #[test]
    fn mixed_local_and_cross_shard_batch_determinism() {
        let sender_local = TxBuilder::new(CHAIN_ID);
        let sender_htlc = TxBuilder::new(CHAIN_ID);
        let recipient = AccountAddress([0xCC; 32]);
        let lock_hash = compute_lock_hash(b"another-preimage-for-mixed-batch!!");

        let local_tx_body = TransactionBody {
            sender: sender_local.sender,
            sequence_number: 0,
            expiry_epoch: EpochNumber(1000),
            gas_limit: 50_000,
            gas_price: 1,
            target_shard: Some(ShardId(0)),
            payload: TransactionPayload::Transfer {
                recipient,
                amount: Amount(1_000),
                token: nexus_primitives::TokenId::Native,
            },
            chain_id: CHAIN_ID,
        };
        let local_tx = sender_local.sign(local_tx_body);

        let htlc_tx_body = TransactionBody {
            sender: sender_htlc.sender,
            sequence_number: 0,
            expiry_epoch: EpochNumber(1000),
            gas_limit: 50_000,
            gas_price: 1,
            target_shard: Some(ShardId(0)),
            payload: TransactionPayload::HtlcLock {
                recipient,
                amount: Amount(2_000),
                target_shard: ShardId(1),
                lock_hash,
                timeout_epoch: EpochNumber(200),
            },
            chain_id: CHAIN_ID,
        };
        let htlc_tx = sender_htlc.sign(htlc_tx_body);

        let batch = vec![local_tx, htlc_tx];

        // Node A — 4 threads
        let mut state_a = MemStateView::new();
        state_a.set_balance(sender_local.sender, 1_000_000);
        state_a.set_balance(sender_htlc.sender, 1_000_000);
        let exec_a =
            BlockStmExecutor::with_config(ShardId(0), CommitSequence(1), TimestampMs::now(), 10, 4);
        let mut exec_a = exec_a;
        exec_a.set_chain_id(CHAIN_ID);
        let result_a = exec_a.execute(&batch, &state_a).unwrap();

        // Node B — 1 thread (serial)
        let mut state_b = MemStateView::new();
        state_b.set_balance(sender_local.sender, 1_000_000);
        state_b.set_balance(sender_htlc.sender, 1_000_000);
        let exec_b =
            BlockStmExecutor::with_config(ShardId(0), CommitSequence(1), TimestampMs::now(), 10, 1);
        let mut exec_b = exec_b;
        exec_b.set_chain_id(CHAIN_ID);
        let result_b = exec_b.execute(&batch, &state_b).unwrap();

        assert_eq!(
            result_a.new_state_root, result_b.new_state_root,
            "mixed batch must yield identical state root"
        );
        assert_eq!(result_a.receipts.len(), result_b.receipts.len());
        for i in 0..result_a.receipts.len() {
            assert_eq!(
                result_a.receipts[i].status, result_b.receipts[i].status,
                "receipt {i}: mixed batch status mismatch"
            );
        }
    }

    // ── X-3 Test 4: HTLC refund determinism ─────────────────────────

    /// HtlcRefund with expired timeout produces identical results on
    /// different executors.
    #[test]
    fn cross_shard_refund_determinism() {
        let sender = TxBuilder::new(CHAIN_ID);
        let lock_hash = compute_lock_hash(TEST_PREIMAGE);

        let htlc_key = format_htlc_state_key(&lock_hash);
        let lock_record_bcs = bcs::to_bytes(&nexus_execution::types::HtlcLockRecord {
            lock_digest: nexus_primitives::Blake3Digest([0xBB; 32]),
            sender: sender.sender,
            recipient: AccountAddress([0xBB; 32]),
            amount: Amount(5_000),
            source_shard: ShardId(0),
            target_shard: ShardId(1),
            lock_hash,
            timeout_epoch: EpochNumber(10),
            status: nexus_execution::types::HtlcStatus::Pending,
            created_epoch: EpochNumber(1),
        })
        .unwrap();

        let refund_body = TransactionBody {
            sender: sender.sender,
            sequence_number: 0,
            expiry_epoch: EpochNumber(1000),
            gas_limit: 50_000,
            gas_price: 1,
            target_shard: Some(ShardId(0)),
            payload: TransactionPayload::HtlcRefund {
                lock_digest: lock_hash,
            },
            chain_id: CHAIN_ID,
        };
        let refund_tx = sender.sign(refund_body);

        // Both executors at epoch 50 (well past timeout_epoch 10)
        let mut state_a = MemStateView::new();
        state_a.set_balance(sender.sender, 500_000);
        state_a.set(
            AccountAddress([0x01; 32]),
            htlc_key.clone(),
            lock_record_bcs.clone(),
        );
        state_a.set(
            AccountAddress([0u8; 32]),
            b"current_epoch".to_vec(),
            50u64.to_le_bytes().to_vec(),
        );
        let mut exec_a = BlockStmExecutor::new(ShardId(0), CommitSequence(1), TimestampMs::now());
        exec_a.set_chain_id(CHAIN_ID);
        exec_a.set_epoch(EpochNumber(50));
        let result_a = exec_a.execute(&[refund_tx.clone()], &state_a).unwrap();

        let mut state_b = MemStateView::new();
        state_b.set_balance(sender.sender, 500_000);
        state_b.set(AccountAddress([0x01; 32]), htlc_key, lock_record_bcs);
        state_b.set(
            AccountAddress([0u8; 32]),
            b"current_epoch".to_vec(),
            50u64.to_le_bytes().to_vec(),
        );
        let mut exec_b = BlockStmExecutor::new(ShardId(0), CommitSequence(1), TimestampMs::now());
        exec_b.set_chain_id(CHAIN_ID);
        exec_b.set_epoch(EpochNumber(50));
        let result_b = exec_b.execute(&[refund_tx], &state_b).unwrap();

        assert_eq!(
            result_a.new_state_root, result_b.new_state_root,
            "refund must produce identical state root"
        );
        assert_eq!(
            result_a.receipts[0].status, result_b.receipts[0].status,
            "refund receipt status must match"
        );
        assert_eq!(
            result_a.receipts[0].status,
            ExecutionStatus::Success,
            "refund should succeed at expired epoch"
        );
    }

    // ── Shared helper ───────────────────────────────────────────────

    fn format_htlc_state_key(lock_digest: &nexus_primitives::Blake3Digest) -> Vec<u8> {
        let mut key = b"htlc_lock_v1:".to_vec();
        key.extend_from_slice(&lock_digest.0);
        key
    }
}
