//! Built-in VM implementation.
//!
//! [`BuiltinVm`] provides a minimal Move-compatible execution environment
//! for native transfers and placeholder Move call/publish operations.
//! It will be replaced by a real Move VM (`move-vm-runtime`) in a
//! future feature-gated implementation.
//!
//! # Supported Operations
//!
//! | Payload | Behaviour |
//! |---------|-----------|
//! | `MoveCall` | Verifies contract exists, deducts gas, returns Success |
//! | `MovePublish` | Bytecode verification → contract address derivation → storage |

use std::collections::HashMap;
use std::fmt::Write;

use crate::error::ExecutionResult;
use crate::types::ExecutionStatus;
use nexus_primitives::AccountAddress;

use super::gas_meter::{estimate_script_gas, GasMeter, GasSchedule, SimpleGasMeter};
use super::publisher::ModulePublisher;
use super::state_view::NexusStateView;
use super::verifier::BytecodeVerifier;
use super::vm_config::VmConfig;
use super::{MoveVm, VmOutput};

/// Built-in VM that handles native operations and placeholders.
pub(crate) struct BuiltinVm {
    /// Gas schedule derived from config.
    schedule: GasSchedule,
    /// Structural bytecode verifier (T-2005).
    verifier: BytecodeVerifier,
}

impl BuiltinVm {
    /// Create a new built-in VM with the given configuration.
    pub fn new(config: &VmConfig) -> Self {
        Self {
            schedule: GasSchedule::from_config(config),
            verifier: BytecodeVerifier::from_vm_config(config),
        }
    }
}

impl MoveVm for BuiltinVm {
    fn execute_function(
        &self,
        state: &NexusStateView<'_>,
        _sender: AccountAddress,
        contract: AccountAddress,
        _function: &str,
        _type_args: &[Vec<u8>],
        _args: &[Vec<u8>],
        gas_limit: u64,
    ) -> ExecutionResult<VmOutput> {
        // Create a gas meter for this call.
        let mut meter = SimpleGasMeter::new(gas_limit);

        // Charge base call gas.
        if meter.charge(self.schedule.call_base).is_err() {
            return Ok(VmOutput {
                status: ExecutionStatus::OutOfGas,
                gas_used: gas_limit,
                state_changes: vec![],
                write_set: HashMap::new(),
            });
        }

        // Verify contract exists.
        let has_code = state.has_module(&contract)?;
        if !has_code {
            // Format first 4 bytes of address for error message.
            let prefix = contract.0.iter().take(4).fold(String::new(), |mut s, b| {
                let _ = write!(s, "{b:02x}");
                s
            });
            return Ok(VmOutput {
                status: ExecutionStatus::MoveAbort {
                    location: format!("0x{prefix}::*"),
                    code: 2, // MODULE_NOT_FOUND
                },
                gas_used: meter.consumed(),
                state_changes: vec![],
                write_set: HashMap::new(),
            });
        }

        // Placeholder: consume base gas, no state changes.
        Ok(VmOutput {
            status: ExecutionStatus::Success,
            gas_used: meter.consumed(),
            state_changes: vec![],
            write_set: HashMap::new(),
        })
    }

