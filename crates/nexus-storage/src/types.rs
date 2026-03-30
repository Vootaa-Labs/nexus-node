// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Core storage types: column family names, key encodings, and write operations.
//!
//! All column family names and key formats are **FROZEN-2**:
//! changes require an RFC and performance-impact proof.

use nexus_primitives::{AccountAddress, Blake3Digest, CommitSequence, ShardId};
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
    /// Block → transaction index: `CommitSequence (8B BE) → Vec<TxDigest> (BCS)`.
    ///
    /// Enables batch receipt retrieval by block sequence number.
    /// FROZEN-2 addition (v0.1.15).
    BlockTxIndex,
    /// Contract events: multi-key index for event queries.
    ///
    /// Three key patterns share this CF:
    /// - Primary:     `e:<block_seq 8B><tx_index 4B><event_seq 4B>` → `ContractEvent (BCS)`
    /// - By-contract: `c:<emitter 32B><block_seq 8B><event_seq 4B>` → `(empty)`
    /// - By-type:     `t:<type_hash 32B><block_seq 8B><event_seq 4B>` → `(empty)`
    ///
    /// FROZEN-2 addition (v0.1.15).
    Events,
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
            Self::BlockTxIndex => "cf_block_tx_index",
            Self::Events => "cf_events",
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
            Self::BlockTxIndex,
            Self::Events,
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

/// Key encoder for the `CF_EVENTS` column family.
///
/// Three key patterns are used to support different query access paths:
/// - **Primary** (`e:` prefix): ordered by block, then tx index, then event sequence.
/// - **By-contract** (`c:` prefix): ordered by emitter address, then block, then event sequence.
/// - **By-type** (`t:` prefix): ordered by event type hash, then block, then event sequence.
///
/// Secondary keys (`c:`, `t:`) store empty values — lookups retrieve the event
/// from the primary key.
pub struct EventKey;

impl EventKey {
    /// Primary key: `b'e' || block_seq(8B BE) || tx_index(4B BE) || event_seq(4B BE)` = 17 bytes.
    pub fn primary(block_seq: CommitSequence, tx_index: u32, event_seq: u32) -> Vec<u8> {
        let mut buf = Vec::with_capacity(17);
        buf.push(b'e');
        buf.extend_from_slice(&block_seq.0.to_be_bytes());
        buf.extend_from_slice(&tx_index.to_be_bytes());
        buf.extend_from_slice(&event_seq.to_be_bytes());
        buf
    }

    /// By-contract key: `b'c' || emitter(32B) || block_seq(8B BE) || event_seq(4B BE)` = 45 bytes.
    pub fn by_contract(
        emitter: &AccountAddress,
        block_seq: CommitSequence,
        event_seq: u32,
    ) -> Vec<u8> {
        let mut buf = Vec::with_capacity(45);
        buf.push(b'c');
        buf.extend_from_slice(emitter.as_ref());
        buf.extend_from_slice(&block_seq.0.to_be_bytes());
        buf.extend_from_slice(&event_seq.to_be_bytes());
        buf
    }

    /// By-type key: `b't' || type_hash(32B) || block_seq(8B BE) || event_seq(4B BE)` = 45 bytes.
    pub fn by_type(type_hash: &Blake3Digest, block_seq: CommitSequence, event_seq: u32) -> Vec<u8> {
        let mut buf = Vec::with_capacity(45);
        buf.push(b't');
        buf.extend_from_slice(type_hash.as_ref());
        buf.extend_from_slice(&block_seq.0.to_be_bytes());
        buf.extend_from_slice(&event_seq.to_be_bytes());
        buf
    }

