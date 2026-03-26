// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Versioned persistence layout for state commitment data.
//!
//! Phase M starts by defining dedicated storage records for commitment
//! metadata, ordered leaves, and internal node hashes. This module keeps the
//! layout versioned so the active tree can move forward without requiring
//! in-place range deletes.

use std::collections::BTreeSet;
use std::sync::RwLock;

use nexus_primitives::Blake3Digest;
use serde::{Deserialize, Serialize};

use crate::commitment::{hash_leaf, hash_node, merkle_root_from_leaves};
use crate::error::StorageError;
use crate::traits::StateStorage;
use crate::types::ColumnFamily;

const LAYOUT_VERSION: u32 = 1;
const ACTIVE_TREE_KEY: &[u8] = b"active_tree_version";
const META_PREFIX: u8 = b'm';
const LEAF_INDEX_PREFIX: u8 = b'i';
const LEAF_LOOKUP_PREFIX: u8 = b'k';
const NODE_PREFIX: u8 = b'n';
const DELETED_LOOKUP_SENTINEL: &[u8] = b"";

/// Persisted metadata for the active commitment tree version.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommitmentMetaRecord {
    /// Layout/schema version for the commitment persistence format.
    pub layout_version: u32,
    /// Monotonic tree version written by the persistence layer.
    pub tree_version: u64,
    /// Canonical root for this tree version.
    pub root: Blake3Digest,
    /// Number of leaves in this tree version.
    pub leaf_count: u64,
    /// Previous tree version this version overlays, if any.
    pub base_tree_version: Option<u64>,
}

/// Persisted ordered leaf record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersistedLeafRecord {
    /// Leaf index in sorted key order.
    pub leaf_index: u64,
    /// Raw storage key.
    pub key: Vec<u8>,
    /// Raw storage value.
    pub value: Vec<u8>,
    /// Domain-separated leaf hash.
    pub leaf_hash: Blake3Digest,
}

/// Persisted internal node position.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct PersistedNodePosition {
    /// Level 0 is the leaf layer, level 1 is the first internal layer, etc.
    pub level: u32,
    /// Zero-based index within the layer.
    pub index: u64,
}

/// Persisted internal node record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersistedNodeRecord {
    /// Node position within the versioned tree.
    pub position: PersistedNodePosition,
    /// Stored node hash.
    pub hash: Blake3Digest,
}

/// Mutation class for an incremental commitment update.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum CommitmentMutationKind {
    /// Insert a previously absent key.
    Insert,
    /// Update an existing key in place.
    Update,
    /// Delete an existing key.
    Delete,
    /// Delete of a missing key; no tree mutation is required.
    Noop,
}

/// Skeleton plan describing which positions an incremental update must touch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IncrementalCommitmentPlan {
    /// Type of mutation being applied.
    pub kind: CommitmentMutationKind,
    /// Existing or inserted leaf position, if any.
    pub leaf_index: Option<u64>,
    /// First leaf index whose persisted ordering must be rewritten.
    pub reindex_from: Option<u64>,
    /// Leaf count before the mutation.
    pub leaf_count_before: u64,
    /// Leaf count after the mutation.
    pub leaf_count_after: u64,
    /// Tree version that should be written for the result.
    pub next_tree_version: u64,
    /// Leaf and internal node positions affected by the mutation.
    pub affected_positions: Vec<PersistedNodePosition>,
}

/// Versioned commitment persistence helper.
#[derive(Debug)]
pub struct CommitmentPersistence<S: StateStorage> {
    store: S,
    cache_capacity: usize,
    cached_active_leaves: RwLock<Option<(u64, Vec<PersistedLeafRecord>)>>,
}

impl<S: StateStorage> CommitmentPersistence<S> {
    /// Construct a persistence helper over a storage backend.
    pub fn new(store: S) -> Self {
        Self::with_cache_capacity(store, 0)
    }

    /// Construct a persistence helper with an active-leaf cache budget.
    pub fn with_cache_capacity(store: S, cache_capacity: usize) -> Self {
        Self {
            store,
            cache_capacity,
            cached_active_leaves: RwLock::new(None),
        }
    }

