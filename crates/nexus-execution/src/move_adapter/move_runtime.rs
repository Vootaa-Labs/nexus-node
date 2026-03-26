//! Real Move VM adapter backed by `move-vm-runtime`.
//!
//! Feature-gated behind `move-vm`.  When active, [`MoveRuntime`] provides
//! real bytecode execution through the upstream Move interpreter, replacing
//! the ABI-driven dispatch in [`super::NexusMoveVm`].
//!
//! # Architecture
//!
//! ```text
//! ┌────────────────────────────────────────────────────┐
//! │  MoveVm trait (execute_function / publish_modules) │
//! └──────────────┬─────────────────────────────────────┘
//!                │ impl
//! ┌──────────────▼─────────────────────────────────────┐
//! │  MoveRuntime                                       │
//! │    ├ RuntimeEnvironment (config, natives, caches)  │
//! │    └ VmConfig (gas limits etc.)                    │
//! └──────────────┬─────────────────────────────────────┘
//!                │ per-call
//! ┌──────────────▼─────────────────────────────────────┐
//! │  NexusBytesStorage                                 │
//! │    ├ ModuleBytesStorage (fetch raw .mv bytes)      │
//! │    ├ WithRuntimeEnvironment                        │
//! │    └ ResourceResolver (get resource bytes)         │
//! └────────────────────────────────────────────────────┘
//! ```
//!
//! # Module storage convention
//!
//! Nexus stores one compiled module per contract address under the key
//! `b"code"`.  The Move module's `self_id()` encodes both the address
//! and module name.  For the initial integration we keep the existing
//! single-module-per-address convention and derive the module name by
//! deserializing the stored bytecode header.

use std::collections::HashMap;

use bytes::Bytes;
use nexus_move_runtime::upstream::move_binary_format::access::ModuleAccess;
use nexus_move_runtime::upstream::move_binary_format::errors::{
    Location, PartialVMError, PartialVMResult, VMResult,
};
use nexus_move_runtime::upstream::move_binary_format::file_format_common::{
    IDENTIFIER_SIZE_MAX, VERSION_MAX,
};
use nexus_move_runtime::upstream::move_binary_format::{
    deserializer::DeserializerConfig, CompiledModule,
};
use nexus_move_runtime::upstream::move_core_types::account_address::AccountAddress as MoveAddress;
use nexus_move_runtime::upstream::move_core_types::identifier::{IdentStr, Identifier};
use nexus_move_runtime::upstream::move_core_types::language_storage::{
    ModuleId, StructTag, TypeTag,
};
use nexus_move_runtime::upstream::move_core_types::metadata::Metadata;
use nexus_move_runtime::upstream::move_core_types::value::MoveTypeLayout;
use nexus_move_runtime::upstream::move_core_types::vm_status::StatusCode;
use nexus_move_runtime::upstream::move_vm_runtime::data_cache::TransactionDataCache;
use nexus_move_runtime::upstream::move_vm_runtime::module_traversal::{
    TraversalContext, TraversalStorage,
};
use nexus_move_runtime::upstream::move_vm_runtime::move_vm::MoveVM;
use nexus_move_runtime::upstream::move_vm_runtime::native_extensions::NativeContextExtensions;
use nexus_move_runtime::upstream::move_vm_runtime::{
    AsUnsyncModuleStorage, ModuleStorage, RuntimeEnvironment, WithRuntimeEnvironment,
};
use nexus_move_runtime::upstream::move_vm_types::code::ModuleBytesStorage;
use nexus_move_runtime::upstream::move_vm_types::gas::UnmeteredGasMeter;
use nexus_move_runtime::upstream::move_vm_types::resolver::ResourceResolver;

use crate::error::{ExecutionError, ExecutionResult};
use crate::types::ExecutionStatus;
use nexus_primitives::AccountAddress;

use super::gas_meter::{
    clamp_gas_to_limit, estimate_call_gas, estimate_publish_gas, estimate_script_gas, GasSchedule,
};
use super::publisher::MODULE_CODE_KEY;
use super::state_view::NexusStateView;
use super::vm_config::VmConfig;
use super::{MoveVm, VmOutput};

