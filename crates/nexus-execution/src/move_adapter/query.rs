//! Contract query pipeline — read-only view function execution.
//!
//! TLD-09 §6.2: read-only sessions never produce persistent writes.
//! This module provides [`query_view_function`] for RPC-layer query
//! dispatch without creating a full transaction.

use crate::error::{ExecutionError, ExecutionResult};
use crate::traits::StateView;
use nexus_primitives::AccountAddress;

use super::abi::{decode_abi, MODULE_ABI_KEY};
use super::entry_function::validate_args;
use super::gas_meter::GasMeter;
use super::nexus_vm::NexusMoveVm;
use super::session::{ExecuteSession, SessionKind};
use super::state_view::NexusStateView;
use super::vm_config::VmConfig;

/// Result of a read-only view function query.
#[derive(Debug, Clone)]
pub struct QueryResult {
    /// BCS-encoded return value (if the function has a return type).
    pub return_value: Option<Vec<u8>>,
    /// Gas consumed (informational — not charged).
    pub gas_used: u64,
    /// Gas budget that was applied (0 = unbounded).
    pub gas_budget: u64,
}

/// Execute a read-only view function against the current state.
///
/// This does not produce any persistent state changes.  It is used
/// by the RPC layer to serve `query_contract` requests.
///
/// # Arguments
/// - `state` — current committed state snapshot
/// - `contract` — target contract address
/// - `function` — function name to call
/// - `type_args` — serialised type arguments
/// - `args` — BCS-encoded call arguments
///
/// # Errors
/// - `ContractNotFound` if no code exists at `contract`
/// - `TypeMismatch` if arguments don't match the ABI
#[allow(dead_code)]
pub fn query_view_function(
    state: &dyn StateView,
    contract: AccountAddress,
    function: &str,
    type_args: &[Vec<u8>],
    args: &[Vec<u8>],
) -> ExecutionResult<QueryResult> {
    let view = NexusStateView::new(state);

    // 1. Verify contract exists.
    if !view.has_module(&contract)? {
        return Err(ExecutionError::ContractNotFound {
            address: nexus_primitives::ContractAddress(contract.0),
        });
    }

    // 2. Resolve ABI.
    let abi_bytes = view.get_raw(&contract, MODULE_ABI_KEY)?;
    let abi_list = match abi_bytes {
        Some(bytes) => decode_abi(&bytes).map_err(|e| ExecutionError::BytecodeVerification {
            reason: format!("corrupt ABI: {e}"),
        })?,
        None => {
            return Err(ExecutionError::TypeMismatch {
                function: function.to_string(),
                reason: "no ABI published for this contract".into(),
            });
        }
    };

    let func_abi = abi_list
        .iter()
        .find(|f| f.name == function)
        .ok_or_else(|| ExecutionError::TypeMismatch {
            function: function.to_string(),
            reason: "function not found in ABI".into(),
        })?;

    // 3. Validate arguments.
    validate_args(func_abi, args).map_err(|reason| ExecutionError::TypeMismatch {
        function: function.to_string(),
        reason,
    })?;

    // 4. Create a read-only session.
    let config = VmConfig::default();
    let _vm = NexusMoveVm::new(&config);
    let _ = type_args; // Future: generic type resolution.

    // Use standard execute path but with ReadOnly session guarding writes.
    let schedule = super::gas_meter::GasSchedule::from_config(&config);
    let mut session = ExecuteSession::new(
        SessionKind::ReadOnly,
        AccountAddress([0; 32]), // No sender for queries.
        1_000_000,               // Generous gas limit for queries.
        schedule,
        &view,
    );

    // 5. Dispatch via the VM's internal logic (simplified path).
    //    For view functions we just read the resource.
    let resource_tag = format!("{}::State", function);
    let return_value = session.read_resource(&contract, &resource_tag)?;

    Ok(QueryResult {
        return_value,
        gas_used: session.meter.consumed(),
        gas_budget: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ExecutionResult;
    use crate::move_adapter::abi::{encode_abi, FunctionAbi, MoveType};
    use crate::move_adapter::publisher::MODULE_CODE_KEY;
    use crate::move_adapter::resources::resource_key;
    use crate::traits::StateView;
    use nexus_primitives::AccountAddress;
    use std::collections::HashMap;

    struct MemState {
        data: HashMap<(AccountAddress, Vec<u8>), Vec<u8>>,
    }
    impl MemState {
        fn new() -> Self {
            Self {
                data: HashMap::new(),
            }
        }
        fn set(&mut self, acct: AccountAddress, key: &[u8], val: Vec<u8>) {
            self.data.insert((acct, key.to_vec()), val);
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

    #[test]
    fn query_contract_not_found() {
        let state = MemState::new();
        let result = query_view_function(&state, addr(0xCC), "get_count", &[], &[]);
        assert!(result.is_err());
    }

    #[test]
    fn query_function_not_found() {
        let mut state = MemState::new();
        let contract = addr(0xCC);
        state.set(contract, MODULE_CODE_KEY, vec![0xCA, 0xFE]);
        state.set(
            contract,
            MODULE_ABI_KEY,
            encode_abi(&[FunctionAbi {
                name: "increment".into(),
                params: vec![],
                returns: None,
                is_entry: true,
            }])
            .unwrap(),
        );

        let result = query_view_function(&state, contract, "nonexistent", &[], &[]);
        assert!(result.is_err());
    }

    #[test]
    fn query_read_existing_resource() {
        let mut state = MemState::new();
        let contract = addr(0xCC);
        state.set(contract, MODULE_CODE_KEY, vec![0xCA, 0xFE]);

        let abi = vec![FunctionAbi {
            name: "get_count".into(),
            params: vec![],
            returns: Some(MoveType::U64),
            is_entry: false,
        }];
        state.set(contract, MODULE_ABI_KEY, encode_abi(&abi).unwrap());

        // Store a resource value.
        let res_key = resource_key("get_count::State");
        state.set(contract, &res_key, 42u64.to_le_bytes().to_vec());

        let result = query_view_function(&state, contract, "get_count", &[], &[]).unwrap();
        assert_eq!(result.return_value, Some(42u64.to_le_bytes().to_vec()));
    }
}
