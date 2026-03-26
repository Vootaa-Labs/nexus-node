// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! BLAKE3 domain separator constants for the Nexus protocol.
//!
//! **FROZEN-3**: These constants are protocol-level and changing them
//! requires a hard fork. Every signing and hashing operation **must**
//! use an appropriate domain separator to prevent cross-domain attacks.

/// Narwhal batch content hash domain.
pub const NARWHAL_BATCH: &[u8] = b"nexus::narwhal::batch::v1";

/// Narwhal certificate hash domain.
pub const NARWHAL_CERT: &[u8] = b"nexus::narwhal::certificate::v1";

/// Shoal++ vote signing domain.
pub const SHOAL_VOTE: &[u8] = b"nexus::shoal::vote::v1";

/// Shoal++ anchor signing domain.
pub const SHOAL_ANCHOR: &[u8] = b"nexus::shoal::anchor::v1";

/// User transaction signing / hashing domain.
pub const USER_TX: &[u8] = b"nexus::execution::transaction::v1";

/// State commitment leaf hashing domain (BLAKE3 sorted Merkle tree).
///
/// The byte value is **FROZEN-3** and must not change.
pub const VERKLE_LEAF: &[u8] = b"nexus::storage::verkle::leaf::v1";

/// P2P network handshake signing domain.
pub const P2P_HANDSHAKE: &[u8] = b"nexus::network::handshake::v1";

/// Consensus block header hashing domain.
pub const BLOCK_HEADER: &[u8] = b"nexus::consensus::block::header::v1";

/// State commitment root hashing domain.
pub const STATE_ROOT: &[u8] = b"nexus::storage::state::root::v1";

/// Intent unique-identifier hashing domain.
pub const INTENT_ID: &[u8] = b"nexus::intent::id::v1";