// ── MoveRuntime ─────────────────────────────────────────────────────────

/// Real Move VM backed by `move-vm-runtime`.
///
/// Stateless — the `RuntimeEnvironment` holds native function registrations,
/// VM config, and shared caches.  Each call constructs ephemeral
/// `NexusBytesStorage` + `UnsyncModuleStorage` from the state view.
pub(crate) struct MoveRuntime {
    /// Upstream runtime environment (natives, config, struct caches).
    runtime_env: RuntimeEnvironment,
    /// Nexus-side schedule used to account for request and write costs.
    schedule: GasSchedule,
    /// Nexus-side config (gas limits, bytecode size caps).
    #[allow(dead_code)]
    config: VmConfig,
}

impl MoveRuntime {
    /// Create a new real Move VM.
    ///
    /// No native functions are registered initially — standard library
    /// functions are pure bytecode in Move.
    pub fn new(config: &VmConfig) -> Self {
        // Register native functions required by the Move standard library.
        let natives = super::stdlib::native_functions();
        let runtime_env = RuntimeEnvironment::new(natives);
        Self {
            runtime_env,
            schedule: GasSchedule::from_config(config),
            config: config.clone(),
        }
    }
}

impl MoveVm for MoveRuntime {
    fn execute_function(
        &self,
        state: &NexusStateView<'_>,
        sender: AccountAddress,
        contract: AccountAddress,
        function: &str,
        type_args: &[Vec<u8>],
        args: &[Vec<u8>],
        gas_limit: u64,
    ) -> ExecutionResult<VmOutput> {
        // -- Parse function name: "module_name::function_name"
        let (module_name, fn_name) = match parse_function_name(function) {
            Ok(pair) => pair,
            Err(_) => {
                return Ok(VmOutput {
                    status: ExecutionStatus::MoveAbort {
                        location: format!("{}::{}", contract, function),
                        code: 4001, // LINKER_ERROR
                    },
                    gas_used: clamp_gas_to_limit(self.schedule.call_base, gas_limit),
                    state_changes: Vec::new(),
                    write_set: HashMap::new(),
                });
            }
        };

        let move_addr = nexus_to_move_address(&contract);
        let module_ident = match Identifier::new(module_name) {
            Ok(id) => id,
            Err(_) => {
                return Ok(VmOutput {
                    status: ExecutionStatus::MoveAbort {
                        location: format!("{module_name}::{fn_name}"),
                        code: 4001,
                    },
                    gas_used: clamp_gas_to_limit(self.schedule.call_base, gas_limit),
                    state_changes: Vec::new(),
                    write_set: HashMap::new(),
                });
            }
        };
        let module_id = ModuleId::new(move_addr, module_ident);
        let fn_ident = match Identifier::new(fn_name) {
            Ok(id) => id,
            Err(_) => {
                return Ok(VmOutput {
                    status: ExecutionStatus::MoveAbort {
                        location: format!("{module_name}::{fn_name}"),
                        code: 4001,
                    },
                    gas_used: clamp_gas_to_limit(self.schedule.call_base, gas_limit),
                    state_changes: Vec::new(),
                    write_set: HashMap::new(),
                });
            }
        };

        // -- Deserialize type arguments (empty for now — future extension)
        let ty_args = deserialize_type_args(type_args)?;

        // -- Build per-call storage bridges
        let bytes_storage = NexusBytesStorage::new(state, &self.runtime_env);
        let module_storage = bytes_storage.as_unsync_module_storage();

        // -- Load function from module storage
        let loaded_fn = match module_storage.load_function(&module_id, &fn_ident, &ty_args) {
            Ok(f) => f,
            Err(e) => {
                // Return a graceful abort instead of a hard error so that
                // Block-STM can produce a receipt for this transaction
                // rather than failing the entire batch.
                return Ok(VmOutput {
                    status: ExecutionStatus::MoveAbort {
                        location: format!("{module_id}"),
                        code: e.major_status() as u64,
                    },
                    gas_used: clamp_gas_to_limit(self.schedule.call_base, gas_limit),
                    state_changes: Vec::new(),
                    write_set: HashMap::new(),
                });
            }
        };

        // -- Prepare execution scaffolding
        let mut data_cache = TransactionDataCache::empty();
        let mut gas_meter = UnmeteredGasMeter;
        let traversal_storage = TraversalStorage::new();
        let mut traversal_ctx = TraversalContext::new(&traversal_storage);
        let mut extensions = NativeContextExtensions::default();

        // -- Prepend sender address to args (Move convention: first arg is signer)
        // The Move VM's signer deserialization uses RuntimeVariants layout:
        // variant 0 = single address. BCS encoding: [0x00 (variant index), 32 address bytes].
        let mut sender_bytes = vec![0u8]; // variant index 0
        sender_bytes.extend_from_slice(&nexus_to_move_address(&sender).into_bytes());
        let mut full_args: Vec<Vec<u8>> = vec![sender_bytes];
        full_args.extend(args.iter().cloned());

        // -- Execute
        let result = MoveVM::execute_loaded_function(
            loaded_fn,
            full_args,
            &mut data_cache,
            &mut gas_meter,
            &mut traversal_ctx,
            &mut extensions,
            &module_storage,
            &bytes_storage,
        );

        match result {
            Ok(_return_values) => {
                // Convert data cache effects into Nexus state changes
                let change_set = data_cache.into_effects(&module_storage).map_err(|e| {
                    ExecutionError::Storage(format!("effects extraction failed: {e}"))
                })?;

                let (state_changes, write_set) = changeset_to_nexus(change_set);

                Ok(VmOutput {
                    status: ExecutionStatus::Success,
                    gas_used: clamp_gas_to_limit(
                        estimate_call_gas(&self.schedule, type_args, args, &state_changes),
                        gas_limit,
                    ),
                    state_changes,
                    write_set,
                })
            }
            Err(vm_err) => Ok(VmOutput {
                status: ExecutionStatus::MoveAbort {
                    location: format!("{}", vm_err.location()),
                    code: vm_err.major_status() as u64,
                },
                gas_used: clamp_gas_to_limit(
                    estimate_call_gas(&self.schedule, type_args, args, &[]),
                    gas_limit,
                ),
                state_changes: Vec::new(),
                write_set: HashMap::new(),
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
        let config = DeserializerConfig::new(VERSION_MAX, IDENTIFIER_SIZE_MAX);
        let move_sender = nexus_to_move_address(&sender);

        let mut state_changes = Vec::new();
        let mut write_set = HashMap::new();

        for module_bytes in modules {
            // Deserialize to validate structure
            let compiled =
                CompiledModule::deserialize_with_config(module_bytes, &config).map_err(|e| {
                    ExecutionError::BytecodeVerification {
                        reason: format!("module deserialization failed: {e}"),
                    }
                })?;

            // Verify the module is published under the sender's address
            if compiled.self_id().address() != &move_sender {
                return Err(ExecutionError::BytecodeVerification {
                    reason: "module address does not match sender".into(),
                });
            }

            // Bytecode verification using the runtime environment
            let bytes_storage = NexusBytesStorage::new(state, &self.runtime_env);
            let module_storage = bytes_storage.as_unsync_module_storage();
            self.runtime_env
                .build_locally_verified_module(
                    std::sync::Arc::new(compiled.clone()),
                    module_bytes.len(),
                    &blake3::hash(module_bytes).as_bytes().clone(),
                )
                .map_err(|e| ExecutionError::BytecodeVerification {
                    reason: format!("bytecode verification failed: {e}"),
                })?;

            // Verify that all dependencies exist
            for dep in compiled.immediate_dependencies() {
                let dep_exists = module_storage
                    .check_module_exists(dep.address(), dep.name())
                    .map_err(|e| {
                        ExecutionError::Storage(format!("dependency check failed: {e}"))
                    })?;
                if !dep_exists && dependency_must_exist(dep.address(), &move_sender) {
                    return Err(ExecutionError::BytecodeVerification {
                        reason: format!("missing dependency: {dep}"),
                    });
                }
            }

            // Store module bytecode under contract address
            let contract_addr = move_to_nexus_address(compiled.self_id().address());
            let code_bytes = module_bytes.clone();
            let code_hash = blake3::hash(&code_bytes);

            state_changes.push(crate::types::StateChange {
                account: contract_addr,
                key: MODULE_CODE_KEY.to_vec(),
                value: Some(code_bytes.clone()),
            });
            state_changes.push(crate::types::StateChange {
                account: contract_addr,
                key: b"code_hash".to_vec(),
                value: Some(code_hash.as_bytes().to_vec()),
            });

            write_set.insert((contract_addr, MODULE_CODE_KEY.to_vec()), Some(code_bytes));
            write_set.insert(
                (contract_addr, b"code_hash".to_vec()),
                Some(code_hash.as_bytes().to_vec()),
            );
        }

        Ok(VmOutput {
            status: ExecutionStatus::Success,
            gas_used: clamp_gas_to_limit(
                estimate_publish_gas(&self.schedule, modules, &state_changes),
                gas_limit,
            ),
            state_changes,
            write_set,
        })
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
        // Full script execution requires VM session support — placeholder for now.
        Ok(VmOutput {
            status: ExecutionStatus::MoveAbort {
                location: "nexus::script".into(),
                code: 255,
            },
            gas_used: clamp_gas_to_limit(
                estimate_script_gas(&self.schedule, bytecode, type_args, args),
                gas_limit,
            ),
            state_changes: vec![],
            write_set: HashMap::new(),
        })
    }
}

// ── View query (public, not via Block-STM) ──────────────────────────────

impl MoveRuntime {
    /// Execute a read-only view function and return BCS-encoded return values
    /// together with an estimated gas cost.
    ///
    /// Unlike `execute_function`, this does NOT prepend a signer argument
    /// and does NOT produce state changes.  It is intended for off-chain
    /// queries against the current committed state.
    ///
    /// The returned gas value is an **estimate** computed from the gas
    /// schedule (I/O cost model) rather than instruction-level metering.
    pub fn query_view(
        &self,
        state: &NexusStateView<'_>,
        contract: AccountAddress,
        function: &str,
        type_args: &[Vec<u8>],
        args: &[Vec<u8>],
    ) -> ExecutionResult<(Vec<Vec<u8>>, u64)> {
        let (module_name, fn_name) = parse_function_name(function)?;

        let move_addr = nexus_to_move_address(&contract);
        let module_id = ModuleId::new(
            move_addr,
            Identifier::new(module_name).map_err(|_| ExecutionError::BytecodeVerification {
                reason: format!("invalid module name: {module_name}"),
            })?,
        );
        let fn_ident =
            Identifier::new(fn_name).map_err(|_| ExecutionError::BytecodeVerification {
                reason: format!("invalid function name: {fn_name}"),
            })?;

        let ty_args = deserialize_type_args(type_args)?;

        let bytes_storage = NexusBytesStorage::new(state, &self.runtime_env);
        let module_storage = bytes_storage.as_unsync_module_storage();

        let loaded_fn = module_storage
            .load_function(&module_id, &fn_ident, &ty_args)
            .map_err(|e| {
                let msg = e.message().cloned().unwrap_or_default();
                let _sub = e
                    .sub_status()
                    .map(|s| format!(", sub={s}"))
                    .unwrap_or_default();
                tracing::warn!(
                    %module_id,
                    fn_name = %fn_ident,
                    status = ?e.major_status(),
                    ?msg,
                    "query_view: load_function failed"
                );
                ExecutionError::MoveAbort {
                    location: format!("{module_id}"),
                    code: e.major_status() as u64,
                }
            })?;

        let mut data_cache = TransactionDataCache::empty();
        let mut gas_meter = UnmeteredGasMeter;
        let traversal_storage = TraversalStorage::new();
        let mut traversal_ctx = TraversalContext::new(&traversal_storage);
        let mut extensions = NativeContextExtensions::default();

        // View functions receive their args directly — no signer prepended.
        let full_args: Vec<Vec<u8>> = args.to_vec();

        let result = MoveVM::execute_loaded_function(
            loaded_fn,
            full_args,
            &mut data_cache,
            &mut gas_meter,
            &mut traversal_ctx,
            &mut extensions,
            &module_storage,
            &bytes_storage,
        );

        match result {
            Ok(serialized) => {
                // Extract raw BCS bytes from each (bytes, layout) pair.
                let return_values: Vec<Vec<u8>> = serialized
                    .return_values
                    .into_iter()
                    .map(|(bytes, _layout)| bytes)
                    .collect();
                let output_bytes: u64 = return_values.iter().map(|b| b.len() as u64).sum();
                let gas_used = super::gas_meter::estimate_query_gas(
                    &self.schedule,
                    type_args,
                    args,
                    output_bytes,
                );
                Ok((return_values, gas_used))
            }
            Err(vm_err) => {
                let msg = vm_err.message().cloned().unwrap_or_default();
                tracing::warn!(
                    location = %vm_err.location(),
                    status = ?vm_err.major_status(),
                    ?msg,
                    "query_view: execution failed"
                );
                Err(ExecutionError::MoveAbort {
                    location: format!("{}", vm_err.location()),
                    code: vm_err.major_status() as u64,
                })
            }
        }
    }
}

// ── NexusBytesStorage ───────────────────────────────────────────────────

/// Bridges Nexus state view to the upstream Move VM's module/resource storage.
///
/// Implements:
/// - [`ModuleBytesStorage`] — fetches raw `.mv` module bytes from state
/// - [`WithRuntimeEnvironment`] — provides VM config and native registry
/// - [`ResourceResolver`] — resolves resources from state by struct tag
struct NexusBytesStorage<'a> {
    state: &'a NexusStateView<'a>,
    runtime_env: &'a RuntimeEnvironment,
}

impl<'a> NexusBytesStorage<'a> {
    fn new(state: &'a NexusStateView<'a>, runtime_env: &'a RuntimeEnvironment) -> Self {
        Self { state, runtime_env }
    }
}

impl WithRuntimeEnvironment for NexusBytesStorage<'_> {
    fn runtime_environment(&self) -> &RuntimeEnvironment {
        self.runtime_env
    }
}

impl ModuleBytesStorage for NexusBytesStorage<'_> {
    fn fetch_module_bytes(
        &self,
        address: &MoveAddress,
        module_name: &IdentStr,
    ) -> VMResult<Option<Bytes>> {
        // Serve embedded framework modules (0x1) first — they are not
        // deployed on-chain but are required by the Move linker.
        if *address == super::stdlib::framework_address() {
            if let Some(bytes) = super::stdlib::get_framework_module(module_name.as_str()) {
                return Ok(Some(Bytes::from(bytes)));
            }
        }

        // Fall through to on-chain storage for user modules.
        let nexus_addr = move_to_nexus_address(address);
        match self.state.get_module(&nexus_addr) {
            Ok(Some(bytes)) => Ok(Some(Bytes::from(bytes))),
            Ok(None) => Ok(None),
            Err(e) => Err(PartialVMError::new(StatusCode::STORAGE_ERROR)
                .with_message(format!("state read failed: {e}"))
                .finish(Location::Undefined)),
        }
    }
}

