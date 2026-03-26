//! State view bridge for the Move adapter.
//!
//! [`NexusStateView`] wraps our [`StateView`](crate::traits::StateView) to
//! provide module and resource resolution for the Move VM.  When a real
//! `move-vm-runtime` integration is added, `NexusStateView` will implement
//! the `MoveResolver` trait from `move-core-types`.
//!
//! # Data Layout
//!
//! State is stored as raw key-value pairs under `(AccountAddress, key)`.
//! The Move adapter uses the following key conventions:
//!
//! - `b"balance"` — native token balance (u64 LE)
//! - `b"code"` — published module bytecode
//! - `b"code_hash"` — BLAKE3 hash of the published bytecode
//! - Resource tags will use BCS-encoded `StructTag` as keys (future)

use crate::error::ExecutionResult;
use crate::traits::StateView;
use nexus_primitives::AccountAddress;

// Re-export key constants from publisher for backward compatibility.
#[allow(unused_imports)]
pub(crate) use super::publisher::MODULE_CODE_HASH_KEY;
pub(crate) use super::publisher::MODULE_CODE_KEY;

/// Storage key for an account's native token balance.
#[allow(dead_code)]
pub(crate) const BALANCE_KEY: &[u8] = b"balance";

// ── NexusStateView ──────────────────────────────────────────────────────

/// Read-only state view that bridges our [`StateView`] for Move VM access.
///
/// Provides typed helpers for common state reads (module code, balances,
/// resources) while delegating raw storage access to the underlying
/// `StateView`.
///
/// # Thread Safety
///
/// `NexusStateView` is `Send + Sync` when the underlying `StateView` is.
pub(crate) struct NexusStateView<'a> {
    /// The underlying state snapshot.
    state: &'a dyn StateView,
}

impl std::fmt::Debug for NexusStateView<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NexusStateView").finish_non_exhaustive()
    }
}

#[allow(dead_code)]
impl<'a> NexusStateView<'a> {
    /// Create a new state view backed by the given state snapshot.
    pub fn new(state: &'a dyn StateView) -> Self {
        Self { state }
    }

    /// Read raw bytes for a given account and key.
    pub fn get_raw(
        &self,
        account: &AccountAddress,
        key: &[u8],
    ) -> ExecutionResult<Option<Vec<u8>>> {
        self.state.get(account, key)
    }

    /// Read a module's published bytecode.
    pub fn get_module(&self, address: &AccountAddress) -> ExecutionResult<Option<Vec<u8>>> {
        self.state.get(address, MODULE_CODE_KEY)
    }

    /// Check whether a module exists at the given address.
    pub fn has_module(&self, address: &AccountAddress) -> ExecutionResult<bool> {
        self.state.contains(address, MODULE_CODE_KEY)
    }

    /// Read an account's native token balance (u64 LE, default 0).
    pub fn get_balance(&self, account: &AccountAddress) -> ExecutionResult<u64> {
        let raw = self.state.get(account, BALANCE_KEY)?;
        Ok(parse_balance_bytes(&raw))
    }

    /// Read a resource by its storage key.
    pub fn get_resource(
        &self,
        account: &AccountAddress,
        resource_key: &[u8],
    ) -> ExecutionResult<Option<Vec<u8>>> {
        self.state.get(account, resource_key)
    }
}

/// Parse a balance from raw bytes (little-endian u64), defaulting to 0.
#[allow(dead_code)]
pub(crate) fn parse_balance_bytes(raw: &Option<Vec<u8>>) -> u64 {
    raw.as_ref()
        .and_then(|b| b.as_slice().try_into().ok())
        .map(u64::from_le_bytes)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ExecutionResult;
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

    #[test]
    fn get_balance_default_zero() {
        let state = MemState::new();
        let view = NexusStateView::new(&state);
        assert_eq!(view.get_balance(&addr(0xAA)).unwrap(), 0);
    }

    #[test]
    fn get_balance_reads_le_u64() {
        let mut state = MemState::new();
        state.set(addr(0xAA), BALANCE_KEY, 42u64.to_le_bytes().to_vec());
        let view = NexusStateView::new(&state);
        assert_eq!(view.get_balance(&addr(0xAA)).unwrap(), 42);
    }

    #[test]
    fn get_module_returns_bytecode() {
        let mut state = MemState::new();
        state.set(addr(0xBB), MODULE_CODE_KEY, vec![0xDE, 0xAD]);
        let view = NexusStateView::new(&state);
        assert_eq!(
            view.get_module(&addr(0xBB)).unwrap(),
            Some(vec![0xDE, 0xAD])
        );
    }

    #[test]
    fn has_module_false_when_absent() {
        let state = MemState::new();
        let view = NexusStateView::new(&state);
        assert!(!view.has_module(&addr(0xCC)).unwrap());
    }

    #[test]
    fn has_module_true_when_present() {
        let mut state = MemState::new();
        state.set(addr(0xCC), MODULE_CODE_KEY, vec![0x01]);
        let view = NexusStateView::new(&state);
        assert!(view.has_module(&addr(0xCC)).unwrap());
    }

    #[test]
    fn get_raw_passthrough() {
        let mut state = MemState::new();
        state.set(addr(0xDD), b"custom_key", vec![1, 2, 3]);
        let view = NexusStateView::new(&state);
        assert_eq!(
            view.get_raw(&addr(0xDD), b"custom_key").unwrap(),
            Some(vec![1, 2, 3])
        );
    }

    #[test]
    fn get_resource_returns_none() {
        let state = MemState::new();
        let view = NexusStateView::new(&state);
        assert_eq!(
            view.get_resource(&addr(0xEE), b"SomeResource").unwrap(),
            None
        );
    }

    #[test]
    fn parse_balance_bytes_malformed() {
        // Short bytes → 0 (not panic).
        assert_eq!(parse_balance_bytes(&Some(vec![1, 2])), 0);
        // Empty → 0.
        assert_eq!(parse_balance_bytes(&Some(vec![])), 0);
        // None → 0.
        assert_eq!(parse_balance_bytes(&None), 0);
    }
}