    fn publish_modules(
        &self,
        state: &NexusStateView<'_>,
        sender: AccountAddress,
        modules: &[Vec<u8>],
        gas_limit: u64,
    ) -> ExecutionResult<VmOutput> {
        // Step 1: Bytecode verification (T-2005).
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

        // Step 2: Delegate to ModulePublisher for address derivation + storage.
        let publisher = ModulePublisher::new(&self.schedule);
        let result = publisher.publish(state, sender, modules, gas_limit)?;
        Ok(result.vm_output)
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
    use super::super::publisher::{derive_contract_address, MODULE_CODE_HASH_KEY, MODULE_CODE_KEY};
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

    fn make_vm() -> BuiltinVm {
        BuiltinVm::new(&VmConfig::for_testing())
    }

    #[test]
    fn call_out_of_gas() {
        let vm = make_vm();
        let state = MemState::new();
        let view = NexusStateView::new(&state);
        let result = vm
            .execute_function(&view, addr(0xAA), addr(0xBB), "do_thing", &[], &[], 100)
            .unwrap();
        assert_eq!(result.status, ExecutionStatus::OutOfGas);
    }

    #[test]
    fn call_contract_not_found() {
        let vm = make_vm();
        let state = MemState::new();
        let view = NexusStateView::new(&state);
        let result = vm
            .execute_function(&view, addr(0xAA), addr(0xBB), "do_thing", &[], &[], 50_000)
            .unwrap();
        assert!(
            matches!(result.status, ExecutionStatus::MoveAbort { code: 2, .. }),
            "expected MoveAbort code 2, got {:?}",
            result.status
        );
    }

    #[test]
    fn call_success_when_contract_exists() {
        let vm = make_vm();
        let mut state = MemState::new();
        state.set(addr(0xBB), MODULE_CODE_KEY, vec![0xCA, 0xFE]);
        let view = NexusStateView::new(&state);
        let result = vm
            .execute_function(&view, addr(0xAA), addr(0xBB), "do_thing", &[], &[], 50_000)
            .unwrap();
        assert_eq!(result.status, ExecutionStatus::Success);
        assert_eq!(result.gas_used, VmConfig::for_testing().call_base_gas);
    }

    #[test]
    fn publish_success() {
        let vm = make_vm();
        let state = MemState::new();
        let view = NexusStateView::new(&state);
        let modules = vec![make_test_module(16)];
        let result = vm
            .publish_modules(&view, addr(0xAA), &modules, 50_000)
            .unwrap();
        assert_eq!(result.status, ExecutionStatus::Success);
        assert!(!result.state_changes.is_empty());
        assert!(!result.write_set.is_empty());

        // Compute expected contract address.
        let bytecode: Vec<u8> = modules.iter().flat_map(|m| m.iter().copied()).collect();
        let code_hash = blake3::hash(&bytecode);
        let contract_addr = derive_contract_address(&addr(0xAA), &code_hash);

        // Verify bytecode was stored under the derived contract address.
        let code = result
            .write_set
            .get(&(contract_addr, MODULE_CODE_KEY.to_vec()));
        assert!(
            code.is_some(),
            "bytecode should be stored under derived contract address"
        );

        // Verify hash was stored.
        let hash = result
            .write_set
            .get(&(contract_addr, MODULE_CODE_HASH_KEY.to_vec()));
        assert!(hash.is_some());
    }

    #[test]
    fn publish_out_of_gas() {
        let vm = make_vm();
        let state = MemState::new();
        let view = NexusStateView::new(&state);
        let modules = vec![make_test_module(100)];
        let result = vm.publish_modules(&view, addr(0xAA), &modules, 10).unwrap();
        assert_eq!(result.status, ExecutionStatus::OutOfGas);
    }

    #[test]
    fn publish_binary_too_large() {
        let cfg = VmConfig {
            max_binary_size: 10,
            ..VmConfig::for_testing()
        };
        let vm = BuiltinVm::new(&cfg);
        let state = MemState::new();
        let view = NexusStateView::new(&state);
        // Module with magic + version (8 bytes) + 20 extra = 28 > 10.
        let modules = vec![make_test_module(20)];
        let result = vm
            .publish_modules(&view, addr(0xAA), &modules, 50_000)
            .unwrap();
        // Verifier catches this: code 14 (per-module size) or 15 (total size).
        assert!(
            matches!(result.status, ExecutionStatus::MoveAbort { code, .. } if code == 14 || code == 15),
            "expected verifier reject, got {:?}",
            result.status
        );
    }

    #[test]
    fn publish_invalid_bytecode_rejected() {
        let vm = make_vm();
        let state = MemState::new();
        let view = NexusStateView::new(&state);
        // No Move magic bytes.
        let modules = vec![vec![0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08]];
        let result = vm
            .publish_modules(&view, addr(0xAA), &modules, 50_000)
            .unwrap();
        assert!(
            matches!(result.status, ExecutionStatus::MoveAbort { code: 12, .. }),
            "expected magic bytes rejection (code 12), got {:?}",
            result.status
        );
    }

    #[test]
    fn publish_empty_modules_rejected() {
        let vm = make_vm();
        let state = MemState::new();
        let view = NexusStateView::new(&state);
        let result = vm.publish_modules(&view, addr(0xAA), &[], 50_000).unwrap();
        assert!(
            matches!(result.status, ExecutionStatus::MoveAbort { code: 10, .. }),
            "expected empty modules rejection (code 10), got {:?}",
            result.status
        );
    }

    #[test]
    fn publish_gas_includes_per_byte() {
        let cfg = VmConfig {
            publish_base_gas: 100,
            publish_per_byte_gas: 10,
            max_binary_size: 10_000,
            ..VmConfig::for_testing()
        };
        let vm = BuiltinVm::new(&cfg);
        let state = MemState::new();
        let view = NexusStateView::new(&state);
        let modules = vec![make_test_module(42)]; // 8 + 42 = 50 bytes
        let result = vm
            .publish_modules(&view, addr(0xAA), &modules, 50_000)
            .unwrap();
        assert_eq!(result.status, ExecutionStatus::Success);
        // 100 + 50 * 10 = 600
        assert_eq!(result.gas_used, 600);
    }

    #[test]
    fn publish_duplicate_module_rejected() {
        let vm = make_vm();
        let state = MemState::new();
        let view = NexusStateView::new(&state);
        let m = make_test_module(16);
        let result = vm
            .publish_modules(&view, addr(0xAA), &[m.clone(), m], 50_000)
            .unwrap();
        assert!(
            matches!(result.status, ExecutionStatus::MoveAbort { code: 16, .. }),
            "expected duplicate module rejection (code 16), got {:?}",
            result.status
        );
    }
}