    /// Load the active commitment metadata, if any.
    pub fn load_meta(&self) -> Result<Option<CommitmentMetaRecord>, StorageError> {
        let Some(version_bytes) = self
            .store
            .get_sync(ColumnFamily::CommitmentMeta.as_str(), ACTIVE_TREE_KEY)?
        else {
            return Ok(None);
        };
        let version = decode_u64(&version_bytes, "active tree version")?;
        Ok(Some(self.load_meta_for_version(version)?))
    }

    /// Return all leaves for the active tree version in sorted order.
    pub fn load_ordered_leaves(&self) -> Result<Vec<PersistedLeafRecord>, StorageError> {
        let Some(meta) = self.load_meta()? else {
            return Ok(Vec::new());
        };
        self.load_ordered_leaves_for_meta(&meta)
    }

    /// Restore raw `(key, value)` entries for the active tree version.
    #[allow(clippy::type_complexity)]
    pub fn restore_entries(&self) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StorageError> {
        Ok(self
            .load_ordered_leaves()?
            .into_iter()
            .map(|leaf| (leaf.key, leaf.value))
            .collect())
    }

    /// Persist a full commitment snapshot under `tree_version` and mark it active.
    pub fn persist_snapshot(
        &self,
        entries: &[(Vec<u8>, Vec<u8>)],
        tree_version: u64,
    ) -> Result<CommitmentMetaRecord, StorageError> {
        let mut ordered_entries = entries.to_vec();
        ordered_entries.sort_by(|left, right| left.0.cmp(&right.0));

        let leaves: Vec<PersistedLeafRecord> = ordered_entries
            .iter()
            .enumerate()
            .map(|(index, (key, value))| PersistedLeafRecord {
                leaf_index: index as u64,
                key: key.clone(),
                value: value.clone(),
                leaf_hash: hash_leaf(key, value),
            })
            .collect();

        let layers = merkle_layers(&leaves.iter().map(|leaf| leaf.leaf_hash).collect::<Vec<_>>());
        for leaf in &leaves {
            let encoded_leaf = bcs::to_bytes(leaf).map_err(|e| {
                StorageError::Serialization(format!("persisted leaf encode failed: {e}"))
            })?;
            self.store.put_sync(
                ColumnFamily::CommitmentLeaves.as_str(),
                leaf_index_key(tree_version, leaf.leaf_index),
                encoded_leaf,
            )?;
            self.store.put_sync(
                ColumnFamily::CommitmentLeaves.as_str(),
                leaf_lookup_key(tree_version, &leaf.key),
                leaf.leaf_index.to_be_bytes().to_vec(),
            )?;
        }

        for (level, layer) in layers.iter().enumerate().skip(1) {
            for (index, hash) in layer.iter().enumerate() {
                let record = PersistedNodeRecord {
                    position: PersistedNodePosition {
                        level: level as u32,
                        index: index as u64,
                    },
                    hash: *hash,
                };
                self.store.put_sync(
                    ColumnFamily::CommitmentNodes.as_str(),
                    node_key(tree_version, record.position.level, record.position.index),
                    bcs::to_bytes(&record).map_err(|e| {
                        StorageError::Serialization(format!("persisted node encode failed: {e}"))
                    })?,
                )?;
            }
        }

        let meta = CommitmentMetaRecord {
            layout_version: LAYOUT_VERSION,
            tree_version,
            root: merkle_root_from_leaves(
                &leaves.iter().map(|leaf| leaf.leaf_hash).collect::<Vec<_>>(),
            ),
            leaf_count: leaves.len() as u64,
            base_tree_version: None,
        };
        self.store.put_sync(
            ColumnFamily::CommitmentMeta.as_str(),
            meta_key(tree_version),
            bcs::to_bytes(&meta).map_err(|e| {
                StorageError::Serialization(format!("commitment meta encode failed: {e}"))
            })?,
        )?;
        self.store.put_sync(
            ColumnFamily::CommitmentMeta.as_str(),
            ACTIVE_TREE_KEY.to_vec(),
            tree_version.to_be_bytes().to_vec(),
        )?;

        Ok(meta)
    }

