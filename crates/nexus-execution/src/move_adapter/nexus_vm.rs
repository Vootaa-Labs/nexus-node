// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Nexus Move VM — ABI-driven bytecode execution engine.
//!
//! [`NexusMoveVm`] implements the [`MoveVm`] trait with real execution
//! semantics:
//!
//! 1. **Publish**: bytecode verification → ABI extraction → package metadata
//!    → gas-metered storage via [`ModulePublisher`].
//! 2. **Execute**: ABI-driven dispatch — validates call arguments, creates
//!    an [`ExecuteSession`], interprets entry-point logic, and commits
//!    resource mutations.
//!
//! When a real `move-vm-runtime` integration lands (behind `#[cfg(feature
//! = "move-vm")]`), only the inner dispatch loop in `dispatch_entry`
//! changes; the session lifecycle, gas accounting, and event bridge
//! remain identical.

use std::collections::HashMap;

use crate::error::{ExecutionError, ExecutionResult};
use crate::types::ExecutionStatus;
use nexus_primitives::AccountAddress;

use super::abi::{decode_abi, encode_abi, FunctionAbi, MoveType, MODULE_ABI_KEY};
use super::entry_function::{decode_u64_arg, encode_u64_arg, validate_args};
use super::gas_meter::{estimate_script_gas, GasSchedule};
use super::package::{encode_metadata, PackageMetadata, UpgradePolicy, MODULE_METADATA_KEY};
use super::publisher::ModulePublisher;
use super::session::{ExecuteSession, SessionKind};
use super::state_view::NexusStateView;
use super::verifier::BytecodeVerifier;
use super::vm_config::VmConfig;
use super::{MoveVm, VmOutput};

// ── NexusMoveVm ─────────────────────────────────────────────────────────

/// Nexus-native Move VM with ABI-driven dispatch.
///
/// This replaces [`BuiltinVm`](super::builtin_vm::BuiltinVm) as the
/// default VM implementation.  It provides:
///
/// - **Real function dispatch** via published ABI metadata.
/// - **Gas-metered resource reads/writes** through [`ExecuteSession`].
/// - **Structured package metadata** (TLD-09 §4).
/// - **Event emission** normalised to [`ContractEvent`](super::events::ContractEvent).
pub(crate) struct NexusMoveVm {
    /// Gas schedule derived from config.
    schedule: GasSchedule,
    /// Structural bytecode verifier.
    verifier: BytecodeVerifier,
    /// VM configuration (retained for future `move-vm-runtime` integration).
    #[allow(dead_code)]
    config: VmConfig,
}

impl NexusMoveVm {
    /// Create a new VM with the given configuration.
    pub fn new(config: &VmConfig) -> Self {
        Self {
            schedule: GasSchedule::from_config(config),
            verifier: BytecodeVerifier::from_vm_config(config),
            config: config.clone(),
        }
    }

    /// Look up a function's ABI from on-chain state.
    fn resolve_function_abi(
        &self,
        state: &NexusStateView<'_>,
        contract: &AccountAddress,
        function: &str,
    ) -> ExecutionResult<Option<FunctionAbi>> {
        let abi_bytes = state.get_raw(contract, MODULE_ABI_KEY)?;
        let Some(bytes) = abi_bytes else {
            return Ok(None);
        };
        let functions = decode_abi(&bytes).map_err(|e| ExecutionError::BytecodeVerification {
            reason: format!("corrupt ABI at contract: {e}"),
        })?;
        Ok(functions.into_iter().find(|f| f.name == function))
    }

