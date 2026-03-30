// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Move VM adapter — isolation boundary for Move bytecode execution.
//!
//! All Move VM internals (bytecode dispatch, module resolution, gas tables)
//! are encapsulated behind the [`MoveVm`] trait.  No Move-internal types
//! leak beyond this module boundary (TLD-03 §5 isolation principle).
//!
//! # Submodules
//!
//! - [`vm_config`]    — VM configuration (gas schedule, binary size limits)
//! - [`state_view`]   — [`NexusStateView`]: bridges [`StateView`] for module/resource resolution
//! - [`builtin_vm`]   — Built-in VM: native transfer + placeholder Move call/publish
//! - [`type_bridge`]  — Move ↔ Nexus type conversions
//! - [`gas_meter`]    — Gas metering trait and schedule
//!
//! # Future
//!
//! When `move-vm-runtime` is integrated (behind a `move-vm`
//! feature gate), a `RealMoveVm` struct will implement [`MoveVm`] and
//! replace `BuiltinVm` as the default.

pub(crate) mod abi;
pub(crate) mod builtin_vm;
pub(crate) mod entry_function;
pub mod events;
pub(crate) mod gas_meter;
#[cfg(feature = "move-vm")]
pub(crate) mod move_runtime;
pub(crate) mod nexus_vm;
pub(crate) mod package;
pub(crate) mod publisher;
pub(crate) mod query;
pub(crate) mod resources;
pub(crate) mod session;
pub(crate) mod state_view;
#[cfg(feature = "move-vm")]
pub(crate) mod stdlib;
pub(crate) mod type_bridge;
pub(crate) mod verifier;
pub(crate) mod vm_config;

use std::collections::HashMap;

use crate::error::ExecutionResult;
use crate::types::{ExecutionStatus, StateChange};
use nexus_primitives::AccountAddress;

pub(crate) use builtin_vm::BuiltinVm;
#[allow(unused_imports)]
pub(crate) use gas_meter::{GasExhausted, GasMeter, GasSchedule, SimpleGasMeter};
#[cfg(not(feature = "move-vm"))]
pub(crate) use nexus_vm::NexusMoveVm;
#[allow(unused_imports)]
pub(crate) use publisher::derive_contract_address;
pub(crate) use state_view::NexusStateView;
pub(crate) use type_bridge::contract_to_account;
#[allow(unused_imports)]
pub(crate) use verifier::BytecodeVerifier;
pub(crate) use vm_config::VmConfig;

#[cfg(feature = "move-vm")]
pub(crate) use move_runtime::MoveRuntime;

// ── Move VM execution output ────────────────────────────────────────────

/// The result of executing a single Move session (call or publish).
#[derive(Debug, Clone)]
pub(crate) struct VmOutput {
    /// Execution outcome.
    pub status: ExecutionStatus,
    /// Gas consumed.
    pub gas_used: u64,
    /// State mutations produced.
    pub state_changes: Vec<StateChange>,
    /// Raw write-set: keys written and their new values.
    pub write_set: HashMap<(AccountAddress, Vec<u8>), Option<Vec<u8>>>,
}

// ── MoveVm trait ────────────────────────────────────────────────────────

/// **\[INTERNAL\]** Abstract interface to a Move-compatible VM.
///
/// Implementors:
/// - [`BuiltinVm`] — native transfer + placeholder call/publish (Phase 1-2)
/// - Future: `MoveRuntime` backed by `move-vm-runtime` (feature-gated)
///
/// The trait is object-safe so that `BlockStmExecutor` can hold a
/// `Box<dyn MoveVm>` and swap implementations at runtime.
pub(crate) trait MoveVm: Send + Sync {
    /// Execute a Move function call.
    ///
    /// # Arguments
    /// - `state` — state view for reading accounts/modules
    /// - `sender` — transaction sender
    /// - `contract` — target contract address
    /// - `function` — fully qualified function name
    /// - `type_args` — serialised type arguments
    /// - `args` — BCS-encoded call arguments
    /// - `gas_limit` — maximum gas for this call
    #[allow(clippy::too_many_arguments)]
    fn execute_function(
        &self,
        state: &NexusStateView<'_>,
        sender: AccountAddress,
        contract: AccountAddress,
        function: &str,
        type_args: &[Vec<u8>],
        args: &[Vec<u8>],
        gas_limit: u64,
    ) -> ExecutionResult<VmOutput>;