    /// Apply a single mutation by writing a new versioned overlay.
    pub fn apply_mutation(
        &self,
        key: &[u8],
        value: Option<&[u8]>,
    ) -> Result<CommitmentMetaRecord, StorageError> {
        let changes = vec![(key.to_vec(), value.map(|bytes| bytes.to_vec()))];
        self.apply_change_set(&changes)
    }

    /// Apply a batch of key/value changes and publish one new active tree version.
    pub fn apply_change_set(
        &self,
        changes: &[(Vec<u8>, Option<Vec<u8>>)],
    ) -> Result<CommitmentMetaRecord, StorageError> {
        let active_meta = self.load_meta()?;
        let mut ordered_entries = self.restore_entries()?;
        let mut reindex_from: Option<u64> = None;
        let mut affected_positions = BTreeSet::new();
        let mut saw_effective_change = false;

        for (key, value) in changes {
            let plan = self.plan_mutation(key, value.as_deref())?;
            if plan.kind == CommitmentMutationKind::Noop {
                continue;
            }
            saw_effective_change = true;
            reindex_from = match (reindex_from, plan.reindex_from) {
                (Some(existing), Some(next)) => Some(existing.min(next)),
                (None, Some(next)) => Some(next),
                (existing, None) => existing,
            };
            affected_positions.extend(plan.affected_positions);

            match (plan.kind, plan.leaf_index, value) {
                (CommitmentMutationKind::Insert, Some(index), Some(new_value)) => {
                    ordered_entries.insert(index as usize, (key.clone(), new_value.clone()));
                }
                (CommitmentMutationKind::Update, Some(index), Some(new_value)) => {
                    ordered_entries[index as usize] = (key.clone(), new_value.clone());
                }
                (CommitmentMutationKind::Delete, Some(index), None) => {
                    ordered_entries.remove(index as usize);
                }
                _ => {
                    return Err(StorageError::StateCommitment(
                        "invalid mutation plan/value combination".into(),
                    ));
                }
            }
        }

        if !saw_effective_change {
            return match active_meta {
                Some(meta) => Ok(meta),
                None => self.persist_snapshot(&[], 1),
            };
        }

        let next_tree_version = active_meta.as_ref().map_or(1, |meta| meta.tree_version + 1);

        let leaves: Vec<PersistedLeafRecord> = ordered_entries
            .iter()
            .enumerate()
            .map(|(index, (entry_key, entry_value))| PersistedLeafRecord {
                leaf_index: index as u64,
                key: entry_key.clone(),
                value: entry_value.clone(),
                leaf_hash: hash_leaf(entry_key, entry_value),
            })
            .collect();
        let leaf_hashes: Vec<Blake3Digest> = leaves.iter().map(|leaf| leaf.leaf_hash).collect();
        let layers = merkle_layers(&leaf_hashes);

        if let Some(start) = reindex_from {
            for leaf in leaves.iter().skip(start as usize) {
                self.write_leaf_record(next_tree_version, leaf)?;
            }
        } else {
            for (key, value) in changes {
                if value.is_none() {
                    continue;
                }
                if let Ok(index) = leaves.binary_search_by(|leaf| leaf.key.as_slice().cmp(key)) {
                    self.write_leaf_record(next_tree_version, &leaves[index])?;
                }
            }
        }

        for (key, value) in changes {
            if value.is_none() {
                self.store.put_sync(
                    ColumnFamily::CommitmentLeaves.as_str(),
                    leaf_lookup_key(next_tree_version, key),
                    DELETED_LOOKUP_SENTINEL.to_vec(),
                )?;
            }
        }

        for position in affected_positions
            .iter()
            .filter(|position| position.level > 0)
        {
            let Some(layer) = layers.get(position.level as usize) else {
                continue;
            };
            let Some(hash) = layer.get(position.index as usize) else {
                continue;
            };
            let record = PersistedNodeRecord {
                position: *position,
                hash: *hash,
            };
            self.store.put_sync(
                ColumnFamily::CommitmentNodes.as_str(),
                node_key(next_tree_version, position.level, position.index),
                bcs::to_bytes(&record).map_err(|e| {
                    StorageError::Serialization(format!("persisted node encode failed: {e}"))
                })?,
            )?;
        }

        let meta = CommitmentMetaRecord {
            layout_version: LAYOUT_VERSION,
            tree_version: next_tree_version,
            root: merkle_root_from_leaves(&leaf_hashes),
            leaf_count: leaves.len() as u64,
            base_tree_version: active_meta.as_ref().map(|meta| meta.tree_version),
        };
        self.store.put_sync(
            ColumnFamily::CommitmentMeta.as_str(),
            meta_key(next_tree_version),
            bcs::to_bytes(&meta).map_err(|e| {
                StorageError::Serialization(format!("commitment meta encode failed: {e}"))
            })?,
        )?;
        self.store.put_sync(
            ColumnFamily::CommitmentMeta.as_str(),
            ACTIVE_TREE_KEY.to_vec(),
            next_tree_version.to_be_bytes().to_vec(),
        )?;

        self.update_cache(&meta, &leaves);

        Ok(meta)
    }

