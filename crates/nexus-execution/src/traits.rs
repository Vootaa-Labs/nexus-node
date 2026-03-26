//! Execution trait contracts.
//!
//! These traits define the boundaries of the execution layer.
//! All implementations must satisfy the contract tests at the bottom
//! of this module.
//!
//! # Stability
//!
//! | Trait | Level |
//! |---|---|
//! | [`TransactionExecutor`] | **SEALED** — changes require architecture review |
//! | [`StateView`] | **SEALED** — changes require architecture review |

use crate::error::ExecutionResult;
use crate::types::{BlockExecutionResult, SignedTransaction};
use nexus_primitives::{AccountAddress, Blake3Digest};

// ── TransactionExecutor ─────────────────────────────────────────────────

/// **\[SEALED\]** Core execution trait — takes an ordered batch of
/// transactions and a pre-state root, returns the execution result
/// including a new state root and per-tx receipts.
///
/// Implementors:
/// - `BlockStmExecutor` (Block-STM parallel, T-1008)
/// - Future: sharded executor, ZK prover executor
///
/// # Contract
///
/// 1. Execution is **deterministic**: same `(transactions, state_root)` →
///    identical `BlockExecutionResult`.
/// 2. Receipts are returned in the **same order** as input transactions.
/// 3. A failing transaction produces a receipt with a non-Success status
///    but does **not** abort the entire block.
/// 4. `new_state_root` reflects all successful and failed-but-gas-charged
///    transactions.
pub trait TransactionExecutor: Send + Sync + 'static {
    /// Execute an ordered block of transactions against the given state root.
    ///
    /// Returns `Ok(BlockExecutionResult)` on success, or an
    /// [`ExecutionError`](crate::error::ExecutionError) if the batch
    /// cannot be processed at all (e.g., storage is unavailable).
    fn execute_block(
        &self,
        transactions: &[SignedTransaction],
        state_root: Blake3Digest,
    ) -> ExecutionResult<BlockExecutionResult>;
}

// ── StateView ───────────────────────────────────────────────────────────

/// **\[SEALED\]** Read-only view into account state.
///
/// Block-STM's optimistic execution reads state through this trait.
/// Implementations back onto `nexus-storage` or an in-memory overlay.
///
/// # Contract
///
/// 1. `get` returns `Ok(None)` for keys that were never written.
/// 2. Reads are consistent within a single call (snapshot isolation).
/// 3. All implementations are `Send + Sync` for parallel access.
pub trait StateView: Send + Sync {
    /// Read a value from the state at a given account + key.
    ///
    /// Returns `Ok(None)` when the key does not exist.
    fn get(&self, account: &AccountAddress, key: &[u8]) -> ExecutionResult<Option<Vec<u8>>>;

    /// Check whether a key exists without reading its full value.
    ///
    /// Default implementation delegates to [`get`](Self::get).
    fn contains(&self, account: &AccountAddress, key: &[u8]) -> ExecutionResult<bool> {
        self.get(account, key).map(|v| v.is_some())
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify TransactionExecutor is object-safe (can be used as dyn).
    #[test]
    fn executor_is_object_safe() {
        fn _accepts_dyn(_e: &dyn TransactionExecutor) {}
    }

    /// Verify StateView is object-safe.
    #[test]
    fn state_view_is_object_safe() {
        fn _accepts_dyn(_s: &dyn StateView) {}
    }

    /// Verify trait bounds include Send + Sync.
    #[test]
    fn traits_are_send_sync() {
        fn assert_send_sync<T: Send + Sync + ?Sized>() {}
        assert_send_sync::<dyn TransactionExecutor>();
        assert_send_sync::<dyn StateView>();
    }
}
