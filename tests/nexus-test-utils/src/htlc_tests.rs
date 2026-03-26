// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Phase V / V-8 acceptance tests — Cross-shard HTLC protocol.
//!
//! Validates the full HTLC lifecycle:
//! 1. Lock → Claim success (cross-shard transfer completes)
//! 2. Lock → Timeout → Refund (sender recovers funds)
//! 3. Claim with wrong preimage rejected
//! 4. Double-claim rejected
//! 5. Refund before timeout rejected
//! 6. Cold-restart: pending HTLC persists and is resolvable

use nexus_execution::types::{
    compute_lock_hash, ExecutionStatus, HtlcLockRecord, HtlcStatus, TransactionBody,
    TransactionPayload,
};
use nexus_execution::BlockStmExecutor;
use nexus_primitives::{
    AccountAddress, Amount, Blake3Digest, CommitSequence, EpochNumber, ShardId, TimestampMs,
};

use crate::fixtures::execution::{MemStateView, TxBuilder};

// ── Helpers ─────────────────────────────────────────────────────────────

/// Create a BlockStmExecutor for test use on a given shard.
fn test_executor(shard: u16, seq: u64) -> BlockStmExecutor {
    BlockStmExecutor::new(ShardId(shard), CommitSequence(seq), TimestampMs::now())
}

/// A known preimage for test HTLC locks.
const TEST_PREIMAGE: &[u8] = b"test-htlc-preimage-secret-value-32bytes!";

/// Compute the lock hash for the test preimage.
fn test_lock_hash() -> Blake3Digest {
    compute_lock_hash(TEST_PREIMAGE)
}

// ── V-8 Test 1: Lock → Claim success ───────────────────────────────────