    /// Publish Move modules.
    ///
    /// # Arguments
    /// - `state` — state view for reading existing modules
    /// - `sender` — publishing account
    /// - `modules` — compiled Move bytecode modules
    /// - `gas_limit` — maximum gas for this publish
    fn publish_modules(
        &self,
        state: &NexusStateView<'_>,
        sender: AccountAddress,
        modules: &[Vec<u8>],
        gas_limit: u64,
    ) -> ExecutionResult<VmOutput>;

    /// Execute a compiled Move script (one-off bytecode, not published).
    ///
    /// # Arguments
    /// - `state` — state view for reading on-chain data
    /// - `sender` — the account executing the script
    /// - `bytecode` — compiled script bytecode
    /// - `type_args` — serialised type arguments
    /// - `args` — BCS-encoded call arguments
    /// - `gas_limit` — maximum gas for this execution
    fn execute_script(
        &self,
        state: &NexusStateView<'_>,
        sender: AccountAddress,
        bytecode: &[u8],
        type_args: &[Vec<u8>],
        args: &[Vec<u8>],
        gas_limit: u64,
    ) -> ExecutionResult<VmOutput>;
}

// ── MoveExecutor (lifecycle manager) ────────────────────────────────────

/// Lifecycle manager for the Move VM.
///
/// Owns the VM instance and its configuration. Created once per
/// [`BlockStmExecutor`](crate::BlockStmExecutor) and shared across
/// all transaction executions within a block.
pub(crate) struct MoveExecutor {
    /// The VM implementation.
    vm: Box<dyn MoveVm>,
    /// VM configuration (used by future gas-metering extensions).
    #[allow(dead_code)]
    config: VmConfig,
}

impl MoveExecutor {
    /// Create a new executor with the best available VM.
    ///
    /// When the `move-vm` feature is active, uses `MoveRuntime` (real
    /// upstream Move interpreter).  Otherwise falls back to `NexusMoveVm`
    /// (ABI-driven dispatch).
    pub fn new(config: VmConfig) -> Self {
        #[cfg(feature = "move-vm")]
        let vm: Box<dyn MoveVm> = Box::new(MoveRuntime::new(&config));
        #[cfg(not(feature = "move-vm"))]
        let vm: Box<dyn MoveVm> = Box::new(NexusMoveVm::new(&config));
        Self { vm, config }
    }

    /// Create an executor backed by the legacy built-in VM (native only).
    #[allow(dead_code)]
    pub fn with_builtin(config: VmConfig) -> Self {
        Self {
            vm: Box::new(BuiltinVm::new(&config)),
            config,
        }
    }

    /// Create an executor with a custom VM implementation (for testing).
    #[cfg(test)]
    #[allow(dead_code)]
    pub fn with_vm(vm: Box<dyn MoveVm>, config: VmConfig) -> Self {
        Self { vm, config }
    }

    /// Execute a Move function call.
    #[allow(clippy::too_many_arguments)]
    pub fn execute_function(
        &self,
        state: &NexusStateView<'_>,
        sender: AccountAddress,
        contract: AccountAddress,
        function: &str,
        type_args: &[Vec<u8>],
        args: &[Vec<u8>],
        gas_limit: u64,
    ) -> ExecutionResult<VmOutput> {
        self.vm.execute_function(
            state, sender, contract, function, type_args, args, gas_limit,
        )
    }

    /// Publish Move modules.
    pub fn publish_modules(
        &self,
        state: &NexusStateView<'_>,
        sender: AccountAddress,
        modules: &[Vec<u8>],
        gas_limit: u64,
    ) -> ExecutionResult<VmOutput> {
        self.vm.publish_modules(state, sender, modules, gas_limit)
    }

    /// Execute a compiled Move script.
    pub fn execute_script(
        &self,
        state: &NexusStateView<'_>,
        sender: AccountAddress,
        bytecode: &[u8],
        type_args: &[Vec<u8>],
        args: &[Vec<u8>],
        gas_limit: u64,
    ) -> ExecutionResult<VmOutput> {
        self.vm
            .execute_script(state, sender, bytecode, type_args, args, gas_limit)
    }

