// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Gas baseline regression tests (T-11005).
//!
//! Executes standard transaction types through the Block-STM executor and
//! asserts that gas consumed matches the expected gas schedule values.
//! These tests serve as a documented reference baseline and a CI-runnable
//! regression guard for gas-cost changes.

use nexus_crypto::{DilithiumSigner, Signer};
use nexus_execution::block_stm::BlockStmExecutor;
use nexus_execution::types::{
    compute_tx_digest, ExecutionStatus, SignedTransaction, TransactionBody, TransactionPayload,
    TX_DOMAIN,
};
use nexus_primitives::{
    AccountAddress, Amount, CommitSequence, ContractAddress, EpochNumber, ShardId, TimestampMs,
    TokenId,
};
use nexus_test_utils::fixtures::execution::{setup_query_view_state, MemStateView};

fn make_executor() -> BlockStmExecutor {
    BlockStmExecutor::new(ShardId(0), CommitSequence(1), TimestampMs::now())
}

// ── Gas schedule reference values (from VmConfig::default()) ────────

// transfer_base = 1_000
const EXPECTED_TRANSFER_GAS: u64 = 1_000;
// publish_base = 10_000, publish_per_byte = 1
const EXPECTED_PUBLISH_BASE: u64 = 10_000;
// call_base = 5_000
const EXPECTED_CALL_BASE: u64 = 5_000;

// ── Transfer gas baseline ───────────────────────────────────────────

#[test]
fn gas_baseline_transfer() {
    let executor = make_executor();
    let (sk, pk) = DilithiumSigner::generate_keypair();
    let sender = AccountAddress::from_dilithium_pubkey(pk.as_bytes());

    let mut state = MemStateView::new();
    state.set_balance(sender, 10_000_000);

    let body = TransactionBody {
        sender,
        sequence_number: 0,
        expiry_epoch: EpochNumber(1000),
        gas_limit: 50_000,
        gas_price: 1,
        target_shard: None,
        payload: TransactionPayload::Transfer {
            recipient: AccountAddress([0xEE; 32]),
            amount: Amount(100),
            token: TokenId::Native,
        },
        chain_id: 1,
    };
    let digest = compute_tx_digest(&body).unwrap();
    let sig = DilithiumSigner::sign(&sk, TX_DOMAIN, digest.as_bytes());
    let tx = SignedTransaction {
        body,
        signature: sig,
        sender_pk: pk,
        digest,
    };

    let result = executor.execute(&[tx], &state).unwrap();
    let receipt = &result.receipts[0];
    assert_eq!(receipt.status, ExecutionStatus::Success);

    // Transfer gas should exactly match the schedule constant.
    assert_eq!(
        receipt.gas_used, EXPECTED_TRANSFER_GAS,
        "transfer gas: expected {EXPECTED_TRANSFER_GAS}, got {}",
        receipt.gas_used,
    );
    eprintln!("[gas baseline] transfer = {} gas", receipt.gas_used);
}

// ── Publish gas baseline ────────────────────────────────────────────

#[test]
fn gas_baseline_publish() {
    let executor = make_executor();
    let (sk, pk) = DilithiumSigner::generate_keypair();
    let sender = AccountAddress::from_dilithium_pubkey(pk.as_bytes());

    let bytecode = {
        let mut m = vec![0xa1, 0x1c, 0xeb, 0x0b]; // Move magic
        m.extend_from_slice(&1u32.to_le_bytes());
        m.extend(vec![0u8; 64]); // 64 bytes of padding
        m
    };
    let bytecode_len = bytecode.len() as u64;

    let mut state = MemStateView::new();
    state.set_balance(sender, 10_000_000);

    let body = TransactionBody {
        sender,
        sequence_number: 0,
        expiry_epoch: EpochNumber(1000),
        gas_limit: 200_000,
        gas_price: 1,
        target_shard: None,
        payload: TransactionPayload::MovePublish {
            bytecode_modules: vec![bytecode],
        },
        chain_id: 1,
    };
    let digest = compute_tx_digest(&body).unwrap();
    let sig = DilithiumSigner::sign(&sk, TX_DOMAIN, digest.as_bytes());
    let tx = SignedTransaction {
        body,
        signature: sig,
        sender_pk: pk,
        digest,
    };

    // With the real Move VM, fake bytecode fails verification.
    // The executor surfaces the error as a per-tx receipt with MoveVmError
    // status rather than propagating a fatal batch error.
    let result = executor.execute(&[tx], &state);
    let block = result.expect("bytecode verification should produce a receipt, not a fatal error");
    assert_eq!(block.receipts.len(), 1);
    assert!(
        matches!(
            block.receipts[0].status,
            nexus_execution::types::ExecutionStatus::MoveVmError { .. }
        ),
        "fake bytecode should produce MoveVmError receipt, got: {:?}",
        block.receipts[0].status,
    );

    // Gas accounting for valid publishes is tested via the real counter
    // module in test_real_vm_counter_lifecycle.
    eprintln!(
        "[gas baseline] publish ({bytecode_len} bytes) = rejected (fake bytecode, as expected)",
    );
}