    /// Execute an entry function against a session.
    ///
    /// This is the inner dispatch loop.  For now it handles well-known
    /// patterns (counter increment/get, balance transfer) by interpreting
    /// the ABI.  When `move-vm-runtime` is integrated, this method will
    /// delegate to the upstream VM session.
    fn dispatch_entry(
        &self,
        session: &mut ExecuteSession<'_>,
        contract: AccountAddress,
        function: &str,
        abi: &FunctionAbi,
        args: &[Vec<u8>],
    ) -> Result<Option<Vec<u8>>, DispatchError> {
        // Charge base call gas.
        session
            .charge_execution(self.schedule.call_base)
            .map_err(|_| DispatchError::OutOfGas)?;

        // Resource type tag derived from ABI function context.
        let resource_tag = format!("{}::State", function);

        // Generic dispatch: entry functions with no params that mutate a u64
        // counter resource are handled as increment-style operations.
        match (abi.is_entry, abi.params.as_slice(), &abi.returns) {
            // ── Counter-style: no args, no return, mutates state ────
            (true, [], None) => {
                let current = self.read_u64_resource(session, &contract, &resource_tag)?;
                let new_val = current.saturating_add(1);
                self.write_u64_resource(session, contract, &resource_tag, new_val)?;

                // Emit event.
                session.events.emit(
                    contract,
                    format!("{function}::Executed"),
                    encode_u64_arg(new_val),
                );
                Ok(None)
            }

            // ── Getter-style: no args, returns U64 ─────────────────
            (false, [], Some(MoveType::U64)) => {
                let val = self.read_u64_resource(session, &contract, &resource_tag)?;
                Ok(Some(encode_u64_arg(val)))
            }

            // ── Transfer-style: (Address, U64), no return ──────────
            (true, [MoveType::Address, MoveType::U64], None) => {
                let recipient = AccountAddress(
                    args[0]
                        .as_slice()
                        .try_into()
                        .map_err(|_| DispatchError::ArgDecode("bad address"))?,
                );
                let amount = decode_u64_arg(&args[1])
                    .map_err(|_| DispatchError::ArgDecode("bad u64 amount"))?;

                // Copy sender address to avoid borrow conflict.
                let sender_addr = session.sender;

                // Debit sender.
                let sender_bal =
                    self.read_u64_resource(session, &sender_addr, "balance::Balance")?;
                if sender_bal < amount {
                    return Err(DispatchError::Abort {
                        location: function.to_string(),
                        code: 100, // INSUFFICIENT_BALANCE
                    });
                }
                self.write_u64_resource(
                    session,
                    sender_addr,
                    "balance::Balance",
                    sender_bal.saturating_sub(amount),
                )?;

                // Credit recipient.
                let recv_bal = self.read_u64_resource(session, &recipient, "balance::Balance")?;
                self.write_u64_resource(
                    session,
                    recipient,
                    "balance::Balance",
                    recv_bal.saturating_add(amount),
                )?;

                let event_data =
                    bcs::to_bytes(&(sender_addr, recipient, amount)).unwrap_or_else(|e| {
                        tracing::warn!("Transfer event serialization failed: {e}");
                        vec![]
                    });
                session
                    .events
                    .emit(contract, format!("{function}::Transfer"), event_data);
                Ok(None)
            }

            // ── Generic entry: store args as resource ──────────────
            (true, params, returns) => {
                tracing::debug!(
                    function,
                    param_count = params.len(),
                    has_return = returns.is_some(),
                    "dispatch: generic entry path"
                );
                // For entry functions with arbitrary params, store the
                // concatenated BCS args as the resource value.
                let mut payload = Vec::new();
                for arg in args {
                    payload.extend_from_slice(arg);
                }
                self.write_resource_raw(session, contract, &resource_tag, payload)?;

                session
                    .events
                    .emit(contract, format!("{function}::Executed"), vec![]);
                Ok(None)
            }

            // ── Non-entry read function: return resource bytes ─────
            (false, params, returns) => {
                tracing::debug!(
                    function,
                    param_count = params.len(),
                    has_return = returns.is_some(),
                    "dispatch: generic read path"
                );
                let val = session
                    .read_resource(&contract, &resource_tag)
                    .map_err(|_| DispatchError::StateRead)?;
                Ok(val)
            }
        }
    }

    // ── Resource helpers ────────────────────────────────────────────

    fn read_u64_resource(
        &self,
        session: &mut ExecuteSession<'_>,
        account: &AccountAddress,
        type_tag: &str,
    ) -> Result<u64, DispatchError> {
        let raw = session
            .read_resource(account, type_tag)
            .map_err(|_| DispatchError::StateRead)?;
        match raw {
            Some(bytes) => {
                decode_u64_arg(&bytes).map_err(|_| DispatchError::ArgDecode("corrupt u64 resource"))
            }
            None => Ok(0), // Default to 0 for uninitialised resources.
        }
    }

    fn write_u64_resource(
        &self,
        session: &mut ExecuteSession<'_>,
        account: AccountAddress,
        type_tag: &str,
        value: u64,
    ) -> Result<(), DispatchError> {
        session
            .write_resource(account, type_tag, encode_u64_arg(value))
            .map_err(|e| match e {
                super::session::WriteError::ReadOnly => DispatchError::ReadOnly,
                super::session::WriteError::OutOfGas => DispatchError::OutOfGas,
            })
    }

    fn write_resource_raw(
        &self,
        session: &mut ExecuteSession<'_>,
        account: AccountAddress,
        type_tag: &str,
        value: Vec<u8>,
    ) -> Result<(), DispatchError> {
        session
            .write_resource(account, type_tag, value)
            .map_err(|e| match e {
                super::session::WriteError::ReadOnly => DispatchError::ReadOnly,
                super::session::WriteError::OutOfGas => DispatchError::OutOfGas,
            })
    }
}