    /// Access the VM configuration.
    #[allow(dead_code)]
    pub fn config(&self) -> &VmConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use crate::traits::StateView;
    use nexus_primitives::AccountAddress;

    struct EmptyState;

    impl StateView for EmptyState {
        fn get(&self, _account: &AccountAddress, _key: &[u8]) -> ExecutionResult<Option<Vec<u8>>> {
            Ok(None)
        }
    }

    struct MockVm {
        calls: Arc<Mutex<Vec<&'static str>>>,
    }

    impl MockVm {
        fn new(calls: Arc<Mutex<Vec<&'static str>>>) -> Self {
            Self { calls }
        }

        fn output(gas_used: u64) -> VmOutput {
            VmOutput {
                status: ExecutionStatus::Success,
                gas_used,
                state_changes: Vec::new(),
                write_set: HashMap::new(),
            }
        }
    }

    impl MoveVm for MockVm {
        fn execute_function(
            &self,
            _state: &NexusStateView<'_>,
            _sender: AccountAddress,
            _contract: AccountAddress,
            _function: &str,
            _type_args: &[Vec<u8>],
            _args: &[Vec<u8>],
            _gas_limit: u64,
        ) -> ExecutionResult<VmOutput> {
            self.calls.lock().unwrap().push("execute_function");
            Ok(Self::output(11))
        }

        fn publish_modules(
            &self,
            _state: &NexusStateView<'_>,
            _sender: AccountAddress,
            _modules: &[Vec<u8>],
            _gas_limit: u64,
        ) -> ExecutionResult<VmOutput> {
            self.calls.lock().unwrap().push("publish_modules");
            Ok(Self::output(22))
        }

        fn execute_script(
            &self,
            _state: &NexusStateView<'_>,
            _sender: AccountAddress,
            _bytecode: &[u8],
            _type_args: &[Vec<u8>],
            _args: &[Vec<u8>],
            _gas_limit: u64,
        ) -> ExecutionResult<VmOutput> {
            self.calls.lock().unwrap().push("execute_script");
            Ok(Self::output(33))
        }
    }

    #[test]
    fn move_vm_trait_is_object_safe() {
        fn _accepts(_: &dyn MoveVm) {}
    }

    #[test]
    fn move_executor_creates_with_defaults() {
        let exec = MoveExecutor::new(VmConfig::default());
        assert_eq!(
            exec.config().max_binary_size,
            VmConfig::default().max_binary_size
        );
    }

    #[test]
    fn move_executor_with_builtin_preserves_config() {
        let config = VmConfig {
            max_binary_size: 1234,
            call_base_gas: 44,
            publish_base_gas: 55,
            publish_per_byte_gas: 2,
            read_per_byte_gas: 3,
            write_per_byte_gas: 4,
        };
        let exec = MoveExecutor::with_builtin(config.clone());

        assert_eq!(exec.config().max_binary_size, config.max_binary_size);
        assert_eq!(exec.config().publish_base_gas, config.publish_base_gas);
    }

    #[test]
    fn move_executor_delegates_to_custom_vm() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let exec = MoveExecutor::with_vm(Box::new(MockVm::new(calls.clone())), VmConfig::default());
        let state = EmptyState;
        let view = NexusStateView::new(&state);
        let sender = AccountAddress([0x11; 32]);
        let contract = AccountAddress([0x22; 32]);

        let function_output = exec
            .execute_function(&view, sender, contract, "counter::get_count", &[], &[], 100)
            .unwrap();
        let publish_output = exec
            .publish_modules(&view, sender, &[vec![1, 2, 3]], 200)
            .unwrap();
        let script_output = exec
            .execute_script(&view, sender, &[4, 5, 6], &[], &[], 300)
            .unwrap();

        assert_eq!(function_output.gas_used, 11);
        assert_eq!(publish_output.gas_used, 22);
        assert_eq!(script_output.gas_used, 33);
        assert_eq!(
            *calls.lock().unwrap(),
            vec!["execute_function", "publish_modules", "execute_script"]
        );
    }
}