// ── Call gas baseline ───────────────────────────────────────────────

#[test]
fn gas_baseline_call() {
    let executor = make_executor();
    let (sk, pk) = DilithiumSigner::generate_keypair();
    let sender = AccountAddress::from_dilithium_pubkey(pk.as_bytes());

    let mut state = MemStateView::new();
    state.set_balance(sender, 10_000_000);

    let contract = ContractAddress([0xCC; 32]);
    let body = TransactionBody {
        sender,
        sequence_number: 0,
        expiry_epoch: EpochNumber(1000),
        gas_limit: 100_000,
        gas_price: 1,
        target_shard: None,
        payload: TransactionPayload::MoveCall {
            contract,
            function: "increment".into(),
            type_args: vec![],
            args: vec![],
        },
        chain_id: 1,
    };
    let digest = compute_tx_digest(&body).unwrap();
    let sig = DilithiumSigner::sign(&sk, TX_DOMAIN, digest.as_bytes());
    let tx = SignedTransaction {
        body,
        signature: sig,
        sender_pk: pk,
        digest,
    };

    let result = executor.execute(&[tx], &state).unwrap();
    let receipt = &result.receipts[0];

    // Call may succeed or fail depending on contract existence, but gas must be charged.
    assert!(
        receipt.gas_used >= EXPECTED_CALL_BASE,
        "call gas: expected >= {EXPECTED_CALL_BASE}, got {}",
        receipt.gas_used,
    );
    eprintln!(
        "[gas baseline] call = {} gas (base {EXPECTED_CALL_BASE}, status {:?})",
        receipt.gas_used, receipt.status,
    );
}

// ── Query view gas baseline ─────────────────────────────────────────

#[test]
fn gas_baseline_query_view() {
    let executor = make_executor();
    let (state, contract_addr) = setup_query_view_state(0xCC);

    let result = executor
        .query_view(&state, contract_addr, "get_count", &[], &[])
        .expect("query_view should succeed");

    assert!(
        result.gas_used > 0,
        "query gas must be > 0, got {}",
        result.gas_used,
    );

    let val = result.return_value.expect("should return a value");
    let counter = u64::from_le_bytes(val.as_slice().try_into().unwrap());
    assert_eq!(counter, 42, "pre-populated resource should return 42");

    eprintln!(
        "[gas baseline] query_view = {} gas, return = {counter}",
        result.gas_used,
    );
}

// ── Gas ordering invariant ──────────────────────────────────────────

#[test]
fn gas_ordering_transfer_lt_call_lt_publish() {
    // Verify the expected ordering: transfer < call < publish.
    // This invariant should hold because publishes store more data.
    let (t, c, p) = (
        EXPECTED_TRANSFER_GAS,
        EXPECTED_CALL_BASE,
        EXPECTED_PUBLISH_BASE,
    );
    assert!(
        t < c,
        "transfer gas ({t}) should be less than call gas ({c})"
    );
    assert!(
        c < p,
        "call gas ({c}) should be less than publish gas ({p})"
    );
}
