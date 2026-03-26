//! Production-grade Multi-Version HashMap for Block-STM MVCC.
//!
//! [`MvHashMap`] is a thread-safe, concurrent multi-version data structure
//! that stores provisional writes from transactions. It uses [`DashMap`] for
//! lock-free concurrent access during Phase 1 (parallel execution) and
//! supports efficient sequential validation in Phase 2.
//!
//! # Version Management
//!
//! Each state key maps to a [`BTreeMap`] of `tx_index → value` entries.
//! When reading, the highest `tx_index` strictly less than the reader's
//! index is returned. A cap of [`MAX_VERSIONS_PER_KEY`] prevents unbounded
//! memory growth from pathological workloads (e.g., hot-key contention).
//!
//! # Thread Safety
//!
//! `MvHashMap` is `Send + Sync` — it can be shared across rayon threads
//! during Phase 1 without external synchronisation.

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use dashmap::DashMap;

use crate::error::ExecutionResult;
use crate::traits::StateView;
use nexus_primitives::AccountAddress;

/// Error returned when a key's version chain exceeds the configured capacity.
///
/// This replaces the old silent-eviction policy (SEC-M10). The caller
/// should treat this as a fatal batch error — the batch is too large
/// for the configured `max_versions_per_key`.
#[derive(Debug)]
pub(crate) struct VersionCapExceeded {
    pub key: StateKey,
    pub cap: usize,
}

impl fmt::Display for VersionCapExceeded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "version chain for key ({:?}, {:?}) exceeded cap of {}",
            self.key.account, self.key.key, self.cap
        )
    }
}

// ── Constants ───────────────────────────────────────────────────────────

/// Default maximum number of version entries stored per state key.
///
/// Set to the batch size during construction so that no version is silently
/// evicted. Eviction could remove entries needed for Phase 2 validation,
/// breaking read-set consistency checks on hot keys.
#[allow(dead_code)]
pub(crate) const DEFAULT_MAX_VERSIONS_PER_KEY: usize = 256;

// ── State key ───────────────────────────────────────────────────────────

/// A unique key into the state — `(account, resource_key)`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct StateKey {
    pub account: AccountAddress,
    pub key: Vec<u8>,
}

// ── MvHashMap (production, DashMap-backed) ───────────────────────────────

/// Thread-safe multi-version overlay on top of a [`StateView`].
///
/// Uses [`DashMap`] for concurrent access during parallel execution.
/// Each state key maps to a [`BTreeMap<u32, Option<Vec<u8>>>`] for
/// efficient version lookup via range queries.
pub(crate) struct MvHashMap<'a> {
    /// The base state snapshot (immutable, read-only).
    base: &'a dyn StateView,
    /// Per-key version chains: `StateKey → BTreeMap<tx_index, value>`.
    versions: DashMap<StateKey, BTreeMap<u32, Option<Vec<u8>>>>,
    /// Maximum versions per key (set to batch size to avoid silent eviction).
    max_versions: usize,
}

impl<'a> MvHashMap<'a> {
    /// Create a new empty MVCC overlay backed by the given state snapshot.
    #[allow(dead_code)]
    pub fn new(base: &'a dyn StateView) -> Self {
        Self::with_capacity(base, DEFAULT_MAX_VERSIONS_PER_KEY)
    }

    /// Create an overlay with an explicit per-key version cap.
    ///
    /// Set `max_versions` to at least the batch size so that no version
    /// needed for Phase 2 validation is silently dropped.
    pub fn with_capacity(base: &'a dyn StateView, max_versions: usize) -> Self {
        Self {
            base,
            versions: DashMap::new(),
            max_versions: max_versions.max(1),
        }
    }

    /// Apply a transaction's write-set to the overlay.
    ///
    /// Thread-safe: can be called concurrently from multiple rayon threads.
    ///
    /// Returns `Err` if any key's version chain would exceed the configured
    /// capacity. This replaces the previous silent-eviction policy that
    /// could drop entries still needed for Phase 2 validation (SEC-M10).
    pub fn apply_writes(
        &self,
        tx_index: u32,
        write_set: &HashMap<StateKey, Option<Vec<u8>>>,
    ) -> Result<(), VersionCapExceeded> {
        for (key, value) in write_set {
            let mut entry = self.versions.entry(key.clone()).or_default();
            let versions = entry.value_mut();
            if versions.len() >= self.max_versions && !versions.contains_key(&tx_index) {
                return Err(VersionCapExceeded {
                    key: key.clone(),
                    cap: self.max_versions,
                });
            }
            versions.insert(tx_index, value.clone());
        }
        Ok(())
    }

    /// Remove all version entries written by the given transaction.
    ///
    /// Called before re-executing a transaction so stale provisional
    /// writes don't persist in the overlay.
    #[allow(dead_code)]
    pub fn remove_versions(&self, tx_index: u32) {
        self.versions.iter_mut().for_each(|mut entry| {
            entry.value_mut().remove(&tx_index);
        });
    }