    /// Plan which positions a future incremental update must touch.
    pub fn plan_mutation(
        &self,
        key: &[u8],
        value: Option<&[u8]>,
    ) -> Result<IncrementalCommitmentPlan, StorageError> {
        let meta = self.load_meta()?.unwrap_or(CommitmentMetaRecord {
            layout_version: LAYOUT_VERSION,
            tree_version: 0,
            root: merkle_root_from_leaves(&[]),
            leaf_count: 0,
            base_tree_version: None,
        });
        let leaves = self.load_ordered_leaves()?;
        let search = leaves.binary_search_by(|leaf| leaf.key.as_slice().cmp(key));

        let (kind, leaf_index, reindex_from, leaf_count_after) = match (search, value) {
            (Ok(index), Some(_)) => (
                CommitmentMutationKind::Update,
                Some(index as u64),
                None,
                meta.leaf_count,
            ),
            (Err(index), Some(_)) => (
                CommitmentMutationKind::Insert,
                Some(index as u64),
                Some(index as u64),
                meta.leaf_count + 1,
            ),
            (Ok(index), None) => (
                CommitmentMutationKind::Delete,
                Some(index as u64),
                Some(index as u64),
                meta.leaf_count.saturating_sub(1),
            ),
            (Err(_), None) => (CommitmentMutationKind::Noop, None, None, meta.leaf_count),
        };

        let mut affected = BTreeSet::new();
        for index in affected_leaf_indices(kind, leaf_index, leaf_count_after) {
            for position in path_positions(index as usize, leaf_count_after as usize) {
                affected.insert(position);
            }
        }

        Ok(IncrementalCommitmentPlan {
            kind,
            leaf_index,
            reindex_from,
            leaf_count_before: meta.leaf_count,
            leaf_count_after,
            next_tree_version: meta.tree_version + 1,
            affected_positions: affected.into_iter().collect(),
        })
    }

    fn load_meta_for_version(
        &self,
        tree_version: u64,
    ) -> Result<CommitmentMetaRecord, StorageError> {
        let Some(meta_bytes) = self.store.get_sync(
            ColumnFamily::CommitmentMeta.as_str(),
            &meta_key(tree_version),
        )?
        else {
            return Err(StorageError::StateCommitment(format!(
                "commitment meta record missing for tree version {tree_version}"
            )));
        };
        bcs::from_bytes::<CommitmentMetaRecord>(&meta_bytes)
            .map_err(|e| StorageError::Serialization(format!("commitment meta decode failed: {e}")))
    }

    fn load_ordered_leaves_for_meta(
        &self,
        meta: &CommitmentMetaRecord,
    ) -> Result<Vec<PersistedLeafRecord>, StorageError> {
        if let Some(cached) = self.cached_active(meta.tree_version) {
            return Ok(cached);
        }

        let mut leaves = Vec::with_capacity(meta.leaf_count as usize);
        for leaf_index in 0..meta.leaf_count {
            leaves.push(self.resolve_leaf_record(meta, leaf_index)?);
        }
        self.update_cache(meta, &leaves);
        Ok(leaves)
    }

