// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! BLAKE3 Sorted Merkle Tree — first implementation of [`StateCommitment`].
//!
//! Uses a sorted map of key→value entries. The commitment root is computed
//! as a binary Merkle tree over `BLAKE3(domain ‖ key ‖ value)` leaves in
//! lexicographic key order. Opening proofs contain the sibling hashes
//! required to reconstruct the path from the target leaf to the root.
//!
//! Domain separator: `nexus::storage::verkle::leaf::v1` (**FROZEN-3** — byte value must not change).

use std::collections::BTreeMap;

use nexus_primitives::Blake3Digest;
use serde::{Deserialize, Serialize};

use crate::error::StorageError;
use crate::traits::StateCommitment;

// ── Domain Constants ────────────────────────────────────────────────────

/// Domain separator for leaf hashing (matches `nexus-crypto/domains.rs`).
const LEAF_DOMAIN: &[u8] = b"nexus::storage::verkle::leaf::v1";

/// Domain separator for internal Merkle nodes.
const NODE_DOMAIN: &[u8] = b"nexus::storage::commitment::node::v1";

/// Domain separator for the overall state root.
const ROOT_DOMAIN: &[u8] = b"nexus::storage::state::root::v1";

// ── Proof Types ─────────────────────────────────────────────────────────

/// Witness for a concrete leaf used by inclusion or exclusion proofs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MerkleNeighborProof {
    /// Raw key of the witnessed leaf.
    pub key: Vec<u8>,
    /// Raw value of the witnessed leaf.
    pub value: Vec<u8>,
    /// Index of the witnessed leaf within the sorted leaf array.
    pub leaf_index: u64,
    /// Sibling hashes from leaf level up to the root (bottom-up order).
    pub siblings: Vec<Blake3Digest>,
}

/// Merkle opening proof for a single key.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MerkleProofKind {
    /// The queried key exists in the tree.
    Inclusion,
    /// The queried key is absent and bounded by neighbouring leaves.
    Exclusion,
}

/// Merkle opening proof for a single key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "proof_type", rename_all = "snake_case")]
pub enum MerkleProof {
    /// Standard inclusion proof for an existing key.
    Inclusion {
        /// Index of the leaf within the sorted leaf array.
        leaf_index: u64,
        /// Total number of leaves at the time of proof generation.
        leaf_count: u64,
        /// Sibling hashes from leaf level up to the root (bottom-up order).
        siblings: Vec<Blake3Digest>,
    },
    /// Sound exclusion proof for a missing key.
    Exclusion {
        /// Total number of leaves at the time of proof generation.
        leaf_count: u64,
        /// Immediate predecessor leaf, if one exists.
        left_neighbor: Option<MerkleNeighborProof>,
        /// Immediate successor leaf, if one exists.
        right_neighbor: Option<MerkleNeighborProof>,
    },
}

impl MerkleProof {
    /// Return the proof kind.
    pub fn proof_kind(&self) -> MerkleProofKind {
        match self {
            Self::Inclusion { .. } => MerkleProofKind::Inclusion,
            Self::Exclusion { .. } => MerkleProofKind::Exclusion,
        }
    }

    /// Return the total leaf count recorded in the proof.
    pub fn leaf_count(&self) -> u64 {
        match self {
            Self::Inclusion { leaf_count, .. } | Self::Exclusion { leaf_count, .. } => *leaf_count,
        }
    }
}

/// The root commitment: a single 32-byte BLAKE3 digest.
pub type CommitmentRoot = Blake3Digest;

// ── Blake3SmtCommitment ─────────────────────────────────────────────────

/// BLAKE3 Sorted Merkle Tree implementing [`StateCommitment`].
///
/// Maintains a `BTreeMap<Vec<u8>, Vec<u8>>` as the authoritative state.
/// The root commitment is derived by building a binary Merkle tree over
/// the sorted leaf hashes; proofs are standard Merkle paths.
pub struct Blake3SmtCommitment {
    entries: BTreeMap<Vec<u8>, Vec<u8>>,
}

impl Blake3SmtCommitment {
    /// Create an empty commitment tree.
    pub fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }

    /// Number of entries in the tree.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the tree is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Compute sorted leaf hashes for the current state.
    fn leaf_hashes(&self) -> Vec<Blake3Digest> {
        self.entries.iter().map(|(k, v)| hash_leaf(k, v)).collect()
    }

    fn sorted_entries(&self) -> Vec<(&Vec<u8>, &Vec<u8>)> {
        self.entries.iter().collect()
    }
}

