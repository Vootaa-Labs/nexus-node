// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Execution test fixtures.
//!
//! Helpers for building signed transactions and in-memory state views
//! for integration tests.

use nexus_crypto::{DilithiumSigner, DilithiumSigningKey, DilithiumVerifyKey, Signer};
use nexus_execution::types::{
    compute_tx_digest, SignedTransaction, TransactionBody, TransactionPayload, TX_DOMAIN,
};
use nexus_execution::{BlockStmExecutor, ExecutionResult, StateView};
use nexus_primitives::{
    AccountAddress, Amount, CommitSequence, EpochNumber, ShardId, TimestampMs, TokenId,
};
use std::collections::HashMap;

/// In-memory state view for integration testing.
pub struct MemStateView {
    data: HashMap<(AccountAddress, Vec<u8>), Vec<u8>>,
}

impl MemStateView {
    /// Create an empty state view.
    pub fn new() -> Self {
        Self {
            data: HashMap::new(),
        }
    }

    /// Set a balance for an account.
    pub fn set_balance(&mut self, addr: AccountAddress, balance: u64) {
        self.data
            .insert((addr, b"balance".to_vec()), balance.to_le_bytes().to_vec());
    }

    /// Set an arbitrary key-value pair.
    pub fn set(&mut self, addr: AccountAddress, key: Vec<u8>, value: Vec<u8>) {
        self.data.insert((addr, key), value);
    }
}

impl Default for MemStateView {
    fn default() -> Self {
        Self::new()
    }
}

impl StateView for MemStateView {
    fn get(&self, account: &AccountAddress, key: &[u8]) -> ExecutionResult<Option<Vec<u8>>> {
        Ok(self.data.get(&(*account, key.to_vec())).cloned())
    }
}

/// A test transaction builder with pre-generated signing keys.
pub struct TxBuilder {
    /// Signing key.
    pub sk: DilithiumSigningKey,
    /// Verify key.
    pub pk: DilithiumVerifyKey,
    /// Sender address derived from the verify key.
    pub sender: AccountAddress,
    /// Chain ID for all transactions.
    pub chain_id: u64,
}

impl TxBuilder {
    /// Create a new builder with a fresh keypair.
    pub fn new(chain_id: u64) -> Self {
        let (sk, pk) = DilithiumSigner::generate_keypair();
        let sender = AccountAddress::from_dilithium_pubkey(pk.as_bytes());
        Self {
            sk,
            pk,
            sender,
            chain_id,
        }
    }

    /// Build a signed transfer transaction.
    pub fn transfer(
        &self,
        recipient: AccountAddress,
        amount: u64,
        nonce: u64,
    ) -> SignedTransaction {
        let body = TransactionBody {
            sender: self.sender,
            sequence_number: nonce,
            expiry_epoch: EpochNumber(1000),
            gas_limit: 50_000,
            gas_price: 1,
            target_shard: None,
            payload: TransactionPayload::Transfer {
                recipient,
                amount: Amount(amount),
                token: TokenId::Native,
            },
            chain_id: self.chain_id,
        };
        self.sign(body)
    }

    /// Sign a transaction body.
    pub fn sign(&self, body: TransactionBody) -> SignedTransaction {
        let digest = compute_tx_digest(&body)
            .expect("BCS serialization of test TransactionBody cannot fail");
        let sig = DilithiumSigner::sign(&self.sk, TX_DOMAIN, digest.as_bytes());
        SignedTransaction {
            body,
            signature: sig,
            sender_pk: self.pk.clone(),
            digest,
        }
    }
}

/// Create a [`BlockStmExecutor`] configured for test use.
pub fn test_executor(shard: u16, seq: u64) -> BlockStmExecutor {
    BlockStmExecutor::new(ShardId(shard), CommitSequence(seq), TimestampMs::now())
}

// ── Move ABI helpers (BCS-compatible mirrors of pub(crate) types) ───

/// ABI descriptor for a single Move function (mirrors `move_adapter::abi::FunctionAbi`).
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct TestFunctionAbi {
    /// Function name.
    pub name: String,
    /// Parameter types.
    pub params: Vec<TestMoveType>,
    /// Return type (None = void).
    pub returns: Option<TestMoveType>,
    /// Whether this is a state-mutating entry function.
    pub is_entry: bool,
}

/// Primitive Move types at the ABI boundary (mirrors `move_adapter::abi::MoveType`).
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub enum TestMoveType {
    /// Unsigned 64-bit integer.
    U64,
    /// Unsigned 128-bit integer.
    U128,
    /// Boolean.
    Bool,
    /// 32-byte address.
    Address,
    /// Variable-length byte vector.
    VectorU8,
}

/// BCS-encode an ABI function list.
pub fn encode_test_abi(functions: &[TestFunctionAbi]) -> Vec<u8> {
    bcs::to_bytes(functions).expect("BCS-encode ABI")
}

/// Install an ABI into the state at a contract address.
pub fn install_abi(
    state: &mut MemStateView,
    contract: AccountAddress,
    functions: &[TestFunctionAbi],
) {
    state.set(contract, b"abi".to_vec(), encode_test_abi(functions));
}

/// Write a u64 resource matching the `"fn_name::State"` convention used by `query_view`.
pub fn install_resource_u64(
    state: &mut MemStateView,
    account: AccountAddress,
    fn_name: &str,
    value: u64,
) {
    let key = format!("resource::{fn_name}::State");
    state.set(account, key.into_bytes(), value.to_le_bytes().to_vec());
}

/// Build a pre-populated state containing contract code + ABI + resource for benchmarks.
///
/// Returns `(state, contract_address)`.
pub fn setup_query_view_state(deployer_byte: u8) -> (MemStateView, AccountAddress) {
    // Build a synthetic module and derive the contract address.
    let bytecode = {
        let mut m = vec![0xa1, 0x1c, 0xeb, 0x0b]; // Move magic
        m.extend_from_slice(&1u32.to_le_bytes()); // Version 1
        m.extend(vec![0u8; 64]); // Padding
        m
    };

    let deployer = AccountAddress([deployer_byte; 32]);
    let bytecode_hash = blake3::hash(&bytecode);
    let contract =
        nexus_primitives::ContractAddress::from_deployment(&deployer, bytecode_hash.as_bytes());
    let contract_addr = AccountAddress(contract.0);

    let mut state = MemStateView::new();
    // Store module code.
    state.set(contract_addr, b"code".to_vec(), bytecode.clone());
    // Store ABI with a getter function.
    install_abi(
        &mut state,
        contract_addr,
        &[TestFunctionAbi {
            name: "get_count".into(),
            params: vec![],
            returns: Some(TestMoveType::U64),
            is_entry: false,
        }],
    );
    // Pre-populate the resource.
    install_resource_u64(&mut state, contract_addr, "get_count", 42);

    (state, contract_addr)
}
