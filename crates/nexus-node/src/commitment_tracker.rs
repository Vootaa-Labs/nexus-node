//! Commitment tracker — maintains state commitment + backup hash tree in
//! lockstep with the execution pipeline.
//!
//! The tracker holds both the primary [`Blake3SmtCommitment`] and the
//! [`Blake3BackupTree`], feeding them every state change that passes
//! through the execution bridge. At epoch boundaries it performs the
//! cross-tree consistency check and persists the commitment root.
//!
//! ```text
//!  ExecutionBridge
//!       │
//!       ▼
//!  CommitmentTracker::apply_state_changes(changes)
//!       ├─ primary.update / delete
//!       └─ backup.insert / delete
//!
//!  Epoch boundary
//!       │
//!       ▼
//!  CommitmentTracker::epoch_boundary_check()
//!       └─ backup.assert_consistent_with_verkle(primary_root)  // cross-tree consistency
//! ```

use std::sync::{Arc, RwLock};

use nexus_primitives::Blake3Digest;
use nexus_storage::backup_tree::Blake3BackupTree;
use nexus_storage::commitment::Blake3SmtCommitment;
use nexus_storage::commitment_persist::CommitmentMetaRecord;
use nexus_storage::traits::{BackupHashTree, StateCommitment};
use nexus_storage::{CommitmentPersistence, StateStorage, StorageError};
use tracing::{debug, info};

/// Shared commitment tracker accessible by both the execution bridge and
/// the RPC layer (for proof queries).
pub type SharedCommitmentTracker = Arc<RwLock<CommitmentTracker>>;

/// Object-safe persistence adapter used by the tracker.
pub trait CommitmentPersistSync: Send + Sync {
    /// Load the active persisted metadata if present.
    fn load_meta(&self) -> Result<Option<CommitmentMetaRecord>, StorageError>;
    /// Restore ordered key/value entries from persisted commitment data.
    #[allow(clippy::type_complexity)]
    fn restore_entries(&self) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StorageError>;
    /// Apply a state-change set and publish one new active commitment version.
    fn apply_change_set(
        &self,
        changes: &[(Vec<u8>, Option<Vec<u8>>)],
    ) -> Result<CommitmentMetaRecord, StorageError>;
}

/// Generic persistence backend for the commitment tracker.
pub struct PersistentCommitmentBackend<S: StateStorage> {
    inner: CommitmentPersistence<S>,
}

impl<S: StateStorage> PersistentCommitmentBackend<S> {
    /// Create a new persistence backend with an active-leaf cache budget.
    pub fn new(store: S, cache_capacity: usize) -> Self {
        Self {
            inner: CommitmentPersistence::with_cache_capacity(store, cache_capacity),
        }
    }
}

impl<S: StateStorage> CommitmentPersistSync for PersistentCommitmentBackend<S> {
    fn load_meta(&self) -> Result<Option<CommitmentMetaRecord>, StorageError> {
        self.inner.load_meta()
    }

    fn restore_entries(&self) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StorageError> {
        self.inner.restore_entries()
    }

    fn apply_change_set(
        &self,
        changes: &[(Vec<u8>, Option<Vec<u8>>)],
    ) -> Result<CommitmentMetaRecord, StorageError> {
        self.inner.apply_change_set(changes)
    }
}

/// Create a new shared commitment tracker.
pub fn new_shared_tracker() -> SharedCommitmentTracker {
    Arc::new(RwLock::new(CommitmentTracker::new()))
}

/// Create a new shared commitment tracker backed by persistent commitment state.
pub fn new_shared_tracker_with_persistence<S: StateStorage>(
    store: S,
    cache_capacity: usize,
) -> Result<SharedCommitmentTracker, StorageError> {
    let tracker = CommitmentTracker::with_persistence(Box::new(PersistentCommitmentBackend::new(
        store,
        cache_capacity,
    )))?;
    Ok(Arc::new(RwLock::new(tracker)))
}

/// A single state change to feed into the commitment tracker.
pub struct StateChangeEntry<'a> {
    /// The full composite storage key (shard_id ‖ address ‖ user_key).
    pub key: &'a [u8],
    /// `Some(value)` for inserts/updates, `None` for deletes.
    pub value: Option<&'a [u8]>,
}

/// Maintains the primary and backup commitment trees.
pub struct CommitmentTracker {
    primary: Blake3SmtCommitment,
    backup: Blake3BackupTree,
    persistence: Option<Box<dyn CommitmentPersistSync>>,
    persisted_tree_version: Option<u64>,
    /// Total state changes applied since creation.
    updates_applied: u64,
    /// Epoch boundary checks passed.
    epoch_checks_passed: u64,
}