/// Return the domain-separated canonical empty-tree root.
///
/// This is the commitment root for a tree with zero entries. All code
/// paths that need to express "no state" must use this value instead of
/// `Blake3Digest::ZERO` or `[0u8; 32]`.
pub fn canonical_empty_root() -> Blake3Digest {
    merkle_root_from_leaves(&[])
}

impl Default for Blake3SmtCommitment {
    fn default() -> Self {
        Self::new()
    }
}

impl StateCommitment for Blake3SmtCommitment {
    type Commitment = CommitmentRoot;
    type Proof = MerkleProof;
    type Error = StorageError;

    fn update(&mut self, kv_pairs: &[(&[u8], &[u8])]) {
        for (key, value) in kv_pairs {
            self.entries.insert(key.to_vec(), value.to_vec());
        }
    }

    fn delete(&mut self, key: &[u8]) {
        self.entries.remove(key);
    }

    fn root_commitment(&self) -> CommitmentRoot {
        let leaves = self.leaf_hashes();
        merkle_root_from_leaves(&leaves)
    }

    fn prove_key(&self, key: &[u8]) -> Result<(Option<Vec<u8>>, MerkleProof), StorageError> {
        let sorted_entries = self.sorted_entries();
        let leaves: Vec<Blake3Digest> = sorted_entries
            .iter()
            .map(|(stored_key, stored_value)| hash_leaf(stored_key, stored_value))
            .collect();
        let leaf_count = leaves.len() as u64;

        // Find the key's position in sorted order.
        let pos = sorted_entries.binary_search_by(|(stored_key, _)| stored_key.as_slice().cmp(key));

        match pos {
            Ok(idx) => {
                // Key exists — inclusion proof.
                let value = self.entries.get(key).cloned();
                let siblings = merkle_path(&leaves, idx);
                Ok((
                    value,
                    MerkleProof::Inclusion {
                        leaf_index: idx as u64,
                        leaf_count,
                        siblings,
                    },
                ))
            }
            Err(insert_idx) => {
                // Key does not exist — exclusion proof.
                if leaves.is_empty() {
                    return Ok((
                        None,
                        MerkleProof::Exclusion {
                            leaf_count: 0,
                            left_neighbor: None,
                            right_neighbor: None,
                        },
                    ));
                }

                let left_neighbor = insert_idx
                    .checked_sub(1)
                    .map(|idx| build_neighbor_proof(&sorted_entries, &leaves, idx));
                let right_neighbor = if insert_idx < sorted_entries.len() {
                    Some(build_neighbor_proof(&sorted_entries, &leaves, insert_idx))
                } else {
                    None
                };

                Ok((
                    None,
                    MerkleProof::Exclusion {
                        leaf_count,
                        left_neighbor,
                        right_neighbor,
                    },
                ))
            }
        }
    }

    fn verify_proof(
        root: &CommitmentRoot,
        key: &[u8],
        value: Option<&[u8]>,
        proof: &MerkleProof,
    ) -> Result<(), StorageError> {
        match (proof, value) {
            (
                MerkleProof::Inclusion {
                    leaf_index,
                    leaf_count,
                    siblings,
                },
                Some(val),
            ) => {
                if siblings.len() != expected_path_len(*leaf_index as usize, *leaf_count as usize) {
                    return Err(invalid_proof("invalid inclusion sibling path length"));
                }
                let computed_root = reconstruct_root(
                    hash_leaf(key, val),
                    *leaf_index as usize,
                    *leaf_count as usize,
                    siblings,
                );
                if *leaf_count == 0 || *leaf_index >= *leaf_count {
                    return Err(invalid_proof("invalid inclusion leaf position"));
                }
                if computed_root != *root {
                    return Err(invalid_proof(
                        "proof verification failed: computed root does not match",
                    ));
                }
                Ok(())
            }
            (MerkleProof::Inclusion { .. }, None) => {
                Err(invalid_proof("inclusion proof requires a value"))
            }
            (
                MerkleProof::Exclusion {
                    leaf_count,
                    left_neighbor,
                    right_neighbor,
                },
                None,
            ) => verify_exclusion_proof(root, key, *leaf_count, left_neighbor, right_neighbor),
            (MerkleProof::Exclusion { .. }, Some(_)) => {
                Err(invalid_proof("exclusion proof must not include a value"))
            }
        }
    }

