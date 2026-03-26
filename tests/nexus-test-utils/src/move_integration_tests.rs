//! Move contract engine integration test matrix (T-11004).
//!
//! End-to-end tests covering the full publish → invoke → query pipeline
//! through Block-STM → MoveExecutor → NexusMoveVm (ABI-driven dispatch).
//!
//! These tests target the ABI-driven NexusMoveVm fallback.  When the
//! `move-vm` feature is active the real MoveRuntime provides different
//! error paths so this entire test module is skipped via a cfg gate in
//! lib.rs.
//!
//! Test matrix:
//! ┌──────────────────────┬───────────────────────────────────────────┐
//! │ Category             │ Cases                                     │
//! ├──────────────────────┼───────────────────────────────────────────┤
//! │ Publish              │ valid, invalid bytecode, out-of-gas,      │
//! │                      │ duplicate module, module too small,       │
//! │                      │ too many modules, total size exceeded,    │
//! │                      │ overwrite rejected, multi-module,         │
//! │                      │ metadata storage keys                     │
//! ├──────────────────────┼───────────────────────────────────────────┤
//! │ Invoke               │ counter increment, transfer, OOG, abort,  │
//! │                      │ arg mismatch, function not found,         │
//! │                      │ insufficient balance                      │
//! ├──────────────────────┼───────────────────────────────────────────┤
//! │ Query                │ view function success, contract missing,  │
//! │                      │ function missing, no state mutation       │
//! ├──────────────────────┼───────────────────────────────────────────┤
//! │ Pipeline             │ publish → invoke → query in sequence,     │
//! │                      │ multi-contract interaction, address       │
//! │                      │ determinism, two publishes same block     │
//! ├──────────────────────┼───────────────────────────────────────────┤
//! │ Failure              │ gas exhaustion, mixed block, gas          │
//! │                      │ accounting, empty block, large batch      │
//! └──────────────────────┴───────────────────────────────────────────┘

use nexus_crypto::{DilithiumSigner, Signer};
use nexus_execution::types::TX_DOMAIN;
use nexus_execution::{
    compute_tx_digest, BlockStmExecutor, ExecutionStatus, SignedTransaction, StateView,
    TransactionBody, TransactionPayload,
};
use nexus_primitives::{
    AccountAddress, Amount, CommitSequence, ContractAddress, EpochNumber, ShardId, TimestampMs,
    TokenId,
};
use std::collections::HashMap;

// ── Test state view ─────────────────────────────────────────────────────

struct MemState {
    data: HashMap<(AccountAddress, Vec<u8>), Vec<u8>>,
}

impl MemState {
    fn new() -> Self {
        Self {
            data: HashMap::new(),
        }
    }

    fn set(&mut self, account: AccountAddress, key: &[u8], value: Vec<u8>) {
        self.data.insert((account, key.to_vec()), value);
    }

    fn with_balance(mut self, account: AccountAddress, balance: u64) -> Self {
        self.set(account, b"balance", balance.to_le_bytes().to_vec());
        self
    }
}