impl CommitmentTracker {
    /// Create a new tracker with empty trees.
    pub fn new() -> Self {
        Self {
            primary: Blake3SmtCommitment::new(),
            backup: Blake3BackupTree::new(),
            persistence: None,
            persisted_tree_version: None,
            updates_applied: 0,
            epoch_checks_passed: 0,
        }
    }

    /// Create a tracker that restores its primary tree from persisted commitment data.
    pub fn with_persistence(
        persistence: Box<dyn CommitmentPersistSync>,
    ) -> Result<Self, StorageError> {
        let mut tracker = Self {
            primary: Blake3SmtCommitment::new(),
            backup: Blake3BackupTree::new(),
            persistence: Some(persistence),
            persisted_tree_version: None,
            updates_applied: 0,
            epoch_checks_passed: 0,
        };

        if let Some(ref persist) = tracker.persistence {
            let entries = persist.restore_entries()?;
            if !entries.is_empty() {
                let kv_refs: Vec<(&[u8], &[u8])> = entries
                    .iter()
                    .map(|(key, value)| (key.as_slice(), value.as_slice()))
                    .collect();
                tracker.primary.update(&kv_refs);
                for (key, value) in &entries {
                    tracker.backup.insert(key, value);
                }
            }
            tracker.persisted_tree_version = persist.load_meta()?.map(|meta| meta.tree_version);
        }

        Ok(tracker)
    }