    fn prove_keys(
        &self,
        keys: &[&[u8]],
    ) -> Result<Vec<(Option<Vec<u8>>, MerkleProof)>, StorageError> {
        keys.iter().map(|k| self.prove_key(k)).collect()
    }
}

// ── Hashing helpers ─────────────────────────────────────────────────────

/// Hash a single leaf: `BLAKE3(LEAF_DOMAIN ‖ key_len ‖ key ‖ value)`.
pub(crate) fn hash_leaf(key: &[u8], value: &[u8]) -> Blake3Digest {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&(LEAF_DOMAIN.len() as u32).to_le_bytes());
    hasher.update(LEAF_DOMAIN);
    hasher.update(&(key.len() as u32).to_le_bytes());
    hasher.update(key);
    hasher.update(value);
    Blake3Digest::from_bytes(*hasher.finalize().as_bytes())
}

/// Hash two child nodes into a parent: `BLAKE3(NODE_DOMAIN ‖ left ‖ right)`.
pub(crate) fn hash_node(left: &Blake3Digest, right: &Blake3Digest) -> Blake3Digest {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&(NODE_DOMAIN.len() as u32).to_le_bytes());
    hasher.update(NODE_DOMAIN);
    hasher.update(left.as_bytes());
    hasher.update(right.as_bytes());
    Blake3Digest::from_bytes(*hasher.finalize().as_bytes())
}

/// Compute the Merkle root from an ordered list of leaf hashes.
///
/// Uses bottom-up binary tree construction with duplication padding
/// for odd counts at each level.
pub(crate) fn merkle_root_from_leaves(leaves: &[Blake3Digest]) -> Blake3Digest {
    if leaves.is_empty() {
        // Domain-separated empty root.
        let mut hasher = blake3::Hasher::new();
        hasher.update(&(ROOT_DOMAIN.len() as u32).to_le_bytes());
        hasher.update(ROOT_DOMAIN);
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
            .map(|pair| hash_node(&pair[0], &pair[1]))
            .collect();
    }
    current[0]
}

/// Compute the Merkle path (sibling hashes) for a leaf at `index` within
/// a tree of `leaves.len()` leaves. Returns siblings bottom-up.
fn merkle_path(leaves: &[Blake3Digest], index: usize) -> Vec<Blake3Digest> {
    if leaves.len() <= 1 {
        return vec![];
    }

    let mut siblings = Vec::new();
    let mut current: Vec<Blake3Digest> = leaves.to_vec();
    let mut idx = index;

    while current.len() > 1 {
        if current.len() % 2 != 0 {
            let last = current[current.len() - 1];
            current.push(last);
        }
        // The sibling of idx is idx^1 (flip the least significant bit).
        let sibling_idx = idx ^ 1;
        siblings.push(current[sibling_idx]);
        // Build next level.
        current = current
            .chunks(2)
            .map(|pair| hash_node(&pair[0], &pair[1]))
            .collect();
        idx /= 2;
    }

    siblings
}

fn build_neighbor_proof(
    sorted_entries: &[(&Vec<u8>, &Vec<u8>)],
    leaves: &[Blake3Digest],
    index: usize,
) -> MerkleNeighborProof {
    let (key, value) = sorted_entries[index];
    MerkleNeighborProof {
        key: key.clone(),
        value: value.clone(),
        leaf_index: index as u64,
        siblings: merkle_path(leaves, index),
    }
}

