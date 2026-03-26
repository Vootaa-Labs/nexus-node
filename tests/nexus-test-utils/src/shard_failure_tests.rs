// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! X-4 — Failure rollback tests for cross-shard / multi-shard resilience.
//!
//! Acceptance criteria from roadmap:
//! 1. HTLC timeout → refund succeeds after expiry, fails before.
//! 2. Cross-shard claim with wrong preimage or missing lock is rejected.
//! 3. Partial shard crash: other shards remain independently operational.
//! 4. HTLC cold-restart: pending lock survives persistence round-trip.

#[cfg(test)]
mod tests {
    use nexus_execution::block_stm::BlockStmExecutor;
    use nexus_execution::types::{
        compute_lock_hash, ExecutionStatus, HtlcLockRecord, HtlcStatus, TransactionBody,
        TransactionPayload,
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

    fn format_htlc_state_key(lock_digest: &nexus_primitives::Blake3Digest) -> Vec<u8> {
        let mut key = b"htlc_lock_v1:".to_vec();
        key.extend_from_slice(&lock_digest.0);
        key
    }

    // ── X-4 Test 1: HTLC refund rejected before timeout ────────────

    #[test]
    fn htlc_refund_rejected_before_timeout() {
        let sender = TxBuilder::new(CHAIN_ID);
        let lock_hash = compute_lock_hash(TEST_PREIMAGE);

        let lock_record = HtlcLockRecord {
            lock_digest: nexus_primitives::Blake3Digest([0xAA; 32]),
            sender: sender.sender,
            recipient: AccountAddress([0xBB; 32]),
            amount: Amount(5_000),
            source_shard: ShardId(0),
            target_shard: ShardId(1),
            lock_hash,
            timeout_epoch: EpochNumber(100),
            status: HtlcStatus::Pending,
            created_epoch: EpochNumber(1),
        };

        let mut state = MemStateView::new();
        state.set_balance(sender.sender, 500_000);
        state.set(
            AccountAddress([0x01; 32]),
            format_htlc_state_key(&lock_hash),
            bcs::to_bytes(&lock_record).unwrap(),
        );
        // Seed current_epoch = 50 < timeout_epoch = 100 → refund too early.
        state.set(
            AccountAddress([0u8; 32]),
            b"current_epoch".to_vec(),
            50u64.to_le_bytes().to_vec(),
        );

        let refund_tx = sender.sign(TransactionBody {
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
        });

        let mut exec = test_executor(0, 1);
        exec.set_epoch(EpochNumber(50)); // before timeout_epoch=100
        let result = exec.execute(&[refund_tx], &state).unwrap();

        assert_eq!(
            result.receipts[0].status,
            ExecutionStatus::HtlcRefundTooEarly,
            "refund before timeout must be rejected"
        );
    }

    // ── X-4 Test 2: HTLC refund succeeds after timeout ─────────────

    #[test]
    fn htlc_refund_succeeds_after_timeout() {
        let sender = TxBuilder::new(CHAIN_ID);
        let lock_hash = compute_lock_hash(TEST_PREIMAGE);

        let lock_record = HtlcLockRecord {
            lock_digest: nexus_primitives::Blake3Digest([0xBB; 32]),
            sender: sender.sender,
            recipient: AccountAddress([0xBB; 32]),
            amount: Amount(5_000),
            source_shard: ShardId(0),
            target_shard: ShardId(1),
            lock_hash,
            timeout_epoch: EpochNumber(10),
            status: HtlcStatus::Pending,
            created_epoch: EpochNumber(1),
        };

        let mut state = MemStateView::new();
        state.set_balance(sender.sender, 500_000);
        state.set(
            AccountAddress([0x01; 32]),
            format_htlc_state_key(&lock_hash),
            bcs::to_bytes(&lock_record).unwrap(),
        );
        // Seed current_epoch = 50 > timeout_epoch = 10 → refund allowed.
        state.set(
            AccountAddress([0u8; 32]),
            b"current_epoch".to_vec(),
            50u64.to_le_bytes().to_vec(),
        );

        let refund_tx = sender.sign(TransactionBody {
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
        });

        let mut exec = test_executor(0, 1);
        exec.set_epoch(EpochNumber(50)); // past timeout_epoch=10
        let result = exec.execute(&[refund_tx], &state).unwrap();

        assert_eq!(
            result.receipts[0].status,
            ExecutionStatus::Success,
            "refund after timeout should succeed"
        );
    }

    // ── X-4 Test 3: Cross-shard claim with wrong preimage ───────────

    #[test]
    fn cross_shard_claim_wrong_preimage_rejected() {
        let claimer = TxBuilder::new(CHAIN_ID);
        let lock_hash = compute_lock_hash(TEST_PREIMAGE);

        let lock_record = HtlcLockRecord {
            lock_digest: nexus_primitives::Blake3Digest([0xCC; 32]),
            sender: AccountAddress([0xAA; 32]),
            recipient: claimer.sender,
            amount: Amount(5_000),
            source_shard: ShardId(0),
            target_shard: ShardId(1),
            lock_hash,
            timeout_epoch: EpochNumber(100),
            status: HtlcStatus::Pending,
            created_epoch: EpochNumber(1),
        };

        let mut state = MemStateView::new();
        state.set_balance(claimer.sender, 200_000);
        state.set(
            AccountAddress([0x01; 32]),
            format_htlc_state_key(&lock_hash),
            bcs::to_bytes(&lock_record).unwrap(),
        );

        let claim_tx = claimer.sign(TransactionBody {
            sender: claimer.sender,
            sequence_number: 0,
            expiry_epoch: EpochNumber(1000),
            gas_limit: 50_000,
            gas_price: 1,
            target_shard: Some(ShardId(1)),
            payload: TransactionPayload::HtlcClaim {
                lock_digest: lock_hash,
                preimage: b"wrong-preimage-not-matching-hash!!".to_vec(),
            },
            chain_id: CHAIN_ID,
        });

        let exec = test_executor(1, 1);
        let result = exec.execute(&[claim_tx], &state).unwrap();

        assert_eq!(
            result.receipts[0].status,
            ExecutionStatus::HtlcPreimageMismatch,
            "claim with wrong preimage should be rejected"
        );
    }

    // ── X-4 Test 4: Cross-shard claim nonexistent lock ──────────────

    #[test]
    fn cross_shard_claim_nonexistent_lock_rejected() {
        let claimer = TxBuilder::new(CHAIN_ID);
        let fake_hash = compute_lock_hash(b"no-lock-was-created-with-this-preimage!!");

        let mut state = MemStateView::new();
        state.set_balance(claimer.sender, 200_000);

        let claim_tx = claimer.sign(TransactionBody {
            sender: claimer.sender,
            sequence_number: 0,
            expiry_epoch: EpochNumber(1000),
            gas_limit: 50_000,
            gas_price: 1,
            target_shard: Some(ShardId(1)),
            payload: TransactionPayload::HtlcClaim {
                lock_digest: fake_hash,
                preimage: b"no-lock-was-created-with-this-preimage!!".to_vec(),
            },
            chain_id: CHAIN_ID,
        });

        let exec = test_executor(1, 1);
        let result = exec.execute(&[claim_tx], &state).unwrap();

        assert_eq!(
            result.receipts[0].status,
            ExecutionStatus::HtlcLockNotFound,
            "claim for nonexistent lock should be rejected"
        );
    }

    // ── X-4 Test 5: Partial shard independence ──────────────────────

    /// Two shards executing independently: a failure on shard 1 does not
    /// affect shard 0's ability to execute and produce correct results.
    #[test]
    fn partial_shard_failure_does_not_affect_other_shard() {
        let sender_s0 = TxBuilder::new(CHAIN_ID);
        let sender_s1 = TxBuilder::new(CHAIN_ID);
        let recipient = AccountAddress([0xCC; 32]);

        // Shard 0: valid transfer
        let tx_s0 = sender_s0.sign(TransactionBody {
            sender: sender_s0.sender,
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
        });

        // Shard 1: insufficient balance → will fail
        let tx_s1 = sender_s1.sign(TransactionBody {
            sender: sender_s1.sender,
            sequence_number: 0,
            expiry_epoch: EpochNumber(1000),
            gas_limit: 50_000,
            gas_price: 1,
            target_shard: Some(ShardId(1)),
            payload: TransactionPayload::Transfer {
                recipient,
                amount: Amount(999_999_999),
                token: nexus_primitives::TokenId::Native,
            },
            chain_id: CHAIN_ID,
        });

        // Shard 0 executes successfully
        let mut state_s0 = MemStateView::new();
        state_s0.set_balance(sender_s0.sender, 1_000_000);
        let exec_s0 = test_executor(0, 1);
        let result_s0 = exec_s0.execute(&[tx_s0], &state_s0).unwrap();
        assert_eq!(
            result_s0.receipts[0].status,
            ExecutionStatus::Success,
            "shard 0 should succeed independently"
        );

        // Shard 1 fails but does not crash — it produces a receipt
        let mut state_s1 = MemStateView::new();
        state_s1.set_balance(sender_s1.sender, 100); // insufficient
        let exec_s1 = test_executor(1, 1);
        let result_s1 = exec_s1.execute(&[tx_s1], &state_s1).unwrap();
        assert_ne!(
            result_s1.receipts[0].status,
            ExecutionStatus::Success,
            "shard 1 should fail (insufficient balance)"
        );

        // Critically: shard 0's result is unaffected
        assert!(
            !result_s0.new_state_root.0.iter().all(|&b| b == 0),
            "shard 0 should have a non-zero state root"
        );
    }

    // ── X-4 Test 6: HTLC lock record survives BCS round-trip ───────

    /// Verifies that a serialized HtlcLockRecord round-trips through BCS
    /// correctly — the persistence layer relies on this.
    #[test]
    fn htlc_lock_record_bcs_round_trip() {
        let lock_hash = compute_lock_hash(TEST_PREIMAGE);

        let original = HtlcLockRecord {
            lock_digest: nexus_primitives::Blake3Digest([0xDD; 32]),
            sender: AccountAddress([0xAA; 32]),
            recipient: AccountAddress([0xBB; 32]),
            amount: Amount(12_345),
            source_shard: ShardId(0),
            target_shard: ShardId(3),
            lock_hash,
            timeout_epoch: EpochNumber(42),
            status: HtlcStatus::Pending,
            created_epoch: EpochNumber(1),
        };

        let bytes = bcs::to_bytes(&original).unwrap();
        let recovered: HtlcLockRecord = bcs::from_bytes(&bytes).unwrap();

        assert_eq!(recovered.sender, original.sender);
        assert_eq!(recovered.recipient, original.recipient);
        assert_eq!(recovered.amount, original.amount);
        assert_eq!(recovered.target_shard, original.target_shard);
        assert_eq!(recovered.lock_hash, original.lock_hash);
        assert_eq!(recovered.timeout_epoch, original.timeout_epoch);
        assert_eq!(recovered.status, original.status);
    }
}