    /// Read a key as seen by transaction `reader_index`.
    ///
    /// Returns the value from the highest `tx_index` strictly less than
    /// `reader_index`, or falls through to the base [`StateView`].
    ///
    /// Thread-safe: can be called concurrently from multiple rayon threads.
    pub fn read(&self, reader_index: u32, key: &StateKey) -> ExecutionResult<Option<Vec<u8>>> {
        if let Some(entry) = self.versions.get(key) {
            // BTreeMap range(..reader_index): all versions with tx_index < reader_index.
            // next_back() gives the highest such index.
            if let Some((_idx, value)) = entry.range(..reader_index).next_back() {
                return Ok(value.clone());
            }
        }
        // Fall through to base state.
        self.base.get(&key.account, &key.key)
    }

    /// Validate whether a read-set entry is still consistent.
    ///
    /// Compares the current overlay value visible to `reader_index`
    /// against the previously observed value. Used in Phase 2 validation.
    pub fn validate_read(
        &self,
        reader_index: u32,
        key: &StateKey,
        observed: &Option<Vec<u8>>,
    ) -> ExecutionResult<bool> {
        let current = self.read(reader_index, key)?;
        Ok(current == *observed)
    }
}

/// Backwards-compatible type alias.
pub(crate) type MvOverlay<'a> = MvHashMap<'a>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ExecutionResult;
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

    fn key(account: AccountAddress, k: &[u8]) -> StateKey {
        StateKey {
            account,
            key: k.to_vec(),
        }
    }

    #[test]
    fn read_falls_through_to_base() {
        let mut base = MemState::new();
        base.set(addr(0xAA), b"balance", vec![42]);
        let mv = MvHashMap::new(&base);

        let k = key(addr(0xAA), b"balance");
        let result = mv.read(0, &k).unwrap();
        assert_eq!(result, Some(vec![42]));
    }

    #[test]
    fn read_missing_key_returns_none() {
        let base = MemState::new();
        let mv = MvHashMap::new(&base);

        let k = key(addr(0xBB), b"nope");
        let result = mv.read(0, &k).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn write_then_read_same_tx_not_visible() {
        let base = MemState::new();
        let mv = MvHashMap::new(&base);

        let k = key(addr(0xAA), b"balance");
        let mut ws = HashMap::new();
        ws.insert(k.clone(), Some(vec![99]));
        mv.apply_writes(5, &ws).unwrap();

        // tx_index=5 reads: range(..5) does NOT include index 5.
        let result = mv.read(5, &k).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn write_visible_to_higher_tx() {
        let base = MemState::new();
        let mv = MvHashMap::new(&base);

        let k = key(addr(0xAA), b"balance");
        let mut ws = HashMap::new();
        ws.insert(k.clone(), Some(vec![99]));
        mv.apply_writes(3, &ws).unwrap();

        // tx 4 should see tx 3's write.
        assert_eq!(mv.read(4, &k).unwrap(), Some(vec![99]));
        // tx 10 should also see it.
        assert_eq!(mv.read(10, &k).unwrap(), Some(vec![99]));
    }

    #[test]
    fn latest_version_wins() {
        let base = MemState::new();
        let mv = MvHashMap::new(&base);

        let k = key(addr(0xAA), b"balance");

        let mut ws1 = HashMap::new();
        ws1.insert(k.clone(), Some(vec![10]));
        mv.apply_writes(1, &ws1).unwrap();

        let mut ws3 = HashMap::new();
        ws3.insert(k.clone(), Some(vec![30]));
        mv.apply_writes(3, &ws3).unwrap();

        // tx 5 sees tx 3's write (highest < 5).
        assert_eq!(mv.read(5, &k).unwrap(), Some(vec![30]));
        // tx 2 sees tx 1's write (highest < 2).
        assert_eq!(mv.read(2, &k).unwrap(), Some(vec![10]));
    }

    #[test]
    fn remove_versions_clears_tx() {
        let base = MemState::new();
        let mv = MvHashMap::new(&base);

        let k = key(addr(0xAA), b"balance");
        let mut ws = HashMap::new();
        ws.insert(k.clone(), Some(vec![10]));
        mv.apply_writes(2, &ws).unwrap();

        assert_eq!(mv.read(3, &k).unwrap(), Some(vec![10]));

        mv.remove_versions(2);
        assert_eq!(mv.read(3, &k).unwrap(), None);
    }

    #[test]
    fn validate_read_detects_stale() {
        let base = MemState::new();
        let mv = MvHashMap::new(&base);

        let k = key(addr(0xAA), b"balance");

        // Initially tx 5 reads None from base.
        let observed: Option<Vec<u8>> = None;
        assert!(mv.validate_read(5, &k, &observed).unwrap());

        // Now tx 2 writes a value.
        let mut ws = HashMap::new();
        ws.insert(k.clone(), Some(vec![42]));
        mv.apply_writes(2, &ws).unwrap();

        // tx 5's observed None is now stale.
        assert!(!mv.validate_read(5, &k, &observed).unwrap());
    }

    #[test]
    fn validate_read_consistent() {
        let base = MemState::new();
        let mv = MvHashMap::new(&base);

        let k = key(addr(0xAA), b"balance");
        let mut ws = HashMap::new();
        ws.insert(k.clone(), Some(vec![42]));
        mv.apply_writes(2, &ws).unwrap();

        let observed = Some(vec![42]);
        assert!(mv.validate_read(5, &k, &observed).unwrap());
    }

    #[test]
    fn max_versions_cap_returns_error() {
        let base = MemState::new();
        // Use a small capacity to trigger the cap.
        let mv = MvHashMap::with_capacity(&base, 4);
        let k = key(addr(0xAA), b"balance");

        // Insert 4 versions (fills the cap).
        for i in 0..4u32 {
            let mut ws = HashMap::new();
            ws.insert(k.clone(), Some(vec![i as u8]));
            mv.apply_writes(i, &ws).unwrap();
        }

        // The 5th distinct tx_index should be rejected.
        let mut ws = HashMap::new();
        ws.insert(k.clone(), Some(vec![99]));
        let err = mv.apply_writes(4, &ws);
        assert!(err.is_err(), "expected VersionCapExceeded");

        // Overwriting an existing tx_index should still succeed.
        let mut ws = HashMap::new();
        ws.insert(k.clone(), Some(vec![100]));
        mv.apply_writes(2, &ws).unwrap();
    }

    #[test]
    fn mvhashmap_hot_key_conflicts_should_not_drop_validation_history() {
        let base = MemState::new();
        let cap = 32;
        let mv = MvHashMap::with_capacity(&base, cap);
        let k = key(addr(0xAA), b"balance");

        // Fill up to the cap with sequential tx writes.
        for i in 0..cap as u32 {
            let mut ws = HashMap::new();
            ws.insert(k.clone(), Some(vec![i as u8]));
            mv.apply_writes(i, &ws).unwrap();
        }

        // All versions must still be present (no silent eviction).
        let entry = mv.versions.get(&k).unwrap();
        assert_eq!(entry.len(), cap);
        for i in 0..cap as u32 {
            assert!(entry.contains_key(&i), "version {i} must not be evicted");
        }

        // Validate reads against each version.
        for i in 1..cap as u32 {
            let expected = Some(vec![(i - 1) as u8]);
            assert!(
                mv.validate_read(i, &k, &expected).unwrap(),
                "validate_read for tx {i} should pass against version {}'s write",
                i - 1
            );
        }
    }

    #[test]
    fn concurrent_apply_writes() {
        let base = MemState::new();
        let mv = MvHashMap::new(&base);

        // Spawn multiple threads writing to the same overlay.
        std::thread::scope(|s| {
            for i in 0..8u32 {
                let mv = &mv;
                s.spawn(move || {
                    let k = StateKey {
                        account: addr(0xAA),
                        key: b"balance".to_vec(),
                    };
                    let mut ws = HashMap::new();
                    ws.insert(k, Some(vec![i as u8]));
                    mv.apply_writes(i, &ws).unwrap();
                });
            }
        });

        // All 8 versions should be present.
        let k = key(addr(0xAA), b"balance");
        let entry = mv.versions.get(&k).unwrap();
        assert_eq!(entry.len(), 8);
    }

    #[test]
    fn concurrent_read_and_write() {
        let mut base = MemState::new();
        base.set(addr(0xAA), b"balance", 1000u64.to_le_bytes().to_vec());
        let mv = MvHashMap::new(&base);

        std::thread::scope(|s| {
            let mv = &mv;

            // Writer thread: tx 0 writes.
            s.spawn(move || {
                let k = StateKey {
                    account: addr(0xAA),
                    key: b"balance".to_vec(),
                };
                let mut ws = HashMap::new();
                ws.insert(k, Some(2000u64.to_le_bytes().to_vec()));
                mv.apply_writes(0, &ws).unwrap();
            });

            // Reader thread: tx 1 reads (should see base or tx 0's write).
            s.spawn(move || {
                let k = StateKey {
                    account: addr(0xAA),
                    key: b"balance".to_vec(),
                };
                let val = mv.read(1, &k).unwrap();
                // Either base (1000) or tx 0's write (2000) — both are valid.
                assert!(val.is_some());
            });
        });
    }

    #[test]
    fn delete_recorded_as_none() {
        let mut base = MemState::new();
        base.set(addr(0xAA), b"data", vec![1, 2, 3]);
        let mv = MvHashMap::new(&base);

        let k = key(addr(0xAA), b"data");

        // tx 2 deletes the key.
        let mut ws = HashMap::new();
        ws.insert(k.clone(), None);
        mv.apply_writes(2, &ws).unwrap();

        // tx 3 sees the deletion.
        assert_eq!(mv.read(3, &k).unwrap(), None);
        // tx 1 still sees base state.
        assert_eq!(mv.read(1, &k).unwrap(), Some(vec![1, 2, 3]));
    }
}