fn verify_exclusion_proof(
    root: &CommitmentRoot,
    key: &[u8],
    leaf_count: u64,
    left_neighbor: &Option<MerkleNeighborProof>,
    right_neighbor: &Option<MerkleNeighborProof>,
) -> Result<(), StorageError> {
    if leaf_count == 0 {
        if left_neighbor.is_some() || right_neighbor.is_some() {
            return Err(invalid_proof(
                "empty-tree exclusion proof must not include neighbours",
            ));
        }
        return if *root == merkle_root_from_leaves(&[]) {
            Ok(())
        } else {
            Err(invalid_proof("empty-tree exclusion proof root mismatch"))
        };
    }

    if left_neighbor.is_none() && right_neighbor.is_none() {
        return Err(invalid_proof(
            "non-empty exclusion proof requires at least one neighbour",
        ));
    }
    if let Some(left) = left_neighbor {
        verify_neighbor(root, leaf_count, left)?;
    }
    if let Some(right) = right_neighbor {
        verify_neighbor(root, leaf_count, right)?;
    }

    match (left_neighbor, right_neighbor) {
        (Some(left), Some(right)) => {
            if left.key >= right.key {
                return Err(invalid_proof(
                    "exclusion neighbours are not strictly ordered",
                ));
            }
            if left.leaf_index + 1 != right.leaf_index {
                return Err(invalid_proof(
                    "exclusion neighbours must be adjacent leaves",
                ));
            }
            if !(left.key.as_slice() < key && key < right.key.as_slice()) {
                return Err(invalid_proof("queried key is not within exclusion gap"));
            }
        }
        (Some(left), None) => {
            if left.leaf_index + 1 != leaf_count {
                return Err(invalid_proof(
                    "right-open exclusion proof must end at the last leaf",
                ));
            }
            if key <= left.key.as_slice() {
                return Err(invalid_proof(
                    "queried key must be greater than the last leaf",
                ));
            }
        }
        (None, Some(right)) => {
            if right.leaf_index != 0 {
                return Err(invalid_proof(
                    "left-open exclusion proof must start at the first leaf",
                ));
            }
            if key >= right.key.as_slice() {
                return Err(invalid_proof(
                    "queried key must be smaller than the first leaf",
                ));
            }
        }
        (None, None) => {
            return Err(invalid_proof(
                "non-empty exclusion proof missing neighbours",
            ))
        }
    }

    Ok(())
}

fn verify_neighbor(
    root: &CommitmentRoot,
    leaf_count: u64,
    neighbor: &MerkleNeighborProof,
) -> Result<(), StorageError> {
    if neighbor.leaf_index >= leaf_count {
        return Err(invalid_proof("neighbour leaf index out of range"));
    }
    if neighbor.siblings.len()
        != expected_path_len(neighbor.leaf_index as usize, leaf_count as usize)
    {
        return Err(invalid_proof("invalid neighbour sibling path length"));
    }

    let computed_root = reconstruct_root(
        hash_leaf(&neighbor.key, &neighbor.value),
        neighbor.leaf_index as usize,
        leaf_count as usize,
        &neighbor.siblings,
    );
    if computed_root != *root {
        return Err(invalid_proof(
            "neighbour path does not reconstruct the commitment root",
        ));
    }

    Ok(())
}

fn invalid_proof(message: &str) -> StorageError {
    StorageError::StateCommitment(message.into())
}

fn expected_path_len(_index: usize, leaf_count: usize) -> usize {
    if leaf_count <= 1 {
        return 0;
    }

    let mut width = leaf_count;
    let mut depth = 0;

    while width > 1 {
        if width % 2 != 0 {
            width += 1;
        }
        depth += 1;
        width /= 2;
    }

    depth
}

