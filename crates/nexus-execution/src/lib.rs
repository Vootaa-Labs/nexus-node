// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! `nexus-execution` — Move VM execution engine for Nexus.
//!
//! Runs Move bytecode with Block-STM parallel execution and gas metering.
//! Maintains a sharded execution environment where shard boundaries are
//! transparent to contract developers.
//!
//! # Modules
//! - [`error`]        — `ExecutionError` enum and `ExecutionResult` alias
//! - [`types`]        — Transaction, receipt, and execution result structures
//! - [`traits`]       — `TransactionExecutor`, `StateView` trait contracts
//! - [`block_stm`]    — `BlockStmExecutor` — Block-STM parallel execution (OCC)
//! - [`metrics`]      — `ExecutionMetrics` — TPS, latency, conflict rate, shard load
//! - [`service`]      — `ExecutionService` async actor for batch processing
//! - [`move_adapter`] — Move VM adapter (isolation boundary for VM internals)

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod block_stm;
pub mod error;
#[cfg(test)]
mod gas_calibration;
pub mod metrics;
pub(crate) mod move_adapter;
pub mod service;
pub mod traits;
pub mod types;

// Re-exports for ergonomic top-level access.
pub use block_stm::AnchorStateEntry;
pub use block_stm::BlockStmExecutor;
pub use error::{ExecutionError, ExecutionResult};
pub use metrics::ExecutionMetrics;
pub use move_adapter::events::ContractEvent;
pub use move_adapter::query::QueryResult;
pub use service::{spawn_execution_service, ExecutionServiceHandle};
pub use traits::{StateView, TransactionExecutor};
pub use types::{compute_lock_hash, HtlcLockRecord, HtlcStatus};

/// Execute a read-only view function against the current state.
///
/// This is the public entry point for RPC-layer contract queries.
/// It creates a temporary VM and executes the function without
/// producing any persistent state changes.
#[cfg(feature = "move-vm")]
pub fn query_view(
    state: &dyn StateView,
    contract: nexus_primitives::AccountAddress,
    function: &str,
    type_args: &[Vec<u8>],
    args: &[Vec<u8>],
) -> ExecutionResult<QueryResult> {
    query_view_with_budget(state, contract, function, type_args, args, 0)
}

/// Execute a read-only view function with a gas budget.
///
/// When `gas_budget > 0`, returns [`ExecutionError::OutOfGas`] if
/// the estimated gas exceeds the budget.  When `gas_budget == 0`, no
/// limit is applied (equivalent to [`query_view`]).
#[cfg(feature = "move-vm")]
pub fn query_view_with_budget(
    state: &dyn StateView,
    contract: nexus_primitives::AccountAddress,
    function: &str,
    type_args: &[Vec<u8>],
    args: &[Vec<u8>],
    gas_budget: u64,
) -> ExecutionResult<QueryResult> {
    let vm = move_adapter::MoveRuntime::new(&move_adapter::VmConfig::default());
    let view = move_adapter::NexusStateView::new(state);
    let (return_values, gas_used) = vm.query_view(&view, contract, function, type_args, args)?;

    if gas_budget > 0 && gas_used > gas_budget {
        return Err(error::ExecutionError::OutOfGas {
            used: gas_used,
            limit: gas_budget,
        });
    }

    let combined = if return_values.is_empty() {
        None
    } else {
        Some(return_values.concat())
    };
    Ok(QueryResult {
        return_value: combined,
        gas_used,
        gas_budget,
    })
}
pub use types::{
    compute_tx_digest, BlockExecutionResult, ExecutionStatus, SignedTransaction, StateChange,
    TransactionBody, TransactionPayload, TransactionReceipt, MAX_TX_PAYLOAD_SIZE, MIN_GAS_LIMIT,
    TX_DOMAIN,
};

#[cfg(all(test, feature = "move-vm"))]
mod tests {
    use super::*;
    use crate::move_adapter::type_bridge::nexus_to_move_address;
    use crate::move_adapter::MoveVm;
    use crate::move_adapter::{MoveRuntime, NexusStateView, VmConfig};
    use std::collections::HashMap;
    use std::sync::RwLock;

    struct TestState {
        data: RwLock<HashMap<(nexus_primitives::AccountAddress, Vec<u8>), Vec<u8>>>,
    }

    impl TestState {
        fn new() -> Self {
            Self {
                data: RwLock::new(HashMap::new()),
            }
        }

        fn set(&self, account: nexus_primitives::AccountAddress, key: &[u8], value: Vec<u8>) {
            self.data
                .write()
                .unwrap()
                .insert((account, key.to_vec()), value);
        }

        fn apply_changes(&self, changes: &[StateChange]) {
            for change in changes {
                if let Some(value) = &change.value {
                    self.set(change.account, &change.key, value.clone());
                }
            }
        }
    }

    impl StateView for TestState {
        fn get(
            &self,
            account: &nexus_primitives::AccountAddress,
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

    fn deployer_address() -> nexus_primitives::AccountAddress {
        let mut bytes = [0u8; 32];
        bytes[30] = 0xCA;
        bytes[31] = 0xFE;
        nexus_primitives::AccountAddress(bytes)
    }

    fn prepare_counter_state() -> (TestState, nexus_primitives::AccountAddress) {
        let state = TestState::new();
        let deployer = deployer_address();
        let vm = MoveRuntime::new(&VmConfig::default());
        let counter_bytes = include_bytes!(
            "../../../contracts/examples/counter/nexus-artifact/bytecode/counter.mv"
        );

        let view = NexusStateView::new(&state);
        let publish_output = vm
            .publish_modules(&view, deployer, &[counter_bytes.to_vec()], 100_000)
            .expect("publish should succeed");
        state.apply_changes(&publish_output.state_changes);

        let view = NexusStateView::new(&state);
        let init_output = vm
            .execute_function(
                &view,
                deployer,
                deployer,
                "counter::initialize",
                &[],
                &[],
                100_000,
            )
            .expect("initialize should succeed");
        state.apply_changes(&init_output.state_changes);

        (state, deployer)
    }

    fn counter_query_args(account: nexus_primitives::AccountAddress) -> Vec<Vec<u8>> {
        vec![bcs::to_bytes(&nexus_to_move_address(&account)).unwrap()]
    }

    #[test]
    fn query_view_returns_value_with_unbounded_budget() {
        let (state, deployer) = prepare_counter_state();
        let args = counter_query_args(deployer);

        let result = query_view(&state, deployer, "counter::get_count", &[], &args).unwrap();

        assert_eq!(result.gas_budget, 0);
        assert!(result.gas_used > 0);
        assert_eq!(result.return_value, Some(0u64.to_le_bytes().to_vec()));
    }

    #[test]
    fn query_view_with_budget_enforces_limit() {
        let (state, deployer) = prepare_counter_state();
        let args = counter_query_args(deployer);
        let baseline = query_view(&state, deployer, "counter::get_count", &[], &args).unwrap();
        assert!(baseline.gas_used > 0);

        let err = query_view_with_budget(
            &state,
            deployer,
            "counter::get_count",
            &[],
            &args,
            baseline.gas_used - 1,
        )
        .unwrap_err();

        match err {
            ExecutionError::OutOfGas { used, limit } => {
                assert_eq!(used, baseline.gas_used);
                assert_eq!(limit, baseline.gas_used - 1);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