impl StateView for MemState {
    fn get(
        &self,
        account: &AccountAddress,
        key: &[u8],
    ) -> nexus_execution::ExecutionResult<Option<Vec<u8>>> {
        Ok(self.data.get(&(*account, key.to_vec())).cloned())
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────

fn addr(b: u8) -> AccountAddress {
    AccountAddress([b; 32])
}

fn make_executor() -> BlockStmExecutor {
    BlockStmExecutor::new(ShardId(0), CommitSequence(1), TimestampMs::now())
}

fn sign_tx(body: TransactionBody) -> SignedTransaction {
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

/// Make valid Move bytecode for testing (Move magic + version + padding).
fn make_test_bytecode(padding: usize) -> Vec<u8> {
    let mut module = vec![0xa1, 0x1c, 0xeb, 0x0b]; // Move magic
    module.extend_from_slice(&1u32.to_le_bytes()); // Version 1
    module.extend(vec![0u8; padding]);
    module
}

fn make_publish_tx(
    sender: AccountAddress,
    modules: Vec<Vec<u8>>,
    gas_limit: u64,
) -> SignedTransaction {
    sign_tx(TransactionBody {
        sender,
        sequence_number: 0,
        expiry_epoch: EpochNumber(1000),
        gas_limit,
        gas_price: 1,
        target_shard: None,
        payload: TransactionPayload::MovePublish {
            bytecode_modules: modules,
        },
        chain_id: 1,
    })
}

fn make_call_tx(
    sender: AccountAddress,
    contract: ContractAddress,
    function: &str,
    args: Vec<Vec<u8>>,
    gas_limit: u64,
) -> SignedTransaction {
    sign_tx(TransactionBody {
        sender,
        sequence_number: 0,
        expiry_epoch: EpochNumber(1000),
        gas_limit,
        gas_price: 1,
        target_shard: None,
        payload: TransactionPayload::MoveCall {
            contract,
            function: function.into(),
            type_args: vec![],
            args,
        },
        chain_id: 1,
    })
}

fn make_transfer_tx(
    sender: AccountAddress,
    recipient: AccountAddress,
    amount: u64,
    gas_limit: u64,
) -> SignedTransaction {
    sign_tx(TransactionBody {
        sender,
        sequence_number: 0,
        expiry_epoch: EpochNumber(1000),
        gas_limit,
        gas_price: 1,
        target_shard: None,
        payload: TransactionPayload::Transfer {
            recipient,
            amount: Amount(amount),
            token: TokenId::Native,
        },
        chain_id: 1,
    })
}

// ═══════════════════════════════════════════════════════════════════════
// PUBLISH TESTS
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn publish_valid_module() {
    let executor = make_executor();
    let state = MemState::new();

    let tx = make_publish_tx(addr(0xAA), vec![make_test_bytecode(32)], 100_000);
    let result = executor.execute(&[tx], &state).unwrap();

    assert_eq!(result.receipts.len(), 1);
    assert_eq!(result.receipts[0].status, ExecutionStatus::Success);
    assert!(result.receipts[0].gas_used > 0);
    assert!(!result.receipts[0].state_changes.is_empty());
}

#[test]
fn publish_invalid_magic_bytes() {
    let executor = make_executor();
    let state = MemState::new();

    let tx = make_publish_tx(addr(0xAA), vec![vec![0xFF; 16]], 100_000);
    let result = executor.execute(&[tx], &state).unwrap();

    assert!(matches!(
        result.receipts[0].status,
        ExecutionStatus::MoveAbort { code: 12, .. }
    ));
}

#[test]
fn publish_empty_modules_rejected() {
    let executor = make_executor();
    let state = MemState::new();

    let tx = make_publish_tx(addr(0xAA), vec![], 100_000);
    let result = executor.execute(&[tx], &state).unwrap();

    assert!(matches!(
        result.receipts[0].status,
        ExecutionStatus::MoveAbort { code: 10, .. }
    ));
}

#[test]
fn publish_out_of_gas() {
    let executor = make_executor();
    let state = MemState::new();

    let tx = make_publish_tx(addr(0xAA), vec![make_test_bytecode(32)], 10);
    let result = executor.execute(&[tx], &state).unwrap();

    assert_eq!(result.receipts[0].status, ExecutionStatus::OutOfGas);
}

#[test]
fn publish_duplicate_modules_rejected() {
    let executor = make_executor();
    let state = MemState::new();

    let m = make_test_bytecode(16);
    let tx = make_publish_tx(addr(0xAA), vec![m.clone(), m], 100_000);
    let result = executor.execute(&[tx], &state).unwrap();

    assert!(matches!(
        result.receipts[0].status,
        ExecutionStatus::MoveAbort { code: 16, .. }
    ));
}

// ═══════════════════════════════════════════════════════════════════════
// INVOKE TESTS
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn invoke_contract_not_found() {
    let executor = make_executor();
    let state = MemState::new();

    let tx = make_call_tx(
        addr(1),
        ContractAddress([0xDD; 32]),
        "do_thing",
        vec![],
        50_000,
    );
    let result = executor.execute(&[tx], &state).unwrap();

    // With gas_limit 50_000 > call_base, we get MODULE_NOT_FOUND (code 2).
    assert!(matches!(
        result.receipts[0].status,
        ExecutionStatus::MoveAbort { code: 2, .. }
    ));
}

#[test]
fn invoke_out_of_gas() {
    let executor = make_executor();
    let state = MemState::new();

    let tx = make_call_tx(
        addr(1),
        ContractAddress([0xDD; 32]),
        "do_thing",
        vec![],
        100,
    );
    let result = executor.execute(&[tx], &state).unwrap();

    assert_eq!(result.receipts[0].status, ExecutionStatus::OutOfGas);
}

// ═══════════════════════════════════════════════════════════════════════
// PIPELINE TESTS (publish → invoke sequential)
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn native_transfer_e2e() {
    let executor = make_executor();
    let state = MemState::new()
        .with_balance(addr(1), 100_000)
        .with_balance(addr(2), 0);

    let tx = make_transfer_tx(addr(1), addr(2), 1_000, 50_000);
    let result = executor.execute(&[tx], &state).unwrap();

    assert_eq!(result.receipts.len(), 1);
    assert_eq!(result.receipts[0].status, ExecutionStatus::Success);
    assert_eq!(result.receipts[0].gas_used, 1_000);
}

#[test]
fn transfer_insufficient_balance() {
    let executor = make_executor();
    let state = MemState::new().with_balance(addr(1), 100);

    let tx = make_transfer_tx(addr(1), addr(2), 999_999, 50_000);
    let result = executor.execute(&[tx], &state).unwrap();

    assert!(matches!(
        result.receipts[0].status,
        ExecutionStatus::MoveAbort { code: 1, .. }
    ));
}

#[test]
fn multiple_transactions_in_block() {
    let executor = make_executor();
    let state = MemState::new()
        .with_balance(addr(1), 1_000_000)
        .with_balance(addr(2), 1_000_000);

    let txs = vec![
        make_transfer_tx(addr(1), addr(3), 100, 50_000),
        make_transfer_tx(addr(2), addr(4), 200, 50_000),
    ];
    let result = executor.execute(&txs, &state).unwrap();

    assert_eq!(result.receipts.len(), 2);
    assert_eq!(result.receipts[0].status, ExecutionStatus::Success);
    assert_eq!(result.receipts[1].status, ExecutionStatus::Success);
    assert!(result.gas_used_total > 0);
}

#[test]
fn publish_then_invoke_sequential_blocks() {
    // Block 1: publish a module.
    let executor = make_executor();
    let state = MemState::new();

    let publish_tx = make_publish_tx(addr(0xAA), vec![make_test_bytecode(16)], 100_000);
    let result = executor.execute(&[publish_tx], &state).unwrap();
    assert_eq!(result.receipts[0].status, ExecutionStatus::Success);

    // The published contract address is deterministic.
    // In block 2, we could call it — but since the state isn't persistent
    // in this test, we verify the state changes include the contract code.
    let has_code = result.receipts[0]
        .state_changes
        .iter()
        .any(|sc| sc.key == b"code");
    assert!(has_code, "publish should store code");

    let has_metadata = result.receipts[0]
        .state_changes
        .iter()
        .any(|sc| sc.key == b"package_metadata");
    assert!(has_metadata, "publish should store package metadata");
}

// ═══════════════════════════════════════════════════════════════════════
// FAILURE CASE TESTS
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn block_with_mixed_success_and_failure() {
    let executor = make_executor();
    let state = MemState::new()
        .with_balance(addr(1), 1_000_000)
        .with_balance(addr(2), 0); // Empty balance

    let txs = vec![
        make_transfer_tx(addr(1), addr(3), 100, 50_000), // Should succeed
        make_transfer_tx(addr(2), addr(4), 999_999, 50_000), // Should fail
    ];
    let result = executor.execute(&txs, &state).unwrap();

    assert_eq!(result.receipts.len(), 2);
    assert_eq!(result.receipts[0].status, ExecutionStatus::Success);
    assert!(matches!(
        result.receipts[1].status,
        ExecutionStatus::MoveAbort { .. }
    ));
}

#[test]
fn gas_accounting_consistency() {
    let executor = make_executor();
    let state = MemState::new().with_balance(addr(1), 1_000_000);

    let txs = vec![
        make_transfer_tx(addr(1), addr(2), 100, 50_000),
        make_transfer_tx(addr(1), addr(3), 200, 50_000),
    ];
    let result = executor.execute(&txs, &state).unwrap();

    let sum: u64 = result.receipts.iter().map(|r| r.gas_used).sum();
    assert_eq!(result.gas_used_total, sum);
}

// ═══════════════════════════════════════════════════════════════════════
// PUBLISH → INVOKE → QUERY PIPELINE (T-11003)
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn publish_then_query_view_pipeline() {
    // Step 1: Publish a valid module.
    let executor = make_executor();
    let state = MemState::new();

    let bytecode = make_test_bytecode(16);
    let publish_tx = make_publish_tx(addr(0xAA), vec![bytecode.clone()], 100_000);
    let result = executor.execute(&[publish_tx], &state).unwrap();
    assert_eq!(result.receipts[0].status, ExecutionStatus::Success);

    // Verify publish stored code and package_metadata.
    let has_code = result.receipts[0]
        .state_changes
        .iter()
        .any(|sc| sc.key == b"code");
    assert!(has_code, "publish must store code");
    let has_metadata = result.receipts[0]
        .state_changes
        .iter()
        .any(|sc| sc.key == b"package_metadata");
    assert!(has_metadata, "publish must store package_metadata");

    // Step 2: Verify query_view returns error for contract without ABI.
    //
    // Publish stores empty ABI by default, so querying a view function
    // should report "no ABI published".
    let contract_addr = result.receipts[0]
        .state_changes
        .iter()
        .find(|sc| sc.key == b"code")
        .map(|sc| sc.account)
        .expect("code state change must have an account");

    // Build a state view from the publish state changes.
    let mut post_state = MemState::new();
    for sc in &result.receipts[0].state_changes {
        if let Some(ref val) = sc.value {
            post_state.set(sc.account, &sc.key, val.clone());
        }
    }

    let query_err = executor.query_view(&post_state, contract_addr, "get_count", &[], &[]);
    assert!(
        query_err.is_err(),
        "query should fail with empty ABI after basic publish"
    );
}

#[test]
fn query_view_contract_not_found() {
    let executor = make_executor();
    let state = MemState::new();

    let result = executor.query_view(&state, addr(0xDD), "get_count", &[], &[]);
    assert!(
        result.is_err(),
        "query should fail when contract does not exist"
    );
}

// ═══════════════════════════════════════════════════════════════════════
// T-11004 EXPANSION: PUBLISH — VERIFICATION CODES 11-16
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn publish_module_too_small() {
    let executor = make_executor();
    let state = MemState::new();

    // Module < 8 bytes (minimum magic + version header) → code 11.
    let tx = make_publish_tx(addr(0xAA), vec![vec![0xa1, 0x1c, 0xeb]], 100_000);
    let result = executor.execute(&[tx], &state).unwrap();

    assert!(matches!(
        result.receipts[0].status,
        ExecutionStatus::MoveAbort { code: 11, .. }
    ));
}

#[test]
fn publish_too_many_modules() {
    let executor = make_executor();
    let state = MemState::new();

    // 65 modules (max is 64) → code 13.
    let modules: Vec<Vec<u8>> = (0..65).map(|i| make_test_bytecode(16 + i)).collect();
    let tx = make_publish_tx(addr(0xAA), modules, 500_000);
    let result = executor.execute(&[tx], &state).unwrap();

    assert!(matches!(
        result.receipts[0].status,
        ExecutionStatus::MoveAbort { code: 13, .. }
    ));
}

#[test]
fn publish_total_size_exceeded() {
    let executor = make_executor();
    let state = MemState::new();

    // Two modules whose combined size exceeds the default 512 KiB limit → code 15.
    // Each module: 8 bytes header + 300KB padding = ~300KB, two of them = ~600KB > 512KB.
    let modules = vec![
        make_test_bytecode(300 * 1024),
        make_test_bytecode(300 * 1024 + 1), // +1 to avoid duplicate detection
    ];
    let tx = make_publish_tx(addr(0xAA), modules, 1_000_000);
    let result = executor.execute(&[tx], &state).unwrap();

    assert!(matches!(
        result.receipts[0].status,
        ExecutionStatus::MoveAbort { code: 15, .. }
    ));
}

#[test]
fn publish_overwrite_rejected() {
    let executor = make_executor();
    let state = MemState::new();

    // First publish succeeds.
    let bytecode = make_test_bytecode(16);
    let tx1 = make_publish_tx(addr(0xAA), vec![bytecode.clone()], 100_000);
    let result1 = executor.execute(&[tx1], &state).unwrap();
    assert_eq!(result1.receipts[0].status, ExecutionStatus::Success);

    // Build post-state from first publish.
    let mut post_state = MemState::new();
    for sc in &result1.receipts[0].state_changes {
        if let Some(ref val) = sc.value {
            post_state.set(sc.account, &sc.key, val.clone());
        }
    }

    // Second publish with same deployer + same bytecode = same address → code 20.
    let tx2 = make_publish_tx(addr(0xAA), vec![bytecode], 100_000);
    let result2 = executor.execute(&[tx2], &post_state).unwrap();

    assert!(matches!(
        result2.receipts[0].status,
        ExecutionStatus::MoveAbort { code: 20, .. }
    ));
}

#[test]
fn publish_multiple_modules_valid() {
    let executor = make_executor();
    let state = MemState::new();

    // Two distinct modules in one transaction.
    let modules = vec![make_test_bytecode(16), make_test_bytecode(32)];
    let tx = make_publish_tx(addr(0xAA), modules, 100_000);
    let result = executor.execute(&[tx], &state).unwrap();

    assert_eq!(result.receipts[0].status, ExecutionStatus::Success);
    assert!(result.receipts[0].gas_used > 0);
}

#[test]
fn publish_stores_all_metadata_keys() {
    let executor = make_executor();
    let state = MemState::new();

    let tx = make_publish_tx(addr(0xBB), vec![make_test_bytecode(24)], 100_000);
    let result = executor.execute(&[tx], &state).unwrap();
    assert_eq!(result.receipts[0].status, ExecutionStatus::Success);

    let keys: Vec<&[u8]> = result.receipts[0]
        .state_changes
        .iter()
        .map(|sc| sc.key.as_slice())
        .collect();

    // The publisher stores: code, code_hash, deployer, module_count.
    // The NexusMoveVm adds: package_metadata.
    assert!(keys.contains(&b"code".as_slice()), "must store code");
    assert!(
        keys.contains(&b"code_hash".as_slice()),
        "must store code_hash"
    );
    assert!(
        keys.contains(&b"deployer".as_slice()),
        "must store deployer"
    );
    assert!(
        keys.contains(&b"module_count".as_slice()),
        "must store module_count"
    );
    assert!(
        keys.contains(&b"package_metadata".as_slice()),
        "must store package_metadata"
    );

    // Verify deployer matches sender.
    let deployer_val = result.receipts[0]
        .state_changes
        .iter()
        .find(|sc| sc.key == b"deployer")
        .and_then(|sc| sc.value.as_ref())
        .expect("deployer must have value");
    assert_eq!(deployer_val.as_slice(), &addr(0xBB).0);
}

// ═══════════════════════════════════════════════════════════════════════
// T-11004 EXPANSION: INVOKE — DISPATCH PATHS
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn invoke_function_not_found_after_publish() {
    let executor = make_executor();
    let state = MemState::new();

    // Publish a module.
    let tx_pub = make_publish_tx(addr(0xCC), vec![make_test_bytecode(16)], 100_000);
    let pub_result = executor.execute(&[tx_pub], &state).unwrap();
    assert_eq!(pub_result.receipts[0].status, ExecutionStatus::Success);

    // Build post-state from publish.
    let mut post_state = MemState::new();
    for sc in &pub_result.receipts[0].state_changes {
        if let Some(ref val) = sc.value {
            post_state.set(sc.account, &sc.key, val.clone());
        }
    }

    // The published contract has an empty ABI, so calling any function
    // should yield FUNCTION_NOT_FOUND (code 3).
    let contract_addr = pub_result.receipts[0]
        .state_changes
        .iter()
        .find(|sc| sc.key == b"code")
        .map(|sc| sc.account)
        .unwrap();

    let tx_call = make_call_tx(
        addr(1),
        ContractAddress(contract_addr.0),
        "nonexistent_fn",
        vec![],
        50_000,
    );
    let call_result = executor.execute(&[tx_call], &post_state).unwrap();

    assert!(matches!(
        call_result.receipts[0].status,
        ExecutionStatus::MoveAbort { code: 3, .. }
    ));
}

#[test]
fn invoke_gas_below_call_base() {
    // If gas_limit < call_base_gas (default 5000), the call should
    // immediately return OutOfGas without even checking the contract.
    let executor = make_executor();
    let state = MemState::new();

    let tx = make_call_tx(
        addr(1),
        ContractAddress([0xDD; 32]),
        "anything",
        vec![],
        10, // Well below call_base of 5000
    );
    let result = executor.execute(&[tx], &state).unwrap();

    assert_eq!(result.receipts[0].status, ExecutionStatus::OutOfGas);
}

// ═══════════════════════════════════════════════════════════════════════
// T-11004 EXPANSION: PIPELINE — MULTI-CONTRACT & DETERMINISM
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn contract_address_determinism() {
    let executor = make_executor();
    let state = MemState::new();

    let bytecode = make_test_bytecode(42);

    // Publish same bytecode from same deployer twice (via two separate executions).
    let tx1 = make_publish_tx(addr(0xAA), vec![bytecode.clone()], 100_000);
    let result1 = executor.execute(&[tx1], &state).unwrap();

    let tx2 = make_publish_tx(addr(0xAA), vec![bytecode], 100_000);
    let result2 = executor.execute(&[tx2], &state).unwrap();

    // Both should produce the same contract address.
    let addr1 = result1.receipts[0]
        .state_changes
        .iter()
        .find(|sc| sc.key == b"code")
        .unwrap()
        .account;
    let addr2 = result2.receipts[0]
        .state_changes
        .iter()
        .find(|sc| sc.key == b"code")
        .unwrap()
        .account;

    assert_eq!(
        addr1, addr2,
        "same deployer + same bytecode = same contract address"
    );
}

#[test]
fn different_deployers_different_addresses() {
    let executor = make_executor();
    let state = MemState::new();

    let bytecode = make_test_bytecode(42);

    let tx1 = make_publish_tx(addr(0xAA), vec![bytecode.clone()], 100_000);
    let result1 = executor.execute(&[tx1], &state).unwrap();

    let tx2 = make_publish_tx(addr(0xBB), vec![bytecode], 100_000);
    let result2 = executor.execute(&[tx2], &state).unwrap();

    let addr1 = result1.receipts[0]
        .state_changes
        .iter()
        .find(|sc| sc.key == b"code")
        .unwrap()
        .account;
    let addr2 = result2.receipts[0]
        .state_changes
        .iter()
        .find(|sc| sc.key == b"code")
        .unwrap()
        .account;

    assert_ne!(
        addr1, addr2,
        "different deployers must produce different contract addresses"
    );
}

#[test]
fn publish_two_contracts_same_block() {
    let executor = make_executor();
    let state = MemState::new();

    // Two different deployers publishing different bytecode in one block.
    let tx1 = make_publish_tx(addr(0xAA), vec![make_test_bytecode(16)], 100_000);
    let tx2 = make_publish_tx(addr(0xBB), vec![make_test_bytecode(32)], 100_000);
    let result = executor.execute(&[tx1, tx2], &state).unwrap();

    assert_eq!(result.receipts.len(), 2);
    assert_eq!(result.receipts[0].status, ExecutionStatus::Success);
    assert_eq!(result.receipts[1].status, ExecutionStatus::Success);
}

// ═══════════════════════════════════════════════════════════════════════
// T-11004 EXPANSION: QUERY — EDGE CASES
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn query_view_no_state_mutation() {
    let executor = make_executor();
    let state = MemState::new();

    // Publish a module.
    let publish_tx = make_publish_tx(addr(0xCC), vec![make_test_bytecode(16)], 100_000);
    let pub_result = executor.execute(&[publish_tx], &state).unwrap();
    assert_eq!(pub_result.receipts[0].status, ExecutionStatus::Success);

    // Build post-state from publish.
    let mut post_state = MemState::new();
    for sc in &pub_result.receipts[0].state_changes {
        if let Some(ref val) = sc.value {
            post_state.set(sc.account, &sc.key, val.clone());
        }
    }

    let contract_addr = pub_result.receipts[0]
        .state_changes
        .iter()
        .find(|sc| sc.key == b"code")
        .map(|sc| sc.account)
        .unwrap();

    // query_view should fail (empty ABI) but crucially should NOT produce
    // any side effects. The state should remain unchanged.
    let _ = executor.query_view(&post_state, contract_addr, "get_count", &[], &[]);

    // Verify state is unmodified by trying the same query again.
    let result2 = executor.query_view(&post_state, contract_addr, "get_count", &[], &[]);
    // Both calls should produce the same error — state wasn't mutated.
    assert!(result2.is_err());
}

// ═══════════════════════════════════════════════════════════════════════
// T-11004 EXPANSION: FAILURE — EDGE CASES
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn empty_block_execution() {
    let executor = make_executor();
    let state = MemState::new();

    let result = executor.execute(&[], &state).unwrap();

    assert!(result.receipts.is_empty());
    assert_eq!(result.gas_used_total, 0);
}

#[test]
fn large_batch_execution() {
    let executor = make_executor();

    // Create 50 transfers from distinct senders.
    let mut state = MemState::new();
    let txs: Vec<_> = (0..50u8)
        .map(|i| {
            let sender = addr(i);
            state = MemState {
                data: std::mem::take(&mut state.data),
            }
            .with_balance(sender, 1_000_000);
            make_transfer_tx(sender, addr(0xFF), 100, 50_000)
        })
        .collect();

    let result = executor.execute(&txs, &state).unwrap();

    assert_eq!(result.receipts.len(), 50);
    assert!(result.gas_used_total > 0);
    // All should succeed — unique senders, sufficient balances.
    for receipt in &result.receipts {
        assert_eq!(receipt.status, ExecutionStatus::Success);
    }
}

#[test]
fn publish_then_call_same_block() {
    // Publishing and calling in the same block: the call should fail
    // because Block-STM reads state from the snapshot, not from the
    // publish write-set of the same block (without overlay).
    let executor = make_executor();
    let state = MemState::new();

    let pub_tx = make_publish_tx(addr(0xAA), vec![make_test_bytecode(16)], 100_000);
    let call_tx = make_call_tx(
        addr(1),
        ContractAddress([0xDD; 32]),
        "increment",
        vec![],
        50_000,
    );

    let result = executor.execute(&[pub_tx, call_tx], &state).unwrap();

    assert_eq!(result.receipts.len(), 2);
    assert_eq!(result.receipts[0].status, ExecutionStatus::Success);
    // The call should fail because the contract doesn't exist in the
    // initial state snapshot — Block-STM overlay propagates write-sets
    // but the contract address differs.
    assert!(matches!(
        result.receipts[1].status,
        ExecutionStatus::MoveAbort { code: 2, .. }
    ));
}

#[test]
fn gas_used_never_exceeds_limit() {
    let executor = make_executor();
    let state = MemState::new().with_balance(addr(1), 1_000_000);

    let gas_limit = 50_000u64;
    let txs = vec![
        make_transfer_tx(addr(1), addr(2), 100, gas_limit),
        make_publish_tx(addr(0xAA), vec![make_test_bytecode(32)], gas_limit),
        make_call_tx(
            addr(1),
            ContractAddress([0xDD; 32]),
            "foo",
            vec![],
            gas_limit,
        ),
    ];
    let result = executor.execute(&txs, &state).unwrap();

    for receipt in &result.receipts {
        assert!(
            receipt.gas_used <= gas_limit,
            "gas_used {} must not exceed gas_limit {}",
            receipt.gas_used,
            gas_limit,
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════
// T-11004: ABI MIRROR TYPES FOR CROSS-CRATE INTEGRATION TESTING
// ═══════════════════════════════════════════════════════════════════════
//
// These types mirror the `pub(crate)` FunctionAbi/MoveType in
// nexus-execution's move_adapter::abi module.  BCS serialisation is
// deterministic and layout-compatible, so encoding these produces
// identical bytes.

#[derive(serde::Serialize, serde::Deserialize)]
struct TestFunctionAbi {
    name: String,
    params: Vec<TestMoveType>,
    returns: Option<TestMoveType>,
    is_entry: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
enum TestMoveType {
    U64,
    U128,
    Bool,
    Address,
    VectorU8,
}

// ── ABI + Resource Helpers ──────────────────────────────────────────────

fn encode_test_abi(functions: &[TestFunctionAbi]) -> Vec<u8> {
    bcs::to_bytes(&functions).expect("BCS encode ABI")
}

/// Derive a resource storage key matching `resources::resource_key`.
fn resource_key(type_tag: &str) -> Vec<u8> {
    let mut key = b"resource::".to_vec();
    key.extend_from_slice(type_tag.as_bytes());
    key
}

/// Extract the contract address from a publish receipt.
fn get_contract_address(receipt: &nexus_execution::TransactionReceipt) -> AccountAddress {
    receipt
        .state_changes
        .iter()
        .find(|sc| sc.key == b"code")
        .map(|sc| sc.account)
        .expect("publish receipt must contain 'code' state change")
}

/// Build a `MemState` from a single receipt's state changes.
fn build_post_state(receipt: &nexus_execution::TransactionReceipt) -> MemState {
    let mut state = MemState::new();
    for sc in &receipt.state_changes {
        if let Some(ref val) = sc.value {
            state.set(sc.account, &sc.key, val.clone());
        }
    }
    state
}

/// Merge state changes from a receipt into an existing state.
fn merge_state_changes(state: &mut MemState, receipt: &nexus_execution::TransactionReceipt) {
    for sc in &receipt.state_changes {
        if let Some(ref val) = sc.value {
            state.set(sc.account, &sc.key, val.clone());
        }
    }
}

/// Install an ABI at a contract address.
fn install_abi(state: &mut MemState, contract: AccountAddress, functions: &[TestFunctionAbi]) {
    state.set(contract, b"abi", encode_test_abi(functions));
}

/// Write a u64 resource value matching the `"fn_name::State"` convention.
fn install_resource_u64(state: &mut MemState, account: AccountAddress, fn_name: &str, value: u64) {
    state.set(
        account,
        &resource_key(&format!("{fn_name}::State")),
        value.to_le_bytes().to_vec(),
    );
}

fn counter_abi() -> Vec<TestFunctionAbi> {
    vec![TestFunctionAbi {
        name: "increment".into(),
        params: vec![],
        returns: None,
        is_entry: true,
    }]
}

fn counter_with_getter_abi() -> Vec<TestFunctionAbi> {
    vec![
        TestFunctionAbi {
            name: "increment".into(),
            params: vec![],
            returns: None,
            is_entry: true,
        },
        TestFunctionAbi {
            name: "get_count".into(),
            params: vec![],
            returns: Some(TestMoveType::U64),
            is_entry: false,
        },
    ]
}

fn transfer_abi() -> Vec<TestFunctionAbi> {
    vec![TestFunctionAbi {
        name: "transfer".into(),
        params: vec![TestMoveType::Address, TestMoveType::U64],
        returns: None,
        is_entry: true,
    }]
}

fn generic_entry_abi() -> Vec<TestFunctionAbi> {
    vec![TestFunctionAbi {
        name: "store_data".into(),
        params: vec![TestMoveType::U64, TestMoveType::Bool],
        returns: None,
        is_entry: true,
    }]
}

// ═══════════════════════════════════════════════════════════════════════
// T-11004 EXPANSION: ABI-BACKED PIPELINE TESTS
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn pipeline_abi_counter_single_increment() {
    let executor = make_executor();
    let state = MemState::new();

    // Step 1: Publish module.
    let pub_tx = make_publish_tx(addr(0xAA), vec![make_test_bytecode(16)], 100_000);
    let pub_result = executor.execute(&[pub_tx], &state).unwrap();
    assert_eq!(pub_result.receipts[0].status, ExecutionStatus::Success);

    // Step 2: Build post-state, install counter ABI.
    let contract = get_contract_address(&pub_result.receipts[0]);
    let mut post_state = build_post_state(&pub_result.receipts[0]);
    install_abi(&mut post_state, contract, &counter_abi());

    // Step 3: Call "increment".
    let call_tx = make_call_tx(
        addr(1),
        ContractAddress(contract.0),
        "increment",
        vec![],
        50_000,
    );
    let call_result = executor.execute(&[call_tx], &post_state).unwrap();
    assert_eq!(call_result.receipts[0].status, ExecutionStatus::Success);
    assert!(call_result.receipts[0].gas_used > 0);

    // Step 4: Verify counter resource written with value 1.
    let resource_sc = call_result.receipts[0]
        .state_changes
        .iter()
        .find(|sc| sc.key == resource_key("increment::State"))
        .expect("increment must produce a resource state change");
    let val = u64::from_le_bytes(
        resource_sc
            .value
            .as_ref()
            .unwrap()
            .as_slice()
            .try_into()
            .unwrap(),
    );
    assert_eq!(val, 1, "counter should be 1 after first increment");
}

#[test]
fn pipeline_abi_counter_cross_block() {
    let executor = make_executor();
    let state = MemState::new();

    // Publish.
    let pub_tx = make_publish_tx(addr(0xAA), vec![make_test_bytecode(16)], 100_000);
    let pub_result = executor.execute(&[pub_tx], &state).unwrap();
    let contract = get_contract_address(&pub_result.receipts[0]);
    let mut post_state = build_post_state(&pub_result.receipts[0]);
    install_abi(&mut post_state, contract, &counter_abi());

    // Block 2: First increment → value 1.
    let call1 = make_call_tx(
        addr(1),
        ContractAddress(contract.0),
        "increment",
        vec![],
        50_000,
    );
    let res1 = executor.execute(&[call1], &post_state).unwrap();
    assert_eq!(res1.receipts[0].status, ExecutionStatus::Success);
    merge_state_changes(&mut post_state, &res1.receipts[0]);

    // Block 3: Second increment → value 2.
    let call2 = make_call_tx(
        addr(2),
        ContractAddress(contract.0),
        "increment",
        vec![],
        50_000,
    );
    let res2 = executor.execute(&[call2], &post_state).unwrap();
    assert_eq!(res2.receipts[0].status, ExecutionStatus::Success);

    let resource_sc = res2.receipts[0]
        .state_changes
        .iter()
        .find(|sc| sc.key == resource_key("increment::State"))
        .expect("state change for increment resource");
    let val = u64::from_le_bytes(
        resource_sc
            .value
            .as_ref()
            .unwrap()
            .as_slice()
            .try_into()
            .unwrap(),
    );
    assert_eq!(val, 2, "counter should be 2 after two increments");
}

#[test]
fn pipeline_abi_full_publish_increment_query() {
    let executor = make_executor();
    let state = MemState::new();

    // Step 1: Publish.
    let pub_tx = make_publish_tx(addr(0xAA), vec![make_test_bytecode(16)], 100_000);
    let pub_result = executor.execute(&[pub_tx], &state).unwrap();
    let contract = get_contract_address(&pub_result.receipts[0]);
    let mut post_state = build_post_state(&pub_result.receipts[0]);
    install_abi(&mut post_state, contract, &counter_abi());

    // Step 2: Increment.
    let call_tx = make_call_tx(
        addr(1),
        ContractAddress(contract.0),
        "increment",
        vec![],
        50_000,
    );
    let call_result = executor.execute(&[call_tx], &post_state).unwrap();
    assert_eq!(call_result.receipts[0].status, ExecutionStatus::Success);
    merge_state_changes(&mut post_state, &call_result.receipts[0]);

    // Step 3: Query view "increment" — reads resource::increment::State.
    let query = executor
        .query_view(&post_state, contract, "increment", &[], &[])
        .expect("query_view should succeed after increment");
    let return_val = query.return_value.expect("should return a value");
    let counter = u64::from_le_bytes(return_val.as_slice().try_into().unwrap());
    assert_eq!(counter, 1, "query should return counter value 1");
}

#[test]
fn pipeline_abi_transfer_dispatch_success() {
    let executor = make_executor();
    let state = MemState::new();

    // Publish + ABI.
    let pub_tx = make_publish_tx(addr(0xAA), vec![make_test_bytecode(16)], 100_000);
    let pub_result = executor.execute(&[pub_tx], &state).unwrap();
    let contract = get_contract_address(&pub_result.receipts[0]);
    let mut post_state = build_post_state(&pub_result.receipts[0]);
    install_abi(&mut post_state, contract, &transfer_abi());

    // Give sender a balance via the resource key convention.
    let sender = addr(1);
    let recipient = addr(2);
    post_state.set(
        sender,
        &resource_key("balance::Balance"),
        1000u64.to_le_bytes().to_vec(),
    );

    // Call "transfer" with (recipient_address, amount).
    let call_tx = make_call_tx(
        sender,
        ContractAddress(contract.0),
        "transfer",
        vec![recipient.0.to_vec(), 500u64.to_le_bytes().to_vec()],
        50_000,
    );
    let result = executor.execute(&[call_tx], &post_state).unwrap();
    assert_eq!(result.receipts[0].status, ExecutionStatus::Success);

    // Verify balance state changes (debit sender, credit recipient).
    let balance_changes: Vec<_> = result.receipts[0]
        .state_changes
        .iter()
        .filter(|sc| sc.key == resource_key("balance::Balance"))
        .collect();
    assert!(
        balance_changes.len() >= 2,
        "should have sender debit and recipient credit"
    );
}

#[test]
fn pipeline_abi_transfer_insufficient_balance() {
    let executor = make_executor();
    let state = MemState::new();

    // Publish + ABI.
    let pub_tx = make_publish_tx(addr(0xAA), vec![make_test_bytecode(16)], 100_000);
    let pub_result = executor.execute(&[pub_tx], &state).unwrap();
    let contract = get_contract_address(&pub_result.receipts[0]);
    let mut post_state = build_post_state(&pub_result.receipts[0]);
    install_abi(&mut post_state, contract, &transfer_abi());

    // Sender has zero balance (not pre-populated).
    let call_tx = make_call_tx(
        addr(1),
        ContractAddress(contract.0),
        "transfer",
        vec![addr(2).0.to_vec(), 500u64.to_le_bytes().to_vec()],
        50_000,
    );
    let result = executor.execute(&[call_tx], &post_state).unwrap();

    // Should abort with INSUFFICIENT_BALANCE (code 100).
    assert!(matches!(
        result.receipts[0].status,
        ExecutionStatus::MoveAbort { code: 100, .. }
    ));
}

#[test]
fn pipeline_abi_generic_entry_stores_payload() {
    let executor = make_executor();
    let state = MemState::new();

    // Publish + ABI.
    let pub_tx = make_publish_tx(addr(0xAA), vec![make_test_bytecode(16)], 100_000);
    let pub_result = executor.execute(&[pub_tx], &state).unwrap();
    let contract = get_contract_address(&pub_result.receipts[0]);
    let mut post_state = build_post_state(&pub_result.receipts[0]);
    install_abi(&mut post_state, contract, &generic_entry_abi());

    // Call with (U64, Bool) args.
    let arg_u64 = 42u64.to_le_bytes().to_vec();
    let arg_bool = vec![1u8]; // true
    let call_tx = make_call_tx(
        addr(1),
        ContractAddress(contract.0),
        "store_data",
        vec![arg_u64.clone(), arg_bool.clone()],
        50_000,
    );
    let result = executor.execute(&[call_tx], &post_state).unwrap();
    assert_eq!(result.receipts[0].status, ExecutionStatus::Success);

    // Verify stored resource = concatenated args.
    let resource_sc = result.receipts[0]
        .state_changes
        .iter()
        .find(|sc| sc.key == resource_key("store_data::State"))
        .expect("generic entry must store resource");
    let stored = resource_sc.value.as_ref().unwrap();
    let mut expected = Vec::new();
    expected.extend_from_slice(&arg_u64);
    expected.extend_from_slice(&arg_bool);
    assert_eq!(
        stored, &expected,
        "stored resource should be concatenated BCS args"
    );
}

// ═══════════════════════════════════════════════════════════════════════
// T-11004 EXPANSION: QUERY VIEW — ABI-BACKED TESTS
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn query_view_abi_returns_u64_value() {
    let executor = make_executor();
    let state = MemState::new();

    // Publish + ABI with getter.
    let pub_tx = make_publish_tx(addr(0xAA), vec![make_test_bytecode(16)], 100_000);
    let pub_result = executor.execute(&[pub_tx], &state).unwrap();
    let contract = get_contract_address(&pub_result.receipts[0]);
    let mut post_state = build_post_state(&pub_result.receipts[0]);
    install_abi(&mut post_state, contract, &counter_with_getter_abi());

    // Pre-populate the resource that get_count reads.
    install_resource_u64(&mut post_state, contract, "get_count", 42);

    let result = executor
        .query_view(&post_state, contract, "get_count", &[], &[])
        .expect("query should succeed");
    let val = result.return_value.expect("should return a value");
    let counter = u64::from_le_bytes(val.as_slice().try_into().unwrap());
    assert_eq!(counter, 42);
    assert!(result.gas_used > 0);
}

#[test]
fn query_view_abi_empty_resource_returns_none() {
    let executor = make_executor();
    let state = MemState::new();

    // Publish + ABI with getter, but no resource pre-populated.
    let pub_tx = make_publish_tx(addr(0xAA), vec![make_test_bytecode(16)], 100_000);
    let pub_result = executor.execute(&[pub_tx], &state).unwrap();
    let contract = get_contract_address(&pub_result.receipts[0]);
    let mut post_state = build_post_state(&pub_result.receipts[0]);
    install_abi(&mut post_state, contract, &counter_with_getter_abi());

    let result = executor
        .query_view(&post_state, contract, "get_count", &[], &[])
        .expect("query should succeed even with no resource");
    assert!(result.return_value.is_none(), "no resource → None return");
}

#[test]
fn query_view_function_not_in_abi() {
    let executor = make_executor();
    let state = MemState::new();

    // Publish + ABI with only "increment".
    let pub_tx = make_publish_tx(addr(0xAA), vec![make_test_bytecode(16)], 100_000);
    let pub_result = executor.execute(&[pub_tx], &state).unwrap();
    let contract = get_contract_address(&pub_result.receipts[0]);
    let mut post_state = build_post_state(&pub_result.receipts[0]);
    install_abi(&mut post_state, contract, &counter_abi());

    let result = executor.query_view(&post_state, contract, "nonexistent", &[], &[]);
    assert!(result.is_err(), "query should fail for function not in ABI");
}

// ═══════════════════════════════════════════════════════════════════════
// T-11004 EXPANSION: ARGUMENT VALIDATION THROUGH BLOCK-STM
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn invoke_abi_arg_count_mismatch() {
    let executor = make_executor();
    let state = MemState::new();

    // Publish + transfer ABI (expects 2 args).
    let pub_tx = make_publish_tx(addr(0xAA), vec![make_test_bytecode(16)], 100_000);
    let pub_result = executor.execute(&[pub_tx], &state).unwrap();
    let contract = get_contract_address(&pub_result.receipts[0]);
    let mut post_state = build_post_state(&pub_result.receipts[0]);
    install_abi(&mut post_state, contract, &transfer_abi());

    // Call with 0 args instead of 2.
    let call_tx = make_call_tx(
        addr(1),
        ContractAddress(contract.0),
        "transfer",
        vec![],
        50_000,
    );

    // TypeMismatch may surface as a block-level error or a non-Success
    // receipt, depending on executor error propagation policy.
    let result = executor.execute(&[call_tx], &post_state);
    match result {
        Err(_) => {} // Propagated as execution error.
        Ok(block) => {
            assert_ne!(
                block.receipts[0].status,
                ExecutionStatus::Success,
                "wrong arg count must not succeed"
            );
        }
    }
}

#[test]
fn invoke_abi_arg_type_mismatch() {
    let executor = make_executor();
    let state = MemState::new();

    // Publish + transfer ABI (expects [Address(32B), U64(8B)]).
    let pub_tx = make_publish_tx(addr(0xAA), vec![make_test_bytecode(16)], 100_000);
    let pub_result = executor.execute(&[pub_tx], &state).unwrap();
    let contract = get_contract_address(&pub_result.receipts[0]);
    let mut post_state = build_post_state(&pub_result.receipts[0]);
    install_abi(&mut post_state, contract, &transfer_abi());

    // Call with wrong-sized args (4B for Address, correct 8B for U64).
    let call_tx = make_call_tx(
        addr(1),
        ContractAddress(contract.0),
        "transfer",
        vec![vec![0u8; 4], vec![0u8; 8]],
        50_000,
    );

    let result = executor.execute(&[call_tx], &post_state);
    match result {
        Err(_) => {} // Propagated as execution error.
        Ok(block) => {
            assert_ne!(
                block.receipts[0].status,
                ExecutionStatus::Success,
                "wrong arg type must not succeed"
            );
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════
// T-11004 EXPANSION: MULTI-CALLER CONTENTION
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn concurrent_counter_increments_same_contract() {
    let executor = make_executor();
    let state = MemState::new();

    // Publish + counter ABI.
    let pub_tx = make_publish_tx(addr(0xAA), vec![make_test_bytecode(16)], 100_000);
    let pub_result = executor.execute(&[pub_tx], &state).unwrap();
    let contract = get_contract_address(&pub_result.receipts[0]);
    let mut post_state = build_post_state(&pub_result.receipts[0]);
    install_abi(&mut post_state, contract, &counter_abi());

    // Three callers increment the same counter in one block.
    // Block-STM resolves conflicts via validation + retry.
    let txs = vec![
        make_call_tx(
            addr(1),
            ContractAddress(contract.0),
            "increment",
            vec![],
            50_000,
        ),
        make_call_tx(
            addr(2),
            ContractAddress(contract.0),
            "increment",
            vec![],
            50_000,
        ),
        make_call_tx(
            addr(3),
            ContractAddress(contract.0),
            "increment",
            vec![],
            50_000,
        ),
    ];
    let result = executor.execute(&txs, &post_state).unwrap();

    assert_eq!(result.receipts.len(), 3);
    for receipt in &result.receipts {
        assert_eq!(receipt.status, ExecutionStatus::Success);
    }
    assert!(result.gas_used_total > 0);
}

// ═══════════════════════════════════════════════════════════════════════
// T-11004 EXPANSION: GAS SCHEDULE VERIFICATION
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn gas_publish_scales_with_size() {
    let executor = make_executor();
    let state = MemState::new();

    // Small module: 16 bytes of padding.
    let small_tx = make_publish_tx(addr(0xA1), vec![make_test_bytecode(16)], 200_000);
    let small_result = executor.execute(&[small_tx], &state).unwrap();
    assert_eq!(small_result.receipts[0].status, ExecutionStatus::Success);
    let gas_small = small_result.receipts[0].gas_used;

    // Large module: 4096 bytes of padding.
    let large_tx = make_publish_tx(addr(0xA2), vec![make_test_bytecode(4096)], 200_000);
    let large_result = executor.execute(&[large_tx], &state).unwrap();
    assert_eq!(large_result.receipts[0].status, ExecutionStatus::Success);
    let gas_large = large_result.receipts[0].gas_used;

    assert!(
        gas_large > gas_small,
        "larger module ({gas_large}) should cost more gas than smaller ({gas_small})"
    );
}

#[test]
fn gas_call_includes_base_cost() {
    let executor = make_executor();
    let state = MemState::new();

    // Publish + counter ABI.
    let pub_tx = make_publish_tx(addr(0xAA), vec![make_test_bytecode(16)], 100_000);
    let pub_result = executor.execute(&[pub_tx], &state).unwrap();
    let contract = get_contract_address(&pub_result.receipts[0]);
    let mut post_state = build_post_state(&pub_result.receipts[0]);
    install_abi(&mut post_state, contract, &counter_abi());

    let call_tx = make_call_tx(
        addr(1),
        ContractAddress(contract.0),
        "increment",
        vec![],
        50_000,
    );
    let result = executor.execute(&[call_tx], &post_state).unwrap();
    assert_eq!(result.receipts[0].status, ExecutionStatus::Success);

    // Default call_base_gas is 5000; actual gas should be >= base.
    assert!(
        result.receipts[0].gas_used >= 5_000,
        "call gas {} should be >= call_base 5000",
        result.receipts[0].gas_used,
    );
}

// ═══════════════════════════════════════════════════════════════════════
// T-11004 EXPANSION: MIXED OPERATIONS
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn mixed_publish_transfer_and_call_in_one_block() {
    let executor = make_executor();
    let state = MemState::new().with_balance(addr(1), 1_000_000);

    let txs = vec![
        // Tx 0: publish a module.
        make_publish_tx(addr(0xC1), vec![make_test_bytecode(16)], 100_000),
        // Tx 1: native transfer.
        make_transfer_tx(addr(1), addr(2), 500, 50_000),
        // Tx 2: call a nonexistent contract (fails gracefully).
        make_call_tx(addr(3), ContractAddress([0xDD; 32]), "foo", vec![], 50_000),
    ];
    let result = executor.execute(&txs, &state).unwrap();

    assert_eq!(result.receipts.len(), 3);
    assert_eq!(result.receipts[0].status, ExecutionStatus::Success);
    assert_eq!(result.receipts[1].status, ExecutionStatus::Success);
    assert!(matches!(
        result.receipts[2].status,
        ExecutionStatus::MoveAbort { code: 2, .. }
    ));
}

#[test]
fn publish_different_bytecode_yields_different_address() {
    let executor = make_executor();
    let state = MemState::new();

    // Same deployer, different bytecode → different contract addresses.
    let tx1 = make_publish_tx(addr(0xAA), vec![make_test_bytecode(16)], 100_000);
    let tx2 = make_publish_tx(addr(0xAA), vec![make_test_bytecode(64)], 100_000);

    let r1 = executor.execute(&[tx1], &state).unwrap();
    let r2 = executor.execute(&[tx2], &state).unwrap();

    let a1 = get_contract_address(&r1.receipts[0]);
    let a2 = get_contract_address(&r2.receipts[0]);
    assert_ne!(
        a1, a2,
        "different bytecode must produce different addresses"
    );
}