    fn resolve_leaf_record(
        &self,
        meta: &CommitmentMetaRecord,
        leaf_index: u64,
    ) -> Result<PersistedLeafRecord, StorageError> {
        let mut current = meta.clone();
        loop {
            if let Some(bytes) = self.store.get_sync(
                ColumnFamily::CommitmentLeaves.as_str(),
                &leaf_index_key(current.tree_version, leaf_index),
            )? {
                return bcs::from_bytes::<PersistedLeafRecord>(&bytes).map_err(|e| {
                    StorageError::Serialization(format!("persisted leaf decode failed: {e}"))
                });
            }

            let Some(base_version) = current.base_tree_version else {
                return Err(StorageError::StateCommitment(format!(
                    "leaf {leaf_index} missing in commitment version chain ending at {}",
                    current.tree_version
                )));
            };
            current = self.load_meta_for_version(base_version)?;
        }
    }

    fn write_leaf_record(
        &self,
        tree_version: u64,
        leaf: &PersistedLeafRecord,
    ) -> Result<(), StorageError> {
        self.store.put_sync(
            ColumnFamily::CommitmentLeaves.as_str(),
            leaf_index_key(tree_version, leaf.leaf_index),
            bcs::to_bytes(leaf).map_err(|e| {
                StorageError::Serialization(format!("persisted leaf encode failed: {e}"))
            })?,
        )?;
        self.store.put_sync(
            ColumnFamily::CommitmentLeaves.as_str(),
            leaf_lookup_key(tree_version, &leaf.key),
            leaf.leaf_index.to_be_bytes().to_vec(),
        )?;
        Ok(())
    }

    fn cached_active(&self, tree_version: u64) -> Option<Vec<PersistedLeafRecord>> {
        self.cached_active_leaves
            .read()
            .ok()
            .and_then(|guard| guard.as_ref().cloned())
            .and_then(|(cached_version, leaves)| {
                if cached_version == tree_version {
                    Some(leaves)
                } else {
                    None
                }
            })
    }

    fn update_cache(&self, meta: &CommitmentMetaRecord, leaves: &[PersistedLeafRecord]) {
        if let Ok(mut guard) = self.cached_active_leaves.write() {
            if self.cache_capacity == 0 || leaves.len() > self.cache_capacity {
                *guard = None;
            } else {
                *guard = Some((meta.tree_version, leaves.to_vec()));
            }
        }
    }
}

fn affected_leaf_indices(
    kind: CommitmentMutationKind,
    leaf_index: Option<u64>,
    leaf_count_after: u64,
) -> Vec<u64> {
    let Some(index) = leaf_index else {
        return Vec::new();
    };
    if leaf_count_after == 0 {
        return Vec::new();
    }

    let mut candidates = BTreeSet::new();
    match kind {
        CommitmentMutationKind::Insert => {
            candidates.insert(index.min(leaf_count_after - 1));
            if index > 0 {
                candidates.insert(index - 1);
            }
            if index + 1 < leaf_count_after {
                candidates.insert(index + 1);
            }
        }
        CommitmentMutationKind::Update => {
            candidates.insert(index);
        }
        CommitmentMutationKind::Delete => {
            if index > 0 {
                candidates.insert(index - 1);
            }
            if index < leaf_count_after {
                candidates.insert(index);
            }
        }
        CommitmentMutationKind::Noop => {}
    }
    candidates.into_iter().collect()
}

fn path_positions(index: usize, leaf_count: usize) -> Vec<PersistedNodePosition> {
    if leaf_count == 0 {
        return Vec::new();
    }

    let mut positions = Vec::new();
    let mut idx = index;
    let mut width = leaf_count;
    let mut level = 0u32;

    loop {
        positions.push(PersistedNodePosition {
            level,
            index: idx as u64,
        });
        if width <= 1 {
            break;
        }
        if width % 2 != 0 {
            width += 1;
        }
        idx /= 2;
        width /= 2;
        level += 1;
    }

    positions
}

fn merkle_layers(leaves: &[Blake3Digest]) -> Vec<Vec<Blake3Digest>> {
    if leaves.is_empty() {
        return Vec::new();
    }

    let mut layers = vec![leaves.to_vec()];
    while layers.last().map_or(0, Vec::len) > 1 {
        let mut current = layers.last().cloned().unwrap_or_default();
        if current.len() % 2 != 0 {
            let last = current[current.len() - 1];
            current.push(last);
        }
        let next: Vec<Blake3Digest> = current
            .chunks(2)
            .map(|pair| hash_node(&pair[0], &pair[1]))
            .collect();
        layers.push(next);
    }
    layers
}