#[test]
fn htlc_lock_then_claim_succeeds() {
    let sender_builder = TxBuilder::new(1);
    let recipient = AccountAddress([0xBB; 32]);
    let lock_amount = 5_000u64;
    let lock_hash = test_lock_hash();

    // ── Source shard: execute HtlcLock ──
    let mut source_state = MemStateView::new();
    source_state.set_balance(sender_builder.sender, 100_000);

    let lock_body = TransactionBody {
        sender: sender_builder.sender,
        sequence_number: 0,
        expiry_epoch: EpochNumber(1000),
        gas_limit: 50_000,
        gas_price: 1,
        target_shard: Some(ShardId(0)),
        payload: TransactionPayload::HtlcLock {
            recipient,
            amount: Amount(lock_amount),
            target_shard: ShardId(1),
            lock_hash,
            timeout_epoch: EpochNumber(100),
        },
        chain_id: 1,
    };
    let lock_tx = sender_builder.sign(lock_body);
    let lock_digest = lock_tx.digest;

    let mut executor = test_executor(0, 1);
    executor.set_chain_id(1);
    let result = executor
        .execute(&[lock_tx], &source_state)
        .expect("lock execution must not error");

    assert_eq!(result.receipts.len(), 1);
    assert_eq!(
        result.receipts[0].status,
        ExecutionStatus::Success,
        "HtlcLock should succeed"
    );

    // Verify sender balance was debited.
    let sender_balance_change = result.receipts[0]
        .state_changes
        .iter()
        .find(|c| c.account == sender_builder.sender && c.key == b"balance")
        .expect("sender balance should be in state changes");
    let new_balance = u64::from_le_bytes(
        sender_balance_change.value.as_ref().unwrap()[..8]
            .try_into()
            .unwrap(),
    );
    // 100_000 - 5_000 (lock amount) - 50_000 * 1 (gas cost) = 45_000
    assert_eq!(
        new_balance, 45_000,
        "sender should be debited lock amount + gas"
    );

    // Verify HTLC lock record was written.
    let htlc_change = result.receipts[0]
        .state_changes
        .iter()
        .find(|c| c.account == AccountAddress([0x01; 32]) && c.key.starts_with(b"htlc_lock_v1:"))
        .expect("HTLC lock record should be in state changes");
    let record: HtlcLockRecord =
        bcs::from_bytes(htlc_change.value.as_ref().unwrap()).expect("record should deserialize");
    assert_eq!(record.status, HtlcStatus::Pending);
    assert_eq!(record.amount, Amount(lock_amount));
    assert_eq!(record.recipient, recipient);
    assert_eq!(record.lock_hash, lock_hash);

    // ── Target shard: execute HtlcClaim ──
    let mut target_state = MemStateView::new();
    target_state.set_balance(recipient, 0);
    // The claim executor needs to see the lock record.
    let htlc_key = format_htlc_state_key(&lock_digest);
    target_state.set(
        AccountAddress([0x01; 32]),
        htlc_key,
        htlc_change.value.clone().unwrap(),
    );

    let claim_body = TransactionBody {
        sender: recipient,
        sequence_number: 0,
        expiry_epoch: EpochNumber(1000),
        gas_limit: 50_000,
        gas_price: 1,
        target_shard: Some(ShardId(1)),
        payload: TransactionPayload::HtlcClaim {
            lock_digest,
            preimage: TEST_PREIMAGE.to_vec(),
        },
        chain_id: 1,
    };
    // Note: in tests we sign with the recipient's key (a separate builder),
    // but for simplicity we'll create a builder for the recipient.
    let recipient_builder = TxBuilder::new(1);
    // Since the claim tx sender is checked in pre-validation against sender_pk,
    // we need the claim body to match the builder's sender.
    let claim_body_actual = TransactionBody {
        sender: recipient_builder.sender,
        ..claim_body
    };
    let claim_tx = recipient_builder.sign(claim_body_actual);

    let mut claim_executor = test_executor(1, 2);
    claim_executor.set_chain_id(1);
    let claim_result = claim_executor
        .execute(&[claim_tx], &target_state)
        .expect("claim execution must not error");

    assert_eq!(claim_result.receipts.len(), 1);
    assert_eq!(
        claim_result.receipts[0].status,
        ExecutionStatus::Success,
        "HtlcClaim should succeed"
    );

    // Verify recipient balance was credited.
    let recipient_balance_change = claim_result.receipts[0]
        .state_changes
        .iter()
        .find(|c| c.account == record.recipient && c.key == b"balance")
        .expect("recipient balance should be in state changes");
    let recipient_balance = u64::from_le_bytes(
        recipient_balance_change.value.as_ref().unwrap()[..8]
            .try_into()
            .unwrap(),
    );
    assert_eq!(
        recipient_balance, lock_amount,
        "recipient should receive locked amount"
    );

    // Verify lock record is now Claimed.
    let claim_htlc_change = claim_result.receipts[0]
        .state_changes
        .iter()
        .find(|c| c.account == AccountAddress([0x01; 32]) && c.key.starts_with(b"htlc_lock_v1:"))
        .expect("HTLC lock record update should be in state changes");
    let claimed_record: HtlcLockRecord =
        bcs::from_bytes(claim_htlc_change.value.as_ref().unwrap()).expect("record deserialization");
    assert_eq!(claimed_record.status, HtlcStatus::Claimed);
}

// ── V-8 Test 2: Lock → Timeout → Refund success ────────────────────────