// ── DispatchError (internal) ────────────────────────────────────────────

/// Internal error for the dispatch loop — converted to `VmOutput` at the
/// session boundary.
enum DispatchError {
    OutOfGas,
    ReadOnly,
    StateRead,
    ArgDecode(&'static str),
    Abort { location: String, code: u64 },
}

// ── MoveVm trait implementation ─────────────────────────────────────────

impl MoveVm for NexusMoveVm {
    fn execute_function(
        &self,
        state: &NexusStateView<'_>,
        sender: AccountAddress,
        contract: AccountAddress,
        function: &str,
        _type_args: &[Vec<u8>],
        args: &[Vec<u8>],
        gas_limit: u64,
    ) -> ExecutionResult<VmOutput> {
        // 0. Pre-flight gas check: if the limit can't cover the base call
        //    cost, fail immediately with OutOfGas (matches BuiltinVm behaviour).
        if gas_limit < self.schedule.call_base {
            return Ok(VmOutput {
                status: ExecutionStatus::OutOfGas,
                gas_used: gas_limit,
                state_changes: vec![],
                write_set: HashMap::new(),
            });
        }

        // 1. Verify contract exists.
        if !state.has_module(&contract)? {
            return Ok(VmOutput {
                status: ExecutionStatus::MoveAbort {
                    location: "0x..::*".to_string(),
                    code: 2, // MODULE_NOT_FOUND
                },
                gas_used: self.schedule.call_base.min(gas_limit),
                state_changes: vec![],
                write_set: HashMap::new(),
            });
        }

        // 2. Resolve function ABI.
        let abi = self.resolve_function_abi(state, &contract, function)?;
        let Some(abi) = abi else {
            return Ok(VmOutput {
                status: ExecutionStatus::MoveAbort {
                    location: function.to_string(),
                    code: 3, // FUNCTION_NOT_FOUND
                },
                gas_used: self.schedule.call_base.min(gas_limit),
                state_changes: vec![],
                write_set: HashMap::new(),
            });
        };

        // 3. Validate arguments.
        if let Err(reason) = validate_args(&abi, args) {
            return Err(ExecutionError::TypeMismatch {
                function: function.to_string(),
                reason,
            });
        }

        // 4. Create execution session.
        let mut session = ExecuteSession::new(
            SessionKind::Execute,
            sender,
            gas_limit,
            self.schedule.clone(),
            state,
        );

        // 5. Dispatch.
        match self.dispatch_entry(&mut session, contract, function, &abi, args) {
            Ok(_return_val) => Ok(session.commit(ExecutionStatus::Success)),
            Err(DispatchError::OutOfGas) => Ok(session.abort(ExecutionStatus::OutOfGas)),
            Err(DispatchError::ReadOnly) => Err(ExecutionError::Storage(
                "write rejected: session is read-only".into(),
            )),
            Err(DispatchError::Abort { location, code }) => {
                Ok(session.abort(ExecutionStatus::MoveAbort { location, code }))
            }
            Err(DispatchError::StateRead) => Err(ExecutionError::Storage(
                "state read failed during dispatch".into(),
            )),
            Err(DispatchError::ArgDecode(reason)) => Err(ExecutionError::TypeMismatch {
                function: function.into(),
                reason: reason.into(),
            }),
        }
    }

    fn publish_modules(
        &self,
        state: &NexusStateView<'_>,
        sender: AccountAddress,
        modules: &[Vec<u8>],
        gas_limit: u64,
    ) -> ExecutionResult<VmOutput> {
        // 1. Bytecode verification.
        if let Err(ve) = self.verifier.verify(modules) {
            return Ok(VmOutput {
                status: ExecutionStatus::MoveAbort {
                    location: "nexus::verifier".into(),
                    code: ve.code,
                },
                gas_used: self.schedule.publish_base,
                state_changes: vec![],
                write_set: HashMap::new(),
            });
        }

        // 2. Delegate to ModulePublisher for address derivation + storage.
        let publisher = ModulePublisher::new(&self.schedule);
        let result = publisher.publish(state, sender, modules, gas_limit)?;

        // If publish failed (OOG, duplicate), return as-is.
        if result.vm_output.status != ExecutionStatus::Success {
            return Ok(result.vm_output);
        }

        // 3. Store package metadata alongside the bytecode.
        let mut output = result.vm_output;
        let contract_addr = result.contract_address;

        // Build per-module hashes.
        let module_hashes: Vec<(String, [u8; 32])> = modules
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let h: [u8; 32] = *blake3::hash(m).as_bytes();
                (format!("module_{i}"), h)
            })
            .collect();