fn meta_key(tree_version: u64) -> Vec<u8> {
    let mut key = vec![META_PREFIX];
    key.extend_from_slice(&tree_version.to_be_bytes());
    key
}

fn leaf_index_key(tree_version: u64, leaf_index: u64) -> Vec<u8> {
    let mut key = vec![LEAF_INDEX_PREFIX];
    key.extend_from_slice(&tree_version.to_be_bytes());
    key.extend_from_slice(&leaf_index.to_be_bytes());
    key
}

fn leaf_lookup_key(tree_version: u64, raw_key: &[u8]) -> Vec<u8> {
    let mut key = vec![LEAF_LOOKUP_PREFIX];
    key.extend_from_slice(&tree_version.to_be_bytes());
    key.extend_from_slice(&(raw_key.len() as u32).to_be_bytes());
    key.extend_from_slice(raw_key);
    key
}

fn node_key(tree_version: u64, level: u32, index: u64) -> Vec<u8> {
    let mut key = vec![NODE_PREFIX];
    key.extend_from_slice(&tree_version.to_be_bytes());
    key.extend_from_slice(&level.to_be_bytes());
    key.extend_from_slice(&index.to_be_bytes());
    key
}

fn decode_u64(bytes: &[u8], label: &str) -> Result<u64, StorageError> {
    if bytes.len() != 8 {
        return Err(StorageError::Serialization(format!(
            "{label} expects 8 bytes, got {}",
            bytes.len()
        )));
    }
    let raw: [u8; 8] = bytes.try_into().unwrap();
    Ok(u64::from_be_bytes(raw))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::MemoryStore;

    #[test]
    fn persist_snapshot_roundtrip_restores_entries_and_meta() {
        let store = MemoryStore::new();
        let persistence = CommitmentPersistence::new(store);
        let entries = vec![
            (b"gamma".to_vec(), b"3".to_vec()),
            (b"alpha".to_vec(), b"1".to_vec()),
            (b"beta".to_vec(), b"2".to_vec()),
        ];

        let meta = persistence.persist_snapshot(&entries, 7).unwrap();
        assert_eq!(meta.tree_version, 7);
        assert_eq!(meta.leaf_count, 3);
        assert_eq!(meta.base_tree_version, None);

        let loaded_meta = persistence.load_meta().unwrap().unwrap();
        assert_eq!(loaded_meta, meta);

        let restored = persistence.restore_entries().unwrap();
        assert_eq!(
            restored,
            vec![
                (b"alpha".to_vec(), b"1".to_vec()),
                (b"beta".to_vec(), b"2".to_vec()),
                (b"gamma".to_vec(), b"3".to_vec()),
            ]
        );
    }

    #[test]
    fn plan_mutation_reports_insert_update_delete_and_noop() {
        let store = MemoryStore::new();
        let persistence = CommitmentPersistence::new(store);
        persistence
            .persist_snapshot(
                &[
                    (b"alpha".to_vec(), b"1".to_vec()),
                    (b"gamma".to_vec(), b"3".to_vec()),
                ],
                3,
            )
            .unwrap();

        let insert_plan = persistence.plan_mutation(b"beta", Some(b"2")).unwrap();
        assert_eq!(insert_plan.kind, CommitmentMutationKind::Insert);
        assert_eq!(insert_plan.leaf_index, Some(1));
        assert_eq!(insert_plan.reindex_from, Some(1));
        assert_eq!(insert_plan.leaf_count_before, 2);
        assert_eq!(insert_plan.leaf_count_after, 3);
        assert!(!insert_plan.affected_positions.is_empty());

        let update_plan = persistence
            .plan_mutation(b"gamma", Some(b"updated"))
            .unwrap();
        assert_eq!(update_plan.kind, CommitmentMutationKind::Update);
        assert_eq!(update_plan.leaf_index, Some(1));
        assert_eq!(update_plan.reindex_from, None);
        assert_eq!(update_plan.leaf_count_after, 2);

        let delete_plan = persistence.plan_mutation(b"alpha", None).unwrap();
        assert_eq!(delete_plan.kind, CommitmentMutationKind::Delete);
        assert_eq!(delete_plan.leaf_index, Some(0));
        assert_eq!(delete_plan.reindex_from, Some(0));
        assert_eq!(delete_plan.leaf_count_after, 1);

        let noop_plan = persistence.plan_mutation(b"missing", None).unwrap();
        assert_eq!(noop_plan.kind, CommitmentMutationKind::Noop);
        assert!(noop_plan.leaf_index.is_none());
        assert!(noop_plan.affected_positions.is_empty());
    }

    #[test]
    fn newer_tree_version_becomes_active_without_deleting_old_versions() {
        let store = MemoryStore::new();
        let persistence = CommitmentPersistence::new(store.clone());
        persistence
            .persist_snapshot(&[(b"a".to_vec(), b"1".to_vec())], 1)
            .unwrap();
        persistence
            .persist_snapshot(&[(b"b".to_vec(), b"2".to_vec())], 2)
            .unwrap();

        let active = persistence.load_meta().unwrap().unwrap();
        assert_eq!(active.tree_version, 2);
        assert_eq!(
            persistence.restore_entries().unwrap(),
            vec![(b"b".to_vec(), b"2".to_vec())]
        );

        let old_meta = store
            .get_sync(ColumnFamily::CommitmentMeta.as_str(), &meta_key(1))
            .unwrap();
        assert!(
            old_meta.is_some(),
            "older tree versions should remain addressable"
        );
    }

    #[test]
    fn apply_mutation_writes_versioned_overlay_for_insert_update_and_delete() {
        let store = MemoryStore::new();
        let persistence = CommitmentPersistence::new(store.clone());
        let initial = persistence
            .persist_snapshot(
                &[
                    (b"alpha".to_vec(), b"1".to_vec()),
                    (b"gamma".to_vec(), b"3".to_vec()),
                ],
                10,
            )
            .unwrap();

        let inserted = persistence.apply_mutation(b"beta", Some(b"2")).unwrap();
        assert_eq!(inserted.tree_version, 11);
        assert_eq!(inserted.base_tree_version, Some(initial.tree_version));
        assert_eq!(
            persistence.restore_entries().unwrap(),
            vec![
                (b"alpha".to_vec(), b"1".to_vec()),
                (b"beta".to_vec(), b"2".to_vec()),
                (b"gamma".to_vec(), b"3".to_vec()),
            ]
        );

        let updated = persistence.apply_mutation(b"gamma", Some(b"33")).unwrap();
        assert_eq!(updated.tree_version, 12);
        assert_eq!(updated.base_tree_version, Some(inserted.tree_version));
        assert_eq!(
            persistence.restore_entries().unwrap(),
            vec![
                (b"alpha".to_vec(), b"1".to_vec()),
                (b"beta".to_vec(), b"2".to_vec()),
                (b"gamma".to_vec(), b"33".to_vec()),
            ]
        );

        let deleted = persistence.apply_mutation(b"beta", None).unwrap();
        assert_eq!(deleted.tree_version, 13);
        assert_eq!(deleted.base_tree_version, Some(updated.tree_version));
        assert_eq!(
            persistence.restore_entries().unwrap(),
            vec![
                (b"alpha".to_vec(), b"1".to_vec()),
                (b"gamma".to_vec(), b"33".to_vec())
            ]
        );

        let tombstone = store
            .get_sync(
                ColumnFamily::CommitmentLeaves.as_str(),
                &leaf_lookup_key(deleted.tree_version, b"beta"),
            )
            .unwrap()
            .unwrap();
        assert!(
            tombstone.is_empty(),
            "deleted lookups should be tombstoned in the overlay"
        );
    }

    #[test]
    fn noop_mutation_keeps_current_version() {
        let store = MemoryStore::new();
        let persistence = CommitmentPersistence::new(store);
        let initial = persistence
            .persist_snapshot(&[(b"alpha".to_vec(), b"1".to_vec())], 5)
            .unwrap();

        let noop = persistence.apply_mutation(b"missing", None).unwrap();
        assert_eq!(noop.tree_version, initial.tree_version);
        assert_eq!(noop.base_tree_version, initial.base_tree_version);
    }
}