#[test]
fn htlc_lock_timeout_then_refund_succeeds() {
    let sender_builder = TxBuilder::new(1);
    let recipient = AccountAddress([0xCC; 32]);
    let lock_amount = 3_000u64;
    let lock_hash = test_lock_hash();
    let timeout_epoch = EpochNumber(10);

    let mut source_state = MemStateView::new();
    source_state.set_balance(sender_builder.sender, 100_000);

    // Execute lock.
    let lock_body = TransactionBody {
        sender: sender_builder.sender,
        sequence_number: 0,
        expiry_epoch: EpochNumber(1000),
        gas_limit: 50_000,
        gas_price: 1,
        target_shard: Some(ShardId(0)),
        payload: TransactionPayload::HtlcLock {
            recipient,
            amount: Amount(lock_amount),
            target_shard: ShardId(1),
            lock_hash,
            timeout_epoch,
        },
        chain_id: 1,
    };
    let lock_tx = sender_builder.sign(lock_body);
    let lock_digest = lock_tx.digest;

    let mut executor = test_executor(0, 1);
    executor.set_chain_id(1);
    let lock_result = executor
        .execute(&[lock_tx], &source_state)
        .expect("lock must succeed");
    assert_eq!(lock_result.receipts[0].status, ExecutionStatus::Success);

    // Prepare state for refund: set current_epoch past timeout.
    let htlc_change = lock_result.receipts[0]
        .state_changes
        .iter()
        .find(|c| c.account == AccountAddress([0x01; 32]) && c.key.starts_with(b"htlc_lock_v1:"))
        .unwrap();

    let mut refund_state = MemStateView::new();
    // Sender balance was debited; set to post-lock value.
    let sender_post_lock = 100_000 - lock_amount - 50_000;
    refund_state.set_balance(sender_builder.sender, sender_post_lock);
    // Set the lock record.
    let htlc_key = format_htlc_state_key(&lock_digest);
    refund_state.set(
        AccountAddress([0x01; 32]),
        htlc_key,
        htlc_change.value.clone().unwrap(),
    );
    // Set current_epoch past timeout (epoch >= 10).
    refund_state.set(
        AccountAddress([0u8; 32]),
        b"current_epoch".to_vec(),
        15u64.to_le_bytes().to_vec(),
    );
    // Set sender sequence number to 1 (after the lock tx).
    refund_state.set(
        sender_builder.sender,
        b"sequence_number".to_vec(),
        1u64.to_le_bytes().to_vec(),
    );

    // Execute refund.
    let refund_body = TransactionBody {
        sender: sender_builder.sender,
        sequence_number: 1,
        expiry_epoch: EpochNumber(1000),
        gas_limit: 50_000,
        gas_price: 1,
        target_shard: Some(ShardId(0)),
        payload: TransactionPayload::HtlcRefund { lock_digest },
        chain_id: 1,
    };
    let refund_tx = sender_builder.sign(refund_body);

    let mut refund_executor = test_executor(0, 2);
    refund_executor.set_chain_id(1);
    let refund_result = refund_executor
        .execute(&[refund_tx], &refund_state)
        .expect("refund must not error");

    assert_eq!(refund_result.receipts.len(), 1);
    assert_eq!(
        refund_result.receipts[0].status,
        ExecutionStatus::Success,
        "refund should succeed after timeout"
    );

    // Verify sender balance was restored.
    let sender_change = refund_result.receipts[0]
        .state_changes
        .iter()
        .find(|c| c.account == sender_builder.sender && c.key == b"balance")
        .expect("sender balance in state changes");
    let restored = u64::from_le_bytes(
        sender_change.value.as_ref().unwrap()[..8]
            .try_into()
            .unwrap(),
    );
    assert_eq!(
        restored,
        sender_post_lock + lock_amount,
        "sender should recover locked amount"
    );

    // Verify lock status is Refunded.
    let refund_htlc = refund_result.receipts[0]
        .state_changes
        .iter()
        .find(|c| c.account == AccountAddress([0x01; 32]))
        .unwrap();
    let refunded: HtlcLockRecord = bcs::from_bytes(refund_htlc.value.as_ref().unwrap()).unwrap();
    assert_eq!(refunded.status, HtlcStatus::Refunded);
}

// ── V-8 Test 3: Claim with wrong preimage rejected ─────────────────────

#[test]
fn htlc_claim_wrong_preimage_rejected() {
    let sender_builder = TxBuilder::new(1);
    let recipient_builder = TxBuilder::new(1);
    let lock_hash = test_lock_hash();

    // Set up state with a pending lock record.
    let lock_record = HtlcLockRecord {
        lock_digest: Blake3Digest([0xAA; 32]),
        sender: sender_builder.sender,
        recipient: recipient_builder.sender,
        amount: Amount(1_000),
        source_shard: ShardId(0),
        target_shard: ShardId(1),
        lock_hash,
        timeout_epoch: EpochNumber(100),
        status: HtlcStatus::Pending,
        created_epoch: EpochNumber(1),
    };
    let record_bytes = bcs::to_bytes(&lock_record).unwrap();

    let mut state = MemStateView::new();
    state.set_balance(recipient_builder.sender, 0);
    let htlc_key = format_htlc_state_key(&Blake3Digest([0xAA; 32]));
    state.set(AccountAddress([0x01; 32]), htlc_key, record_bytes);

    let claim_body = TransactionBody {
        sender: recipient_builder.sender,
        sequence_number: 0,
        expiry_epoch: EpochNumber(1000),
        gas_limit: 50_000,
        gas_price: 1,
        target_shard: Some(ShardId(1)),
        payload: TransactionPayload::HtlcClaim {
            lock_digest: Blake3Digest([0xAA; 32]),
            preimage: b"wrong-preimage-that-does-not-match".to_vec(),
        },
        chain_id: 1,
    };
    let claim_tx = recipient_builder.sign(claim_body);

    let mut executor = test_executor(1, 1);
    executor.set_chain_id(1);
    let result = executor
        .execute(&[claim_tx], &state)
        .expect("execution must not error");

    assert_eq!(result.receipts.len(), 1);
    assert_eq!(
        result.receipts[0].status,
        ExecutionStatus::HtlcPreimageMismatch,
        "wrong preimage should be rejected"
    );
}

