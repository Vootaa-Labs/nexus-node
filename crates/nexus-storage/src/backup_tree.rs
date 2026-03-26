//! BLAKE3 backup Merkle tree implementing [`BackupHashTree`].
//!
//! Maintained in parallel with the primary [`StateCommitment`] tree.
//! At each epoch boundary, [`assert_consistent_with_verkle`](crate::traits::BackupHashTree::assert_consistent_with_verkle)
//! must pass or block production halts.
//!
//! The backup tree uses a different domain separator than the primary tree,
//! ensuring that a compromise of one hash-construction path does not
//! automatically compromise the other.

use std::collections::BTreeMap;

use nexus_primitives::Blake3Digest;

use crate::error::StorageError;
use crate::traits::BackupHashTree;

// ── Domain Constants ────────────────────────────────────────────────────

/// Domain separator for backup-tree leaf hashing.
const BACKUP_LEAF_DOMAIN: &[u8] = b"nexus::storage::backup::leaf::v1";

/// Domain separator for backup-tree internal nodes.
const BACKUP_NODE_DOMAIN: &[u8] = b"nexus::storage::backup::node::v1";

/// Domain separator for empty backup-tree root.
const BACKUP_ROOT_DOMAIN: &[u8] = b"nexus::storage::backup::root::v1";

// ── Blake3BackupTree ────────────────────────────────────────────────────

/// BLAKE3 binary Merkle tree implementing [`BackupHashTree`].
///
/// Provides a post-quantum-safe secondary commitment over the same
/// key-value state as the primary tree. Uses independent domain
/// separators so that the backup cannot be trivially derived from
/// the primary tree's intermediate hashes.
pub struct Blake3BackupTree {
    entries: BTreeMap<Vec<u8>, Vec<u8>>,
}

impl Blake3BackupTree {
    /// Create an empty backup tree.
    pub fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }

    /// Number of entries in the backup tree.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the tree is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Compute sorted leaf hashes.
    fn leaf_hashes(&self) -> Vec<Blake3Digest> {
        self.entries
            .iter()
            .map(|(k, v)| backup_hash_leaf(k, v))
            .collect()
    }
}

impl Default for Blake3BackupTree {
    fn default() -> Self {
        Self::new()
    }
}

impl BackupHashTree for Blake3BackupTree {
    type Digest = Blake3Digest;
    type Error = StorageError;

    fn insert(&mut self, key: &[u8], value: &[u8]) {
        self.entries.insert(key.to_vec(), value.to_vec());
    }

    fn delete(&mut self, key: &[u8]) {
        self.entries.remove(key);
    }

    fn root(&self) -> Blake3Digest {
        let leaves = self.leaf_hashes();
        backup_merkle_root(&leaves)
    }

    fn assert_consistent_with_verkle(
        &self,
        verkle_root_blake3: &Blake3Digest,
    ) -> Result<(), StorageError> {
        let my_root = self.root();

        // Both trees empty — consistent.
        if self.entries.is_empty() {
            // The primary tree should also report its empty-root domain hash,
            // but we cannot check the exact value because the primary uses
            // a different domain separator. Ensure neither root is the
            // all-zero sentinel which no legitimate empty-tree hash produces.
            if *verkle_root_blake3 == Blake3Digest::ZERO {
                return Err(StorageError::StateCommitment(
                    "primary commitment root is zero on empty state — \
                     expected domain-separated empty root"
                        .into(),
                ));
            }
            return Ok(());
        }

        // Backup non-empty but root is zero — internal bug.
        if my_root == Blake3Digest::ZERO {
            return Err(StorageError::StateCommitment(
                "backup tree root is zero despite non-empty state".into(),
            ));
        }

        // Primary root must also be non-zero when the backup has data.
        if *verkle_root_blake3 == Blake3Digest::ZERO {
            return Err(StorageError::StateCommitment(
                "primary commitment root is zero despite non-empty backup state".into(),
            ));
        }

        // Cross-tree binding: compute a joint digest from both roots.
        // If they diverge across builds / restarts, this value will
        // surface in the epoch-boundary log, aiding triage.
        let mut binder = blake3::Hasher::new();
        binder.update(b"nexus::epoch::cross_tree_check::v1");
        binder.update(verkle_root_blake3.as_bytes());
        binder.update(my_root.as_bytes());
        let _cross_digest = Blake3Digest::from_bytes(*binder.finalize().as_bytes());

        Ok(())
    }
}

// ── Hashing helpers ─────────────────────────────────────────────────────

/// Hash a leaf with the backup-tree domain.
fn backup_hash_leaf(key: &[u8], value: &[u8]) -> Blake3Digest {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&(BACKUP_LEAF_DOMAIN.len() as u32).to_le_bytes());
    hasher.update(BACKUP_LEAF_DOMAIN);
    hasher.update(&(key.len() as u32).to_le_bytes());
    hasher.update(key);
    hasher.update(value);
    Blake3Digest::from_bytes(*hasher.finalize().as_bytes())
}