    /// Returns the 1-byte prefix for primary keys (`b'e'`).
    pub fn primary_prefix() -> &'static [u8] {
        b"e"
    }

    /// Returns the 33-byte prefix for scanning events by a specific contract.
    pub fn contract_prefix(emitter: &AccountAddress) -> Vec<u8> {
        let mut buf = Vec::with_capacity(33);
        buf.push(b'c');
        buf.extend_from_slice(emitter.as_ref());
        buf
    }

    /// Returns the 33-byte prefix for scanning events by a specific type hash.
    pub fn type_prefix(type_hash: &Blake3Digest) -> Vec<u8> {
        let mut buf = Vec::with_capacity(33);
        buf.push(b't');
        buf.extend_from_slice(type_hash.as_ref());
        buf
    }
}

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
        assert_eq!(ColumnFamily::BlockTxIndex.as_str(), "cf_block_tx_index");
        assert_eq!(ColumnFamily::Events.as_str(), "cf_events");
    }

    #[test]
    fn all_column_families_count() {
        assert_eq!(ColumnFamily::all().len(), 14);
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

    #[test]
    fn event_key_primary_length() {
        let key = EventKey::primary(CommitSequence(1), 0, 0);
        assert_eq!(key.len(), 17);
        assert_eq!(key[0], b'e');
    }

    #[test]
    fn event_key_by_contract_length() {
        let addr = AccountAddress([0xAA; 32]);
        let key = EventKey::by_contract(&addr, CommitSequence(1), 0);
        assert_eq!(key.len(), 45);
        assert_eq!(key[0], b'c');
    }

    #[test]
    fn event_key_by_type_length() {
        let hash = Blake3Digest::from_bytes([0xBB; 32]);
        let key = EventKey::by_type(&hash, CommitSequence(1), 0);
        assert_eq!(key.len(), 45);
        assert_eq!(key[0], b't');
    }

    #[test]
    fn event_key_primary_ordering() {
        // Keys for block 1 sort before block 2.
        let k1 = EventKey::primary(CommitSequence(1), 0, 0);
        let k2 = EventKey::primary(CommitSequence(2), 0, 0);
        assert!(k1 < k2);
        // Within same block, tx_index orders.
        let k3 = EventKey::primary(CommitSequence(1), 1, 0);
        assert!(k1 < k3);
    }

    #[test]
    fn event_key_primary_embeds_components() {
        let key = EventKey::primary(CommitSequence(256), 3, 7);
        // Prefix
        assert_eq!(key[0], b'e');
        // block_seq 256 in big-endian
        let seq = u64::from_be_bytes(key[1..9].try_into().unwrap());
        assert_eq!(seq, 256);
        // tx_index
        let tx = u32::from_be_bytes(key[9..13].try_into().unwrap());
        assert_eq!(tx, 3);
        // event_seq
        let ev = u32::from_be_bytes(key[13..17].try_into().unwrap());
        assert_eq!(ev, 7);
    }

    #[test]
    fn event_key_contract_prefix_scopes_emitter() {
        let addr_a = AccountAddress([0xAA; 32]);
        let addr_b = AccountAddress([0xBB; 32]);
        let k1 = EventKey::by_contract(&addr_a, CommitSequence(1), 0);
        let k2 = EventKey::by_contract(&addr_b, CommitSequence(1), 0);
        // Different emitter → different prefix → disjoint scan range.
        let prefix_a = EventKey::contract_prefix(&addr_a);
        let prefix_b = EventKey::contract_prefix(&addr_b);
        assert!(k1.starts_with(&prefix_a));
        assert!(!k1.starts_with(&prefix_b));
        assert!(k2.starts_with(&prefix_b));
        assert!(!k2.starts_with(&prefix_a));
    }

    #[test]
    fn event_key_type_prefix_scopes_hash() {
        let hash_a = Blake3Digest::from_bytes([0xAA; 32]);
        let hash_b = Blake3Digest::from_bytes([0xBB; 32]);
        let k1 = EventKey::by_type(&hash_a, CommitSequence(1), 0);
        let prefix_a = EventKey::type_prefix(&hash_a);
        let prefix_b = EventKey::type_prefix(&hash_b);
        assert!(k1.starts_with(&prefix_a));
        assert!(!k1.starts_with(&prefix_b));
    }
}