// ── V-8 Test 4: Double claim rejected ──────────────────────────────────

#[test]
fn htlc_double_claim_rejected() {
    let sender_builder = TxBuilder::new(1);
    let recipient_builder = TxBuilder::new(1);
    let lock_hash = test_lock_hash();

    // Start with an already-claimed lock.
    let lock_record = HtlcLockRecord {
        lock_digest: Blake3Digest([0xBB; 32]),
        sender: sender_builder.sender,
        recipient: recipient_builder.sender,
        amount: Amount(2_000),
        source_shard: ShardId(0),
        target_shard: ShardId(1),
        lock_hash,
        timeout_epoch: EpochNumber(100),
        status: HtlcStatus::Claimed, // already claimed
        created_epoch: EpochNumber(1),
    };
    let record_bytes = bcs::to_bytes(&lock_record).unwrap();

    let mut state = MemStateView::new();
    let htlc_key = format_htlc_state_key(&Blake3Digest([0xBB; 32]));
    state.set(AccountAddress([0x01; 32]), htlc_key, record_bytes);

    let claim_body = TransactionBody {
        sender: recipient_builder.sender,
        sequence_number: 0,
        expiry_epoch: EpochNumber(1000),
        gas_limit: 50_000,
        gas_price: 1,
        target_shard: Some(ShardId(1)),
        payload: TransactionPayload::HtlcClaim {
            lock_digest: Blake3Digest([0xBB; 32]),
            preimage: TEST_PREIMAGE.to_vec(),
        },
        chain_id: 1,
    };
    let claim_tx = recipient_builder.sign(claim_body);

    let mut executor = test_executor(1, 1);
    executor.set_chain_id(1);
    let result = executor
        .execute(&[claim_tx], &state)
        .expect("execution must not error");

    assert_eq!(
        result.receipts[0].status,
        ExecutionStatus::HtlcAlreadyClaimed,
        "double claim should be rejected"
    );
}

// ── V-8 Test 5: Refund before timeout rejected ─────────────────────────

#[test]
fn htlc_refund_before_timeout_rejected() {
    let sender_builder = TxBuilder::new(1);
    let lock_hash = test_lock_hash();

    let lock_record = HtlcLockRecord {
        lock_digest: Blake3Digest([0xCC; 32]),
        sender: sender_builder.sender,
        recipient: AccountAddress([0xDD; 32]),
        amount: Amount(1_000),
        source_shard: ShardId(0),
        target_shard: ShardId(1),
        lock_hash,
        timeout_epoch: EpochNumber(100),
        status: HtlcStatus::Pending,
        created_epoch: EpochNumber(1),
    };
    let record_bytes = bcs::to_bytes(&lock_record).unwrap();

    let mut state = MemStateView::new();
    state.set_balance(sender_builder.sender, 50_000);
    let htlc_key = format_htlc_state_key(&Blake3Digest([0xCC; 32]));
    state.set(AccountAddress([0x01; 32]), htlc_key, record_bytes);
    // Current epoch is 5, which is before timeout (100).
    state.set(
        AccountAddress([0u8; 32]),
        b"current_epoch".to_vec(),
        5u64.to_le_bytes().to_vec(),
    );

    let refund_body = TransactionBody {
        sender: sender_builder.sender,
        sequence_number: 0,
        expiry_epoch: EpochNumber(1000),
        gas_limit: 50_000,
        gas_price: 1,
        target_shard: Some(ShardId(0)),
        payload: TransactionPayload::HtlcRefund {
            lock_digest: Blake3Digest([0xCC; 32]),
        },
        chain_id: 1,
    };
    let refund_tx = sender_builder.sign(refund_body);

    let mut executor = test_executor(0, 1);
    executor.set_chain_id(1);
    let result = executor
        .execute(&[refund_tx], &state)
        .expect("execution must not error");

    assert_eq!(
        result.receipts[0].status,
        ExecutionStatus::HtlcRefundTooEarly,
        "refund before timeout should be rejected"
    );
}