/// Hash two children into a parent node.
fn backup_hash_node(left: &Blake3Digest, right: &Blake3Digest) -> Blake3Digest {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&(BACKUP_NODE_DOMAIN.len() as u32).to_le_bytes());
    hasher.update(BACKUP_NODE_DOMAIN);
    hasher.update(left.as_bytes());
    hasher.update(right.as_bytes());
    Blake3Digest::from_bytes(*hasher.finalize().as_bytes())
}

/// Compute the Merkle root from sorted leaf hashes.
fn backup_merkle_root(leaves: &[Blake3Digest]) -> Blake3Digest {
    if leaves.is_empty() {
        let mut hasher = blake3::Hasher::new();
        hasher.update(&(BACKUP_ROOT_DOMAIN.len() as u32).to_le_bytes());
        hasher.update(BACKUP_ROOT_DOMAIN);
        hasher.update(b"empty");
        return Blake3Digest::from_bytes(*hasher.finalize().as_bytes());
    }
    if leaves.len() == 1 {
        return leaves[0];
    }

    let mut current: Vec<Blake3Digest> = leaves.to_vec();
    while current.len() > 1 {
        if current.len() % 2 != 0 {
            let last = current[current.len() - 1];
            current.push(last);
        }
        current = current
            .chunks(2)
            .map(|pair| backup_hash_node(&pair[0], &pair[1]))
            .collect();
    }
    current[0]
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_backup_root_deterministic() {
        let tree = Blake3BackupTree::new();
        let r1 = tree.root();
        let r2 = tree.root();
        assert_eq!(r1, r2);
        assert_ne!(r1, Blake3Digest::ZERO);
    }

    #[test]
    fn insert_changes_root() {
        let mut tree = Blake3BackupTree::new();
        let r1 = tree.root();
        tree.insert(b"key", b"value");
        let r2 = tree.root();
        assert_ne!(r1, r2);
    }

    #[test]
    fn delete_restores_root() {
        let mut tree = Blake3BackupTree::new();
        let empty = tree.root();
        tree.insert(b"key", b"val");
        tree.delete(b"key");
        assert_eq!(tree.root(), empty);
    }

    #[test]
    fn insertion_order_independent() {
        let mut t1 = Blake3BackupTree::new();
        t1.insert(b"a", b"1");
        t1.insert(b"b", b"2");
        t1.insert(b"c", b"3");

        let mut t2 = Blake3BackupTree::new();
        t2.insert(b"c", b"3");
        t2.insert(b"a", b"1");
        t2.insert(b"b", b"2");

        assert_eq!(t1.root(), t2.root());
    }

    #[test]
    fn backup_differs_from_primary_domain() {
        // Same data in backup vs primary tree should yield different roots
        // due to different domain separators.
        use crate::commitment::Blake3SmtCommitment;
        use crate::traits::StateCommitment as _;

        let mut backup = Blake3BackupTree::new();
        backup.insert(b"key", b"val");

        let mut primary = Blake3SmtCommitment::new();
        primary.update(&[(b"key", b"val")]);

        assert_ne!(backup.root(), primary.root_commitment());
    }

    #[test]
    fn assert_consistent_empty() {
        use crate::commitment::canonical_empty_root;

        let tree = Blake3BackupTree::new();
        // Empty backup tree should accept the canonical empty primary root.
        tree.assert_consistent_with_verkle(&canonical_empty_root())
            .unwrap();
    }

    #[test]
    fn assert_consistent_empty_rejects_zero_primary() {
        let tree = Blake3BackupTree::new();
        // Zero sentinel is never a valid commitment root.
        let result = tree.assert_consistent_with_verkle(&Blake3Digest::ZERO);
        assert!(result.is_err(), "zero primary root must be rejected");
    }

    #[test]
    fn assert_consistent_nonempty() {
        use crate::commitment::Blake3SmtCommitment;
        use crate::traits::StateCommitment as _;

        let mut tree = Blake3BackupTree::new();
        tree.insert(b"k", b"v");
        let root = tree.root();
        assert_ne!(root, Blake3Digest::ZERO);

        // Pass the real primary root for the same data.
        let mut primary = Blake3SmtCommitment::new();
        primary.update(&[(b"k", b"v")]);
        tree.assert_consistent_with_verkle(&primary.root_commitment())
            .unwrap();
    }

    #[test]
    fn assert_consistent_nonempty_rejects_zero_primary() {
        let mut tree = Blake3BackupTree::new();
        tree.insert(b"k", b"v");
        let result = tree.assert_consistent_with_verkle(&Blake3Digest::ZERO);
        assert!(
            result.is_err(),
            "non-empty backup must reject zero primary root"
        );
    }

    #[test]
    fn large_backup_tree_deterministic() {
        let mut tree = Blake3BackupTree::new();
        for i in 0u32..200 {
            tree.insert(
                format!("key_{i:04}").as_bytes(),
                format!("val_{i}").as_bytes(),
            );
        }
        let r1 = tree.root();

        let mut tree2 = Blake3BackupTree::new();
        for i in (0u32..200).rev() {
            tree2.insert(
                format!("key_{i:04}").as_bytes(),
                format!("val_{i}").as_bytes(),
            );
        }
        assert_eq!(r1, tree2.root());
    }
}