impl ResourceResolver for NexusBytesStorage<'_> {
    fn get_resource_bytes_with_metadata_and_layout(
        &self,
        address: &MoveAddress,
        struct_tag: &StructTag,
        _metadata: &[Metadata],
        _layout: Option<&MoveTypeLayout>,
    ) -> PartialVMResult<(Option<Bytes>, usize)> {
        // Store resources under BCS-encoded struct tag as key.
        let key = bcs::to_bytes(struct_tag).map_err(|_| {
            PartialVMError::new(StatusCode::INTERNAL_TYPE_ERROR)
                .with_message("failed to serialize struct tag".into())
        })?;
        let nexus_addr = move_to_nexus_address(address);
        match self.state.get_resource(&nexus_addr, &key) {
            Ok(Some(bytes)) => {
                let size = bytes.len();
                Ok((Some(Bytes::from(bytes)), size))
            }
            Ok(None) => Ok((None, 0)),
            Err(e) => Err(PartialVMError::new(StatusCode::STORAGE_ERROR)
                .with_message(format!("resource read failed: {e}"))),
        }
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────

/// Convert a Nexus `AccountAddress` to a Move `AccountAddress`.
fn nexus_to_move_address(addr: &AccountAddress) -> MoveAddress {
    MoveAddress::new(addr.0)
}

/// Convert a Move `AccountAddress` back to a Nexus `AccountAddress`.
fn move_to_nexus_address(addr: &MoveAddress) -> AccountAddress {
    AccountAddress(addr.into_bytes())
}

fn dependency_must_exist(dep_address: &MoveAddress, move_sender: &MoveAddress) -> bool {
    let mut framework_bytes = [0u8; 32];
    framework_bytes[31] = 1;
    let framework_address = MoveAddress::new(framework_bytes);

    dep_address != move_sender && dep_address != &framework_address
}

/// Parse `"module_name::function_name"` into its two parts.
fn parse_function_name(function: &str) -> ExecutionResult<(&str, &str)> {
    let parts: Vec<&str> = function.splitn(2, "::").collect();
    if parts.len() != 2 {
        return Err(ExecutionError::BytecodeVerification {
            reason: format!(
                "invalid function format '{}', expected 'module::function'",
                function
            ),
        });
    }
    Ok((parts[0], parts[1]))
}

/// Deserialize BCS-encoded TypeTag arguments.
fn deserialize_type_args(type_args: &[Vec<u8>]) -> ExecutionResult<Vec<TypeTag>> {
    type_args
        .iter()
        .map(|bytes| {
            bcs::from_bytes(bytes).map_err(|e| ExecutionError::BytecodeVerification {
                reason: format!("type arg deserialization failed: {e}"),
            })
        })
        .collect()
}

/// Convert a Move `ChangeSet` into Nexus state changes and write set.
#[allow(clippy::type_complexity)]
fn changeset_to_nexus(
    change_set: nexus_move_runtime::upstream::move_core_types::effects::ChangeSet,
) -> (
    Vec<crate::types::StateChange>,
    HashMap<(AccountAddress, Vec<u8>), Option<Vec<u8>>>,
) {
    let mut state_changes = Vec::new();
    let mut write_set = HashMap::new();

    for (addr, account_changes) in change_set.into_inner() {
        let nexus_addr = move_to_nexus_address(&addr);
        for (struct_tag, op) in account_changes.into_resources() {
            let key = bcs::to_bytes(&struct_tag).unwrap_or_default();
            match op {
                nexus_move_runtime::upstream::move_core_types::effects::Op::New(bytes)
                | nexus_move_runtime::upstream::move_core_types::effects::Op::Modify(bytes) => {
                    state_changes.push(crate::types::StateChange {
                        account: nexus_addr,
                        key: key.clone(),
                        value: Some(bytes.to_vec()),
                    });
                    write_set.insert((nexus_addr, key), Some(bytes.to_vec()));
                }
                nexus_move_runtime::upstream::move_core_types::effects::Op::Delete => {
                    state_changes.push(crate::types::StateChange {
                        account: nexus_addr,
                        key: key.clone(),
                        value: None,
                    });
                    write_set.insert((nexus_addr, key), None);
                }
            }
        }
    }

    (state_changes, write_set)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_function_name_valid() {
        let (module, func) = parse_function_name("counter::increment").unwrap();
        assert_eq!(module, "counter");
        assert_eq!(func, "increment");
    }

    #[test]
    fn test_parse_function_name_invalid() {
        assert!(parse_function_name("nocolon").is_err());
    }

    #[test]
    fn test_address_roundtrip() {
        let nexus_addr = AccountAddress([42u8; 32]);
        let move_addr = nexus_to_move_address(&nexus_addr);
        let back = move_to_nexus_address(&move_addr);
        assert_eq!(nexus_addr, back);
    }

    #[test]
    fn test_dependency_policy_skips_sender_and_framework() {
        let sender = MoveAddress::new([0x22; 32]);
        let mut framework_bytes = [0u8; 32];
        framework_bytes[31] = 1;
        let framework = MoveAddress::new(framework_bytes);
        let external = MoveAddress::new([0x33; 32]);

        assert!(!dependency_must_exist(&sender, &sender));
        assert!(!dependency_must_exist(&framework, &sender));
        assert!(dependency_must_exist(&external, &sender));
    }

    /// Test that the real Move VM can deploy and execute a counter module
    /// with signer-based entry functions and view queries.
    #[test]
    fn test_real_vm_counter_lifecycle() {
        use crate::traits::StateView;
        use std::collections::HashMap;
        use std::sync::RwLock;

        // Simple in-memory state view for testing.
        #[allow(clippy::type_complexity)]
        struct TestState {
            data: RwLock<HashMap<(AccountAddress, Vec<u8>), Vec<u8>>>,
        }
        impl TestState {
            fn new() -> Self {
                Self {
                    data: RwLock::new(HashMap::new()),
                }
            }
            fn set(&self, account: AccountAddress, key: &[u8], value: Vec<u8>) {
                self.data
                    .write()
                    .unwrap()
                    .insert((account, key.to_vec()), value);
            }
        }
        impl StateView for TestState {
            fn get(
                &self,
                account: &AccountAddress,
                key: &[u8],
            ) -> ExecutionResult<Option<Vec<u8>>> {
                Ok(self
                    .data
                    .read()
                    .unwrap()
                    .get(&(*account, key.to_vec()))
                    .cloned())
            }
        }

        // Load the compiled counter module.
        let counter_bytes = include_bytes!(
            "../../../../contracts/examples/counter/nexus-artifact/bytecode/counter.mv"
        );

        let deployer = {
            // Must match the dev-address in contracts/examples/counter/Move.toml
            // which is compiled into the bytecode as the module address.
            let mut bytes = [0u8; 32];
            bytes[30] = 0xCA;
            bytes[31] = 0xFE;
            AccountAddress(bytes)
        };
        let state = TestState::new();

        let vm = MoveRuntime::new(&VmConfig::default());

        // Step 1: Publish the counter module.
        let nexus_view = NexusStateView::new(&state);
        let pub_result =
            vm.publish_modules(&nexus_view, deployer, &[counter_bytes.to_vec()], 100_000);
        let pub_output = pub_result.expect("publish should succeed");
        assert!(
            matches!(pub_output.status, ExecutionStatus::Success),
            "publish status: {:?}",
            pub_output.status
        );

        // Apply state changes from publish.
        for change in &pub_output.state_changes {
            if let Some(ref value) = change.value {
                state.set(change.account, &change.key, value.clone());
            }
        }

        // Step 2: Execute initialize(signer).
        let nexus_view = NexusStateView::new(&state);
        let init_result = vm.execute_function(
            &nexus_view,
            deployer,
            deployer,
            "counter::initialize",
            &[],
            &[],
            100_000,
        );
        let init_output = init_result.expect("initialize should not hard-fail");

        // Apply state changes.
        for change in &init_output.state_changes {
            if let Some(ref value) = change.value {
                state.set(change.account, &change.key, value.clone());
            }
        }

        assert!(
            matches!(init_output.status, ExecutionStatus::Success),
            "initialize status: {:?}",
            init_output.status
        );

        // Step 3: Query get_count (view function).
        let nexus_view = NexusStateView::new(&state);
        let query_result = vm.query_view(
            &nexus_view,
            deployer,
            "counter::get_count",
            &[],
            &[bcs::to_bytes(&nexus_to_move_address(&deployer)).unwrap()],
        );
        let (return_values, _gas) = query_result.expect("query should succeed");
        assert_eq!(return_values.len(), 1);
        // Expected: u64 = 0 (just initialized)
        let count = u64::from_le_bytes(return_values[0].clone().try_into().unwrap());
        assert_eq!(count, 0);
    }
}
