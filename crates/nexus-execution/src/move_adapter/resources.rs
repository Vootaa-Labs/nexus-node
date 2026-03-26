//! Resource read/write bridge for Move contract state.
//!
//! TLD-09 §8: each resource type is stored under a structured key at the
//! contract address. This module provides typed helpers that keep
//! Move resource layout details inside the execution boundary.

use crate::error::ExecutionResult;
use crate::types::StateChange;
use nexus_primitives::AccountAddress;
use std::collections::HashMap;

use super::state_view::NexusStateView;

/// Type alias for the write-set produced by [`ResourceStore::into_changes`].
pub(crate) type WriteSet = HashMap<(AccountAddress, Vec<u8>), Option<Vec<u8>>>;

// ── Resource key derivation ─────────────────────────────────────────────

/// Derive a storage key for a typed resource.
///
/// Format: `"resource::" || type_tag_bytes`
///
/// The type tag is a human-readable identifier (e.g. `"counter::Counter"`)
/// encoded to UTF-8. This ensures different resource types never collide.
pub(crate) fn resource_key(type_tag: &str) -> Vec<u8> {
    let mut key = b"resource::".to_vec();
    key.extend_from_slice(type_tag.as_bytes());
    key
}

// ── ResourceStore ───────────────────────────────────────────────────────

/// In-session resource overlay that collects writes for inclusion in the
/// final [`VmOutput`](super::VmOutput) write-set.
///
/// Reads fall through to the underlying [`NexusStateView`] when the
/// overlay has no entry.
#[derive(Debug)]
pub(crate) struct ResourceStore<'a> {
    /// Underlying state snapshot.
    view: &'a NexusStateView<'a>,
    /// Pending writes: `(account, key) → Some(value)` or `None` (delete).
    overlay: HashMap<(AccountAddress, Vec<u8>), Option<Vec<u8>>>,
}

impl<'a> ResourceStore<'a> {
    /// Create a new store backed by the given state view.
    pub fn new(view: &'a NexusStateView<'a>) -> Self {
        Self {
            view,
            overlay: HashMap::new(),
        }
    }

    /// Read a resource, checking the overlay first.
    pub fn get(
        &self,
        account: &AccountAddress,
        type_tag: &str,
    ) -> ExecutionResult<Option<Vec<u8>>> {
        let key = resource_key(type_tag);
        if let Some(entry) = self.overlay.get(&(*account, key.clone())) {
            return Ok(entry.clone());
        }
        self.view.get_resource(account, &key)
    }

    /// Write a resource value (pending until commit).
    pub fn set(&mut self, account: AccountAddress, type_tag: &str, value: Vec<u8>) {
        let key = resource_key(type_tag);
        self.overlay.insert((account, key), Some(value));
    }

    /// Delete a resource (pending until commit).
    #[allow(dead_code)]
    pub fn remove(&mut self, account: AccountAddress, type_tag: &str) {
        let key = resource_key(type_tag);
        self.overlay.insert((account, key), None);
    }

    /// Consume this store and produce the final write-set and state changes.
    pub fn into_changes(self) -> (WriteSet, Vec<StateChange>) {
        let mut state_changes = Vec::with_capacity(self.overlay.len());
        for ((account, key), value) in &self.overlay {
            state_changes.push(StateChange {
                account: *account,
                key: key.clone(),
                value: value.clone(),
            });
        }
        (self.overlay, state_changes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::StateView;
    use std::collections::HashMap as StdMap;

    struct MemState {
        data: StdMap<(AccountAddress, Vec<u8>), Vec<u8>>,
    }
    impl MemState {
        fn new() -> Self {
            Self {
                data: StdMap::new(),
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
    fn overlay_shadows_underlying() {
        let mut backing = MemState::new();
        let key = resource_key("counter::Counter");
        backing.set(addr(1), &key, vec![10, 0, 0, 0, 0, 0, 0, 0]);

        let view = NexusStateView::new(&backing);
        let mut store = ResourceStore::new(&view);

        // Before overlay write, reads from backing.
        let val = store.get(&addr(1), "counter::Counter").unwrap();
        assert_eq!(val, Some(vec![10, 0, 0, 0, 0, 0, 0, 0]));

        // After overlay write, reads from overlay.
        store.set(addr(1), "counter::Counter", vec![20, 0, 0, 0, 0, 0, 0, 0]);
        let val = store.get(&addr(1), "counter::Counter").unwrap();
        assert_eq!(val, Some(vec![20, 0, 0, 0, 0, 0, 0, 0]));
    }

    #[test]
    fn remove_resource_produces_none() {
        let backing = MemState::new();
        let view = NexusStateView::new(&backing);
        let mut store = ResourceStore::new(&view);

        store.set(addr(1), "token::Balance", vec![0xFF]);
        store.remove(addr(1), "token::Balance");

        let val = store.get(&addr(1), "token::Balance").unwrap();
        assert_eq!(val, None);
    }

    #[test]
    fn into_changes_produces_write_set() {
        let backing = MemState::new();
        let view = NexusStateView::new(&backing);
        let mut store = ResourceStore::new(&view);

        store.set(addr(1), "counter::Counter", vec![42]);
        store.set(addr(2), "token::Balance", vec![100]);

        let (write_set, state_changes) = store.into_changes();
        assert_eq!(write_set.len(), 2);
        assert_eq!(state_changes.len(), 2);
    }
}
