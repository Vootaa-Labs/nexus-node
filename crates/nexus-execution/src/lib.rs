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