/// Reconstruct the Merkle root from a leaf hash, its index, the total leaf
/// count, and the sibling path (bottom-up).
fn reconstruct_root(
    leaf: Blake3Digest,
    index: usize,
    leaf_count: usize,
    siblings: &[Blake3Digest],
) -> Blake3Digest {
    if leaf_count <= 1 {
        return leaf;
    }

    let mut current = leaf;
    let mut idx = index;

    for sibling in siblings {
        if idx % 2 == 0 {
            current = hash_node(&current, sibling);
        } else {
            current = hash_node(sibling, &current);
        }
        idx /= 2;
    }

    current
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_tree_root_deterministic() {
        let tree = Blake3SmtCommitment::new();
        let r1 = tree.root_commitment();
        let r2 = tree.root_commitment();
        assert_eq!(r1, r2);
        assert_ne!(r1, Blake3Digest::ZERO);
    }

    #[test]
    fn single_entry_commitment() {
        let mut tree = Blake3SmtCommitment::new();
        tree.update(&[(b"key1", b"val1")]);

        let root = tree.root_commitment();
        assert_ne!(root, Blake3Digest::ZERO);

        // Same entry produces same root.
        let mut tree2 = Blake3SmtCommitment::new();
        tree2.update(&[(b"key1", b"val1")]);
        assert_eq!(tree2.root_commitment(), root);
    }

    #[test]
    fn root_changes_on_update() {
        let mut tree = Blake3SmtCommitment::new();
        tree.update(&[(b"key1", b"val1")]);
        let r1 = tree.root_commitment();

        tree.update(&[(b"key2", b"val2")]);
        let r2 = tree.root_commitment();
        assert_ne!(r1, r2);
    }

    #[test]
    fn root_changes_on_delete() {
        let mut tree = Blake3SmtCommitment::new();
        tree.update(&[(b"key1", b"val1"), (b"key2", b"val2")]);
        let r1 = tree.root_commitment();

        tree.delete(b"key2");
        let r2 = tree.root_commitment();
        assert_ne!(r1, r2);
    }

    #[test]
    fn delete_restores_previous_root() {
        let mut tree = Blake3SmtCommitment::new();
        tree.update(&[(b"key1", b"val1")]);
        let r1 = tree.root_commitment();

        tree.update(&[(b"key2", b"val2")]);
        tree.delete(b"key2");
        assert_eq!(tree.root_commitment(), r1);
    }

    #[test]
    fn insertion_order_independent() {
        let mut t1 = Blake3SmtCommitment::new();
        t1.update(&[(b"alpha", b"1"), (b"beta", b"2"), (b"gamma", b"3")]);

        let mut t2 = Blake3SmtCommitment::new();
        t2.update(&[(b"gamma", b"3")]);
        t2.update(&[(b"alpha", b"1")]);
        t2.update(&[(b"beta", b"2")]);

        assert_eq!(t1.root_commitment(), t2.root_commitment());
    }

    #[test]
    fn prove_key_inclusion() {
        let mut tree = Blake3SmtCommitment::new();
        tree.update(&[(b"a", b"1"), (b"b", b"2"), (b"c", b"3")]);

        let root = tree.root_commitment();
        let (value, proof) = tree.prove_key(b"b").unwrap();

        assert_eq!(value, Some(b"2".to_vec()));
        assert_eq!(proof.proof_kind(), MerkleProofKind::Inclusion);
        assert_eq!(proof.leaf_count(), 3);

        // Verify succeeds.
        Blake3SmtCommitment::verify_proof(&root, b"b", Some(b"2"), &proof).unwrap();
    }

    #[test]
    fn prove_key_exclusion() {
        let mut tree = Blake3SmtCommitment::new();
        tree.update(&[(b"a", b"1"), (b"c", b"3")]);

        let root = tree.root_commitment();
        let (value, proof) = tree.prove_key(b"b").unwrap();

        assert!(value.is_none());
        assert_eq!(proof.proof_kind(), MerkleProofKind::Exclusion);

        // Exclusion proof verification.
        Blake3SmtCommitment::verify_proof(&root, b"b", None, &proof).unwrap();
    }

    #[test]
    fn prove_key_empty_tree() {
        let tree = Blake3SmtCommitment::new();
        let root = tree.root_commitment();
        let (value, proof) = tree.prove_key(b"missing").unwrap();

        assert!(value.is_none());
        assert_eq!(proof.proof_kind(), MerkleProofKind::Exclusion);
        assert_eq!(proof.leaf_count(), 0);

        Blake3SmtCommitment::verify_proof(&root, b"missing", None, &proof).unwrap();
    }

    #[test]
    fn inclusion_proof_rejects_wrong_value() {
        let mut tree = Blake3SmtCommitment::new();
        tree.update(&[(b"a", b"1"), (b"b", b"2"), (b"c", b"3")]);

        let root = tree.root_commitment();
        let (_value, proof) = tree.prove_key(b"b").unwrap();

        // Wrong value should fail verification.
        let result = Blake3SmtCommitment::verify_proof(&root, b"b", Some(b"WRONG"), &proof);
        assert!(result.is_err());
    }

    #[test]
    fn inclusion_proof_rejects_wrong_root() {
        let mut tree = Blake3SmtCommitment::new();
        tree.update(&[(b"key", b"val")]);

        let (_value, proof) = tree.prove_key(b"key").unwrap();

        let fake_root = Blake3Digest::ZERO;
        let result = Blake3SmtCommitment::verify_proof(&fake_root, b"key", Some(b"val"), &proof);
        assert!(result.is_err());
    }

    #[test]
    fn batch_prove_keys() {
        let mut tree = Blake3SmtCommitment::new();
        tree.update(&[(b"x", b"1"), (b"y", b"2"), (b"z", b"3")]);

        let root = tree.root_commitment();
        let results = tree.prove_keys(&[b"x", b"z", b"missing"]).unwrap();

        assert_eq!(results.len(), 3);
        assert_eq!(results[0].0, Some(b"1".to_vec()));
        assert_eq!(results[1].0, Some(b"3".to_vec()));
        assert!(results[2].0.is_none());

        // Verify each valid inclusion proof.
        Blake3SmtCommitment::verify_proof(&root, b"x", Some(b"1"), &results[0].1).unwrap();
        Blake3SmtCommitment::verify_proof(&root, b"z", Some(b"3"), &results[1].1).unwrap();
    }

    #[test]
    fn large_tree_proof_roundtrip() {
        let mut tree = Blake3SmtCommitment::new();
        let entries: Vec<(Vec<u8>, Vec<u8>)> = (0u32..100)
            .map(|i| {
                (
                    format!("key_{i:04}").into_bytes(),
                    format!("val_{i}").into_bytes(),
                )
            })
            .collect();

        let pairs: Vec<(&[u8], &[u8])> = entries
            .iter()
            .map(|(k, v)| (k.as_slice(), v.as_slice()))
            .collect();
        tree.update(&pairs);

        let root = tree.root_commitment();

        // Verify a sample of proofs.
        for i in [0, 25, 50, 75, 99] {
            let key = format!("key_{i:04}");
            let val = format!("val_{i}");
            let (value, proof) = tree.prove_key(key.as_bytes()).unwrap();
            assert_eq!(value, Some(val.as_bytes().to_vec()));
            Blake3SmtCommitment::verify_proof(&root, key.as_bytes(), Some(val.as_bytes()), &proof)
                .unwrap();
        }
    }

    #[test]
    fn exclusion_proof_rejects_swapped_neighbours() {
        let mut tree = Blake3SmtCommitment::new();
        tree.update(&[(b"a", b"1"), (b"c", b"3")]);

        let root = tree.root_commitment();
        let (_value, proof) = tree.prove_key(b"b").unwrap();
        let tampered = match proof {
            MerkleProof::Exclusion {
                leaf_count,
                left_neighbor: Some(left_neighbor),
                right_neighbor: Some(right_neighbor),
            } => MerkleProof::Exclusion {
                leaf_count,
                left_neighbor: Some(right_neighbor),
                right_neighbor: Some(left_neighbor),
            },
            _ => panic!("expected exclusion proof"),
        };

        let result = Blake3SmtCommitment::verify_proof(&root, b"b", None, &tampered);
        assert!(result.is_err());
    }

    #[test]
    fn exclusion_proof_rejects_tampered_sibling() {
        let mut tree = Blake3SmtCommitment::new();
        tree.update(&[(b"a", b"1"), (b"c", b"3"), (b"e", b"5")]);

        let root = tree.root_commitment();
        let (_value, proof) = tree.prove_key(b"d").unwrap();
        let tampered = match proof {
            MerkleProof::Exclusion {
                leaf_count,
                left_neighbor,
                right_neighbor,
            } => {
                let mut right_neighbor = right_neighbor.expect("right neighbour must exist");
                right_neighbor.siblings[0] = Blake3Digest::ZERO;
                MerkleProof::Exclusion {
                    leaf_count,
                    left_neighbor,
                    right_neighbor: Some(right_neighbor),
                }
            }
            _ => panic!("expected exclusion proof"),
        };

        let result = Blake3SmtCommitment::verify_proof(&root, b"d", None, &tampered);
        assert!(result.is_err());
    }

    #[test]
    fn exclusion_proof_handles_left_and_right_edges() {
        let mut tree = Blake3SmtCommitment::new();
        tree.update(&[(b"b", b"2"), (b"d", b"4")]);

        let root = tree.root_commitment();
        let (_left_value, left_proof) = tree.prove_key(b"a").unwrap();
        let (_right_value, right_proof) = tree.prove_key(b"z").unwrap();

        Blake3SmtCommitment::verify_proof(&root, b"a", None, &left_proof).unwrap();
        Blake3SmtCommitment::verify_proof(&root, b"z", None, &right_proof).unwrap();
    }
}