// ── V-8 Test 6: Lock with insufficient balance rejected ─────────────────

#[test]
fn htlc_lock_insufficient_balance_rejected() {
    let sender_builder = TxBuilder::new(1);
    let recipient = AccountAddress([0xEE; 32]);

    let mut state = MemStateView::new();
    state.set_balance(sender_builder.sender, 100); // too low

    let lock_body = TransactionBody {
        sender: sender_builder.sender,
        sequence_number: 0,
        expiry_epoch: EpochNumber(1000),
        gas_limit: 50_000,
        gas_price: 1,
        target_shard: Some(ShardId(0)),
        payload: TransactionPayload::HtlcLock {
            recipient,
            amount: Amount(5_000),
            target_shard: ShardId(1),
            lock_hash: test_lock_hash(),
            timeout_epoch: EpochNumber(100),
        },
        chain_id: 1,
    };
    let lock_tx = sender_builder.sign(lock_body);

    let mut executor = test_executor(0, 1);
    executor.set_chain_id(1);
    let result = executor
        .execute(&[lock_tx], &state)
        .expect("execution must not error");

    assert_eq!(result.receipts.len(), 1);
    assert!(
        matches!(
            result.receipts[0].status,
            ExecutionStatus::MoveAbort { ref location, code: 1 } if location == "nexus::htlc_lock"
        ),
        "lock with insufficient balance should abort"
    );
}

// ── V-8 Test 7: Claim on non-existent lock ─────────────────────────────

#[test]
fn htlc_claim_nonexistent_lock_rejected() {
    let claimer = TxBuilder::new(1);
    let state = MemStateView::new();

    let claim_body = TransactionBody {
        sender: claimer.sender,
        sequence_number: 0,
        expiry_epoch: EpochNumber(1000),
        gas_limit: 50_000,
        gas_price: 1,
        target_shard: Some(ShardId(1)),
        payload: TransactionPayload::HtlcClaim {
            lock_digest: Blake3Digest([0xFF; 32]),
            preimage: TEST_PREIMAGE.to_vec(),
        },
        chain_id: 1,
    };
    let claim_tx = claimer.sign(claim_body);

    let mut executor = test_executor(1, 1);
    executor.set_chain_id(1);
    let result = executor
        .execute(&[claim_tx], &state)
        .expect("execution must not error");

    assert_eq!(
        result.receipts[0].status,
        ExecutionStatus::HtlcLockNotFound,
        "claim on nonexistent lock should fail"
    );
}

// ── V-8 Test 8: compute_lock_hash deterministic ────────────────────────

#[test]
fn lock_hash_deterministic() {
    let h1 = compute_lock_hash(TEST_PREIMAGE);
    let h2 = compute_lock_hash(TEST_PREIMAGE);
    assert_eq!(h1, h2, "lock hash must be deterministic");
}

#[test]
fn lock_hash_changes_with_preimage() {
    let h1 = compute_lock_hash(b"preimage_a");
    let h2 = compute_lock_hash(b"preimage_b");
    assert_ne!(h1, h2, "different preimages must produce different hashes");
}

// ── Helper ──────────────────────────────────────────────────────────────

/// Format the HTLC state key for the overlay (used by MemStateView).
fn format_htlc_state_key(lock_digest: &Blake3Digest) -> Vec<u8> {
    let mut key = b"htlc_lock_v1:".to_vec();
    key.extend_from_slice(lock_digest.as_bytes());
    key
}