    /// Apply a batch of state changes to both trees.
    ///
    /// This should be called after each block's state changes are persisted
    /// to storage, keeping the commitment trees in sync.
    pub fn apply_state_changes(&mut self, changes: &[StateChangeEntry<'_>]) {
        self.try_apply_state_changes(changes)
            .expect("commitment tracker apply_state_changes failed");
    }

    /// Apply a batch of state changes to both trees, surfacing persistence errors.
    pub fn try_apply_state_changes(
        &mut self,
        changes: &[StateChangeEntry<'_>],
    ) -> Result<(), StorageError> {
        let keyed_changes: Vec<(Vec<u8>, Option<Vec<u8>>)> = changes
            .iter()
            .map(|change| {
                (
                    change.key.to_vec(),
                    change.value.map(|value| value.to_vec()),
                )
            })
            .collect();

        if let Some(ref persistence) = self.persistence {
            let meta = persistence.apply_change_set(&keyed_changes)?;
            self.persisted_tree_version = Some(meta.tree_version);
        }

        let mut inserts: Vec<(&[u8], &[u8])> = Vec::new();

        for change in changes {
            match change.value {
                Some(val) => {
                    inserts.push((change.key, val));
                    self.backup.insert(change.key, val);
                }
                None => {
                    self.primary.delete(change.key);
                    self.backup.delete(change.key);
                }
            }
        }

        if !inserts.is_empty() {
            self.primary.update(&inserts);
        }

        self.updates_applied += changes.len() as u64;
        Ok(())
    }

    /// Return the current primary commitment root.
    pub fn commitment_root(&self) -> Blake3Digest {
        self.primary.root_commitment()
    }

    /// Return the current backup tree root.
    pub fn backup_root(&self) -> Blake3Digest {
        self.backup.root()
    }

    /// Generate an inclusion/exclusion proof for a single key.
    pub fn prove_key(
        &self,
        key: &[u8],
    ) -> Result<(Option<Vec<u8>>, nexus_storage::MerkleProof), nexus_storage::StorageError> {
        self.primary.prove_key(key)
    }

    /// Generate proofs for multiple keys.
    #[allow(clippy::type_complexity)]
    pub fn prove_keys(
        &self,
        keys: &[&[u8]],
    ) -> Result<Vec<(Option<Vec<u8>>, nexus_storage::MerkleProof)>, nexus_storage::StorageError>
    {
        self.primary.prove_keys(keys)
    }

    /// Perform the epoch-boundary consistency check between the primary
    /// and backup trees.
    ///
    /// Checks:
    /// 1. Both trees have the same entry count.
    /// 2. The backup tree passes its internal root-validity check against
    ///    the primary root.
    ///
    /// **Critical**: if this check fails, block production must halt.
    /// The caller is responsible for enforcing this invariant.
    pub fn epoch_boundary_check(&mut self) -> Result<(), nexus_storage::StorageError> {
        let primary_root = self.primary.root_commitment();
        let primary_len = self.primary.len();
        let backup_len = self.backup.len();

        info!(
            primary_root = %hex::encode(primary_root.0),
            backup_root = %hex::encode(self.backup.root().0),
            primary_entries = primary_len,
            backup_entries = backup_len,
            updates = self.updates_applied,
            "commitment tracker: epoch boundary check"
        );

        // Entry-count divergence means one tree missed an insert or delete.
        if primary_len != backup_len {
            return Err(nexus_storage::StorageError::StateCommitment(format!(
                "epoch boundary: entry count mismatch — primary has {primary_len} \
                     entries, backup has {backup_len}"
            )));
        }

        self.backup.assert_consistent_with_verkle(&primary_root)?;

        self.epoch_checks_passed += 1;
        debug!(
            epoch_checks = self.epoch_checks_passed,
            "commitment tracker: epoch boundary check passed"
        );

        Ok(())
    }

    /// Number of entries in the primary commitment tree.
    pub fn entry_count(&self) -> usize {
        self.primary.len()
    }

    /// Total state changes applied since creation.
    pub fn updates_applied(&self) -> u64 {
        self.updates_applied
    }

    /// Epoch boundary checks that have passed.
    pub fn epoch_checks_passed(&self) -> u64 {
        self.epoch_checks_passed
    }

    /// Active persisted tree version, if the tracker is backed by persistence.
    pub fn persisted_tree_version(&self) -> Option<u64> {
        self.persisted_tree_version
    }
}

impl Default for CommitmentTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracker_apply_and_root() {
        let mut tracker = CommitmentTracker::new();
        tracker.apply_state_changes(&[
            StateChangeEntry {
                key: b"k1",
                value: Some(b"v1"),
            },
            StateChangeEntry {
                key: b"k2",
                value: Some(b"v2"),
            },
        ]);

        assert_eq!(tracker.entry_count(), 2);
        assert_eq!(tracker.updates_applied(), 2);
        assert_ne!(tracker.commitment_root(), Blake3Digest::ZERO);
        assert_ne!(tracker.backup_root(), Blake3Digest::ZERO);
    }

    #[test]
    fn tracker_delete() {
        let mut tracker = CommitmentTracker::new();
        tracker.apply_state_changes(&[StateChangeEntry {
            key: b"k1",
            value: Some(b"v1"),
        }]);
        let root_before = tracker.commitment_root();

        tracker.apply_state_changes(&[StateChangeEntry {
            key: b"k1",
            value: None,
        }]);
        let root_after = tracker.commitment_root();
        assert_ne!(root_before, root_after);
        assert_eq!(tracker.entry_count(), 0);
    }

    #[test]
    fn tracker_epoch_check_passes() {
        let mut tracker = CommitmentTracker::new();
        tracker.apply_state_changes(&[StateChangeEntry {
            key: b"k1",
            value: Some(b"v1"),
        }]);
        tracker.epoch_boundary_check().unwrap();
        assert_eq!(tracker.epoch_checks_passed(), 1);
    }

    #[test]
    fn tracker_prove_key() {
        let mut tracker = CommitmentTracker::new();
        tracker.apply_state_changes(&[
            StateChangeEntry {
                key: b"a",
                value: Some(b"1"),
            },
            StateChangeEntry {
                key: b"b",
                value: Some(b"2"),
            },
        ]);

        let root = tracker.commitment_root();
        let (value, proof) = tracker.prove_key(b"a").unwrap();
        assert_eq!(value, Some(b"1".to_vec()));

        Blake3SmtCommitment::verify_proof(&root, b"a", Some(b"1"), &proof).unwrap();
    }

    #[test]
    fn tracker_prove_missing_key() {
        let mut tracker = CommitmentTracker::new();
        tracker.apply_state_changes(&[StateChangeEntry {
            key: b"a",
            value: Some(b"1"),
        }]);

        let (value, proof) = tracker.prove_key(b"missing").unwrap();
        assert!(value.is_none());
        assert_eq!(proof.leaf_count(), 1);
    }

    #[test]
    fn shared_tracker_thread_safe() {
        let shared = new_shared_tracker();

        {
            let mut t = shared.write().unwrap();
            t.apply_state_changes(&[StateChangeEntry {
                key: b"k",
                value: Some(b"v"),
            }]);
        }

        {
            let t = shared.read().unwrap();
            assert_eq!(t.entry_count(), 1);
        }
    }

    #[test]
    fn persistent_tracker_restores_root_from_commitment_meta() {
        let store = nexus_storage::MemoryStore::new();
        {
            let mut tracker = CommitmentTracker::with_persistence(Box::new(
                PersistentCommitmentBackend::new(store.clone(), 16),
            ))
            .unwrap();
            tracker
                .try_apply_state_changes(&[
                    StateChangeEntry {
                        key: b"a",
                        value: Some(b"1"),
                    },
                    StateChangeEntry {
                        key: b"b",
                        value: Some(b"2"),
                    },
                ])
                .unwrap();
            assert_eq!(tracker.persisted_tree_version(), Some(1));
        }

        let restored = CommitmentTracker::with_persistence(Box::new(
            PersistentCommitmentBackend::new(store, 16),
        ))
        .unwrap();
        assert_eq!(restored.entry_count(), 2);
        assert_eq!(restored.prove_key(b"a").unwrap().0, Some(b"1".to_vec()));
        assert_eq!(restored.persisted_tree_version(), Some(1));
    }
}
