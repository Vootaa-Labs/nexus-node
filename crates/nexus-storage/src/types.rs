//! Core storage types: column family names, key encodings, and write operations.
//!
//! All column family names and key formats are **FROZEN-2**:
//! changes require an RFC and performance-impact proof.

use nexus_primitives::{AccountAddress, ShardId};
use serde::{Deserialize, Serialize};

// ── Column Families (FROZEN-2) ───────────────────────────────────────────────

/// Column family names for the RocksDB instance.
///
/// These are **FROZEN-2** — changing them requires a network-wide migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColumnFamily {
    /// Block metadata: `CommitSequence → BlockHeader (BCS)`.
    Blocks,
    /// Raw transactions: `TxDigest → SignedTransaction (BCS)`.
    Transactions,
    /// Transaction receipts: `TxDigest → TransactionReceipt (BCS, Zstd)`.
    Receipts,
    /// Global state: `AccountKey|ResourceKey → AccountState (BCS)`.
    State,
    /// Consensus certificates: `CertDigest → NarwhalCertificate (BCS)`.
    Certificates,
    /// Batch payloads: `BatchDigest → Vec<SignedTransaction> (BCS)`.
    ///
    /// FROZEN-2 addition (v0.1.6): required for cold-restart recovery of
    /// the consensus→execution bridge.  See PERSIST-CRITICAL-2.
    Batches,
    /// Agent sessions: `SessionId (Blake3Digest) → AgentSession (BCS)`.
    Sessions,
    /// Provenance records: `ProvenanceId (Blake3Digest) → ProvenanceRecord (BCS)`.
    /// Secondary indexes use composite prefix keys for by_agent/by_session/by_capability lookups.
    Provenance,
    /// Commitment metadata: active tree version and root/leaf counters.
    CommitmentMeta,
    /// Commitment leaves: ordered leaves plus key→index lookup records.
    CommitmentLeaves,
    /// Commitment internal nodes: versioned Merkle node hashes by `(level, index)`.
    CommitmentNodes,
    /// HTLC lock records: `LockDigest (Blake3Digest) → HtlcLockRecord (BCS)`.
    ///
    /// Stores pending, claimed, and refunded HTLC locks for cross-shard
    /// atomic transfers.  Both source-shard locks and target-shard claims
    /// reference the same lock digest key.
    HtlcLocks,
}

impl ColumnFamily {
    /// The string name used by RocksDB.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Blocks => "cf_blocks",
            Self::Transactions => "cf_transactions",
            Self::Receipts => "cf_receipts",
            Self::State => "cf_state",
            Self::Certificates => "cf_certificates",
            Self::Batches => "cf_batches",
            Self::Sessions => "cf_sessions",
            Self::Provenance => "cf_provenance",
            Self::CommitmentMeta => "cf_commitment_meta",
            Self::CommitmentLeaves => "cf_commitment_leaves",
            Self::CommitmentNodes => "cf_commitment_nodes",
            Self::HtlcLocks => "cf_htlc_locks",
        }
    }

    /// All column families in declaration order.
    pub fn all() -> &'static [ColumnFamily] {
        &[
            Self::Blocks,
            Self::Transactions,
            Self::Receipts,
            Self::State,
            Self::Certificates,
            Self::Batches,
            Self::Sessions,
            Self::Provenance,
            Self::CommitmentMeta,
            Self::CommitmentLeaves,
            Self::CommitmentNodes,
            Self::HtlcLocks,
        ]
    }
}

// ── Key Types (FROZEN-2 encoding) ────────────────────────────────────────────

/// Key for the `CF_STATE` column family — identifies an account's base state.
///
/// Wire format: `[shard_id: 2 bytes LE] [address: 32 bytes]` = **34 bytes**.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AccountKey {
    /// Shard prefix — enables shard-scoped range scans.
    pub shard_id: ShardId,
    /// The account address.
    pub address: AccountAddress,
}

impl AccountKey {
    /// Encode to the canonical 34-byte wire format.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(34);
        buf.extend_from_slice(&self.shard_id.0.to_le_bytes());
        buf.extend_from_slice(self.address.as_ref());
        buf
    }

    /// Decode from 34-byte wire format.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the input is not exactly 34 bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, crate::error::StorageError> {
        if bytes.len() != 34 {
            return Err(crate::error::StorageError::KeyCodec(format!(
                "AccountKey expects 34 bytes, got {}",
                bytes.len()
            )));
        }
        let shard_id = ShardId(u16::from_le_bytes([bytes[0], bytes[1]]));
        let mut addr_bytes = [0u8; 32];
        addr_bytes.copy_from_slice(&bytes[2..34]);
        Ok(Self {
            shard_id,
            address: AccountAddress(addr_bytes),
        })
    }
}

