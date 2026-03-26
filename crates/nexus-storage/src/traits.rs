//! Core storage trait contracts.
//!
//! # Stability levels
//!
//! | Trait | Level | Change cost |
//! |-------|-------|-------------|
//! | [`StateStorage`] | **SEALED** | Hard-fork (FROZEN-3 path) |
//! | [`WriteBatchOps`] | **SEALED** | Hard-fork |
//! | [`StateCommitment`] | **SEALED** | Hard-fork |
//! | [`BackupHashTree`] | **STABLE** | RFC required (FROZEN-2) |
//!
//! All traits use native `async fn` in trait (stable since Rust 1.75).

use crate::error::StorageError;

// ── WriteBatchOps ────────────────────────────────────────────────────────────

/// Atomic batch of write operations.
///
/// All mutations go through a `WriteBatch` — no single-key puts are allowed.
/// This ensures atomicity and enables backpressure via [`size_hint`](WriteBatchOps::size_hint).
pub trait WriteBatchOps: Send {
    /// Insert or update a key-value pair (default column family).
    fn put(&mut self, key: Vec<u8>, value: Vec<u8>) -> &mut Self;

    /// Delete a key (default column family).
    fn delete(&mut self, key: Vec<u8>) -> &mut Self;

    /// Insert a key-value pair into a specific column family.
    fn put_cf(&mut self, cf: &str, key: Vec<u8>, value: Vec<u8>) -> &mut Self;

    /// Delete a key from a specific column family.
    fn delete_cf(&mut self, cf: &str, key: Vec<u8>) -> &mut Self;

    /// Number of operations accumulated so far.
    fn size_hint(&self) -> usize;
}

// ── StateStorage [SEALED] ────────────────────────────────────────────────────

/// Persistent key-value storage backend.
///
/// **SEALED**: changing this trait's signature requires a protocol hard-fork.
///
/// Two implementations are provided:
/// - [`RocksStore`](crate::rocks::RocksStore) — production (RocksDB)
/// - [`MemoryStore`](crate::memory::MemoryStore) — testing (BTreeMap)
///
/// # Key/Value encoding
///
/// Keys and values are opaque byte vectors (`Vec<u8>`). Higher layers
/// (execution, consensus) are responsible for BCS encoding/decoding
/// domain objects before passing them to storage.
pub trait StateStorage: Send + Sync + Clone + 'static {
    /// The write-batch type associated with this backend.
    type WriteBatch: WriteBatchOps;

    /// Retrieve a single value by key.
    ///
    /// Returns `Ok(None)` if the key does not exist.
    fn get(
        &self,
        cf: &str,
        key: &[u8],
    ) -> impl std::future::Future<Output = Result<Option<Vec<u8>>, StorageError>> + Send;

    /// Scan a key range `[start, end)` within a column family.
    ///
    /// Returns an ordered iterator of `(key, value)` pairs.
    /// For async contexts, callers should use `spawn_blocking` if the
    /// scan is expected to be large.
    #[allow(clippy::type_complexity)]
    fn scan(
        &self,
        cf: &str,
        start: &[u8],
        end: &[u8],
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StorageError>;

    /// Atomically apply a write batch.
    fn write_batch(
        &self,
        batch: Self::WriteBatch,
    ) -> impl std::future::Future<Output = Result<(), StorageError>> + Send;

    /// Create a new empty write batch for this backend.
    fn new_batch(&self) -> Self::WriteBatch;

    /// Create a point-in-time snapshot.
    ///
    /// The returned store reflects all data written before the call
    /// and is unaffected by subsequent writes to the original.
    fn snapshot(&self) -> impl StateStorage<WriteBatch = Self::WriteBatch>;

    /// Synchronous single-key get for non-async contexts (e.g. StateView, RPC handlers).
    fn get_sync(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError>;

    /// Synchronous single-key put for non-async contexts (e.g. faucet mint).
    fn put_sync(&self, cf: &str, key: Vec<u8>, value: Vec<u8>) -> Result<(), StorageError>;
}

// ── StateCommitment [SEALED] ─────────────────────────────────────────────────

/// State commitment (sorted Merkle tree) interface.
///
/// **SEALED**: changing this trait requires a protocol hard-fork.
///
/// Current implementation: BLAKE3 binary sorted Merkle tree.
/// The domain separator `VERKLE_LEAF` is **FROZEN-3**.
pub trait StateCommitment: Send + Sync + 'static {
    /// The commitment type (e.g., Merkle root).
    type Commitment: Clone + serde::Serialize + for<'de> serde::Deserialize<'de>;
    /// The proof type (e.g., opening proof).
    type Proof: Clone + serde::Serialize + for<'de> serde::Deserialize<'de>;
    /// Error type for commitment operations.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Batch-update the tree with new key-value pairs.
    fn update(&mut self, kv_pairs: &[(&[u8], &[u8])]);

    /// Delete a key from the tree.
    fn delete(&mut self, key: &[u8]);

    /// Compute the current root commitment.
    fn root_commitment(&self) -> Self::Commitment;

    /// Generate an opening proof for a single key.
    #[allow(clippy::type_complexity)]
    fn prove_key(&self, key: &[u8]) -> Result<(Option<Vec<u8>>, Self::Proof), Self::Error>;

    /// Verify an opening proof against a known root.
    fn verify_proof(
        root: &Self::Commitment,
        key: &[u8],
        value: Option<&[u8]>,
        proof: &Self::Proof,
    ) -> Result<(), Self::Error>;

    /// Generate proofs for multiple keys.
    #[allow(clippy::type_complexity)]
    fn prove_keys(
        &self,
        keys: &[&[u8]],
    ) -> Result<Vec<(Option<Vec<u8>>, Self::Proof)>, Self::Error>;
}

// ── BackupHashTree [STABLE] ──────────────────────────────────────────────────

/// BLAKE3-based backup Merkle tree for post-quantum safety.
///
/// **STABLE** (FROZEN-2): changes require an RFC.
///
/// Maintained in parallel with the primary state commitment tree.
/// At each epoch boundary, `assert_consistent_with_verkle` must pass
/// or block production halts.
pub trait BackupHashTree: Send + Sync + 'static {
    /// Digest type (typically [`Blake3Digest`](nexus_primitives::Blake3Digest)).
    type Digest: Clone + PartialEq + std::fmt::Debug;
    /// Error type for tree operations.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Insert or update a key-value pair in the backup tree.
    fn insert(&mut self, key: &[u8], value: &[u8]);

    /// Delete a key from the backup tree.
    fn delete(&mut self, key: &[u8]);

    /// Compute the current Merkle root.
    fn root(&self) -> Self::Digest;

    /// Assert that this tree's root matches the primary commitment tree
    /// root (rehashed through BLAKE3).
    ///
    /// # Errors
    ///
    /// Returns `Err` on mismatch — callers **must** halt block production.
    fn assert_consistent_with_verkle(
        &self,
        verkle_root_blake3: &Self::Digest,
    ) -> Result<(), Self::Error>;
}