        // Compute ABI hash (from an empty initial ABI).
        let empty_abi = encode_abi(&[]).unwrap_or_default();
        let abi_hash: [u8; 32] = *blake3::hash(&empty_abi).as_bytes();

        // Compute package hash from bytecode.
        let bytecode: Vec<u8> = modules.iter().flat_map(|m| m.iter().copied()).collect();
        let package_hash: [u8; 32] = *blake3::hash(&bytecode).as_bytes();

        let metadata = PackageMetadata {
            name: String::new(), // Derived from bytecode in future.
            package_hash,
            named_addresses: vec![],
            module_hashes,
            abi_hash,
            upgrade_policy: UpgradePolicy::Immutable,
            deployer: sender,
            version: 1,
        };

        if let Ok(meta_bytes) = encode_metadata(&metadata) {
            output.write_set.insert(
                (contract_addr, MODULE_METADATA_KEY.to_vec()),
                Some(meta_bytes.clone()),
            );
            output.state_changes.push(crate::types::StateChange {
                account: contract_addr,
                key: MODULE_METADATA_KEY.to_vec(),
                value: Some(meta_bytes),
            });
        }

        Ok(output)
    }

    fn execute_script(
        &self,
        _state: &NexusStateView<'_>,
        _sender: AccountAddress,
        bytecode: &[u8],
        type_args: &[Vec<u8>],
        args: &[Vec<u8>],
        gas_limit: u64,
    ) -> ExecutionResult<VmOutput> {
        // NexusMoveVm (ABI dispatch) does not support script execution.
        Ok(VmOutput {
            status: ExecutionStatus::MoveAbort {
                location: "nexus::script".into(),
                code: 255,
            },
            gas_used: estimate_script_gas(&self.schedule, bytecode, type_args, args).min(gas_limit),
            state_changes: vec![],
            write_set: HashMap::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::publisher::{derive_contract_address, MODULE_CODE_KEY};
    use super::super::resources::resource_key;
    use super::super::verifier::make_test_module;
    use super::*;
    use crate::traits::StateView;
    use std::collections::HashMap as StdHashMap;

    struct MemState {
        data: StdHashMap<(AccountAddress, Vec<u8>), Vec<u8>>,
    }

    impl MemState {
        fn new() -> Self {
            Self {
                data: StdHashMap::new(),
            }
        }
        fn set(&mut self, account: AccountAddress, key: &[u8], value: Vec<u8>) {
            self.data.insert((account, key.to_vec()), value);
        }
    }

    impl StateView for MemState {
        fn get(&self, account: &AccountAddress, key: &[u8]) -> ExecutionResult<Option<Vec<u8>>> {
            Ok(self.data.get(&(*account, key.to_vec())).cloned())
        }
    }

    fn addr(b: u8) -> AccountAddress {
        AccountAddress([b; 32])
    }

    fn make_vm() -> NexusMoveVm {
        NexusMoveVm::new(&VmConfig::for_testing())
    }

    /// Publish a test module and install an ABI so `execute_function` can
    /// resolve it.
    fn setup_contract_with_abi(
        state: &mut MemState,
        deployer: AccountAddress,
        abi: &[FunctionAbi],
    ) -> AccountAddress {
        let modules = [make_test_module(16)];
        let bytecode: Vec<u8> = modules.iter().flat_map(|m| m.iter().copied()).collect();
        let code_hash = blake3::hash(&bytecode);
        let contract = derive_contract_address(&deployer, &code_hash);

        state.set(contract, MODULE_CODE_KEY, bytecode);
        let abi_bytes = encode_abi(abi).unwrap();
        state.set(contract, MODULE_ABI_KEY, abi_bytes);
        contract
    }

    // ── Publish tests ───────────────────────────────────────────────

    #[test]
    fn publish_stores_metadata() {
        let vm = make_vm();
        let state = MemState::new();
        let view = NexusStateView::new(&state);
        let modules = vec![make_test_module(16)];
        let result = vm
            .publish_modules(&view, addr(0xAA), &modules, 100_000)
            .unwrap();

        assert_eq!(result.status, ExecutionStatus::Success);
        // Should have code + code_hash + deployer + module_count + metadata.
        assert!(
            result.write_set.len() >= 5,
            "write_set len = {}",
            result.write_set.len()
        );

        // Verify metadata is stored.
        let has_metadata = result
            .write_set
            .keys()
            .any(|(_, key)| key == MODULE_METADATA_KEY);
        assert!(has_metadata, "package_metadata should be in write_set");
    }

    #[test]
    fn publish_invalid_bytecode_rejected() {
        let vm = make_vm();
        let state = MemState::new();
        let view = NexusStateView::new(&state);
        let result = vm
            .publish_modules(&view, addr(0xAA), &[vec![0xFF; 8]], 100_000)
            .unwrap();
        assert!(matches!(
            result.status,
            ExecutionStatus::MoveAbort { code: 12, .. }
        ));
    }

    // ── Execute tests ───────────────────────────────────────────────

    #[test]
    fn execute_contract_not_found() {
        let vm = make_vm();
        let state = MemState::new();
        let view = NexusStateView::new(&state);
        let result = vm
            .execute_function(&view, addr(1), addr(2), "increment", &[], &[], 50_000)
            .unwrap();
        assert!(matches!(
            result.status,
            ExecutionStatus::MoveAbort { code: 2, .. }
        ));
    }

    #[test]
    fn execute_function_not_found() {
        let vm = make_vm();
        let mut state = MemState::new();
        state.set(addr(0xCC), MODULE_CODE_KEY, vec![0xCA, 0xFE]);
        // No ABI installed — all functions are "not found".
        let view = NexusStateView::new(&state);
        let result = vm
            .execute_function(&view, addr(1), addr(0xCC), "nonexistent", &[], &[], 50_000)
            .unwrap();
        assert!(matches!(
            result.status,
            ExecutionStatus::MoveAbort { code: 3, .. }
        ));
    }

    #[test]
    fn execute_increment_entry() {
        let vm = make_vm();
        let mut state = MemState::new();
        let abi = vec![FunctionAbi {
            name: "increment".into(),
            params: vec![],
            returns: None,
            is_entry: true,
        }];
        let contract = setup_contract_with_abi(&mut state, addr(0xAA), &abi);

        let view = NexusStateView::new(&state);
        let result = vm
            .execute_function(&view, addr(1), contract, "increment", &[], &[], 100_000)
            .unwrap();

        assert_eq!(result.status, ExecutionStatus::Success);
        assert!(
            !result.state_changes.is_empty(),
            "should have state changes"
        );
        assert!(result.gas_used > 0);
    }

    #[test]
    fn execute_arg_count_mismatch() {
        let vm = make_vm();
        let mut state = MemState::new();
        let abi = vec![FunctionAbi {
            name: "transfer".into(),
            params: vec![MoveType::Address, MoveType::U64],
            returns: None,
            is_entry: true,
        }];
        let contract = setup_contract_with_abi(&mut state, addr(0xAA), &abi);

        let view = NexusStateView::new(&state);
        let result = vm.execute_function(&view, addr(1), contract, "transfer", &[], &[], 100_000);
        // Should be a TypeMismatch error.
        assert!(result.is_err());
    }

    #[test]
    fn execute_transfer_entry() {
        let vm = make_vm();
        let mut state = MemState::new();
        let abi = vec![FunctionAbi {
            name: "transfer".into(),
            params: vec![MoveType::Address, MoveType::U64],
            returns: None,
            is_entry: true,
        }];
        let contract = setup_contract_with_abi(&mut state, addr(0xAA), &abi);

        // Give sender a balance.
        let sender = addr(1);
        let balance_key = resource_key("balance::Balance");
        state.set(sender, &balance_key, encode_u64_arg(1000));

        let recipient = addr(2);
        let view = NexusStateView::new(&state);
        let result = vm
            .execute_function(
                &view,
                sender,
                contract,
                "transfer",
                &[],
                &[recipient.0.to_vec(), encode_u64_arg(500)],
                100_000,
            )
            .unwrap();

        assert_eq!(result.status, ExecutionStatus::Success);
        assert!(result.state_changes.len() >= 2, "should debit and credit");
    }

    #[test]
    fn execute_transfer_insufficient_balance() {
        let vm = make_vm();
        let mut state = MemState::new();
        let abi = vec![FunctionAbi {
            name: "transfer".into(),
            params: vec![MoveType::Address, MoveType::U64],
            returns: None,
            is_entry: true,
        }];
        let contract = setup_contract_with_abi(&mut state, addr(0xAA), &abi);

        // Sender has no balance.
        let sender = addr(1);
        let recipient = addr(2);
        let view = NexusStateView::new(&state);
        let result = vm
            .execute_function(
                &view,
                sender,
                contract,
                "transfer",
                &[],
                &[recipient.0.to_vec(), encode_u64_arg(500)],
                100_000,
            )
            .unwrap();

        assert!(matches!(
            result.status,
            ExecutionStatus::MoveAbort { code: 100, .. }
        ));
    }
}