// NOTE: AsRef<[u8]> was removed — composite keys cannot cheaply reference a
// contiguous byte slice without an internal buffer. Callers should use
// `to_bytes()` to obtain the wire-format bytes. (M-001 remediation)

/// Key for the `CF_STATE` column family — identifies a resource under an account.
///
/// Wire format: `[shard_id: 2 bytes LE] [address: 32 bytes] [type_hash: 32 bytes]` = **66 bytes**.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ResourceKey {
    /// Shard prefix — enables shard-scoped range scans.
    pub shard_id: ShardId,
    /// The owning account address.
    pub address: AccountAddress,
    /// BLAKE3 hash of the Move TypeTag.
    pub type_hash: [u8; 32],
}

impl ResourceKey {
    /// Encode to the canonical 66-byte wire format.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(66);
        buf.extend_from_slice(&self.shard_id.0.to_le_bytes());
        buf.extend_from_slice(self.address.as_ref());
        buf.extend_from_slice(&self.type_hash);
        buf
    }

    /// Decode from 66-byte wire format.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the input is not exactly 66 bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, crate::error::StorageError> {
        if bytes.len() != 66 {
            return Err(crate::error::StorageError::KeyCodec(format!(
                "ResourceKey expects 66 bytes, got {}",
                bytes.len()
            )));
        }
        let shard_id = ShardId(u16::from_le_bytes([bytes[0], bytes[1]]));
        let mut addr_bytes = [0u8; 32];
        addr_bytes.copy_from_slice(&bytes[2..34]);
        let mut type_hash = [0u8; 32];
        type_hash.copy_from_slice(&bytes[34..66]);
        Ok(Self {
            shard_id,
            address: AccountAddress(addr_bytes),
            type_hash,
        })
    }
}

/// A single write operation within a batch.
#[derive(Debug, Clone)]
pub enum WriteOp {
    /// Insert or update a key-value pair.
    Put {
        /// Column family target.
        cf: ColumnFamily,
        /// The key bytes.
        key: Vec<u8>,
        /// The value bytes.
        value: Vec<u8>,
    },
    /// Delete a key.
    Delete {
        /// Column family target.
        cf: ColumnFamily,
        /// The key bytes.
        key: Vec<u8>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_family_names_are_frozen() {
        assert_eq!(ColumnFamily::Blocks.as_str(), "cf_blocks");
        assert_eq!(ColumnFamily::Transactions.as_str(), "cf_transactions");
        assert_eq!(ColumnFamily::Receipts.as_str(), "cf_receipts");
        assert_eq!(ColumnFamily::State.as_str(), "cf_state");
        assert_eq!(ColumnFamily::Certificates.as_str(), "cf_certificates");
        assert_eq!(ColumnFamily::Batches.as_str(), "cf_batches");
        assert_eq!(ColumnFamily::Sessions.as_str(), "cf_sessions");
        assert_eq!(ColumnFamily::Provenance.as_str(), "cf_provenance");
        assert_eq!(ColumnFamily::CommitmentMeta.as_str(), "cf_commitment_meta");
        assert_eq!(
            ColumnFamily::CommitmentLeaves.as_str(),
            "cf_commitment_leaves"
        );
        assert_eq!(
            ColumnFamily::CommitmentNodes.as_str(),
            "cf_commitment_nodes"
        );
        assert_eq!(ColumnFamily::HtlcLocks.as_str(), "cf_htlc_locks");
    }

    #[test]
    fn all_column_families_count() {
        assert_eq!(ColumnFamily::all().len(), 12);
    }

    #[test]
    fn account_key_roundtrip() {
        let key = AccountKey {
            shard_id: ShardId(42),
            address: AccountAddress([0xAB; 32]),
        };
        let bytes = key.to_bytes();
        assert_eq!(bytes.len(), 34);
        let decoded = AccountKey::from_bytes(&bytes).unwrap();
        assert_eq!(key, decoded);
    }

    #[test]
    fn account_key_wrong_length() {
        assert!(AccountKey::from_bytes(&[0u8; 10]).is_err());
    }

    #[test]
    fn resource_key_roundtrip() {
        let key = ResourceKey {
            shard_id: ShardId(7),
            address: AccountAddress([0xCD; 32]),
            type_hash: [0xEF; 32],
        };
        let bytes = key.to_bytes();
        assert_eq!(bytes.len(), 66);
        let decoded = ResourceKey::from_bytes(&bytes).unwrap();
        assert_eq!(key, decoded);
    }

    #[test]
    fn resource_key_wrong_length() {
        assert!(ResourceKey::from_bytes(&[0u8; 10]).is_err());
    }
}
