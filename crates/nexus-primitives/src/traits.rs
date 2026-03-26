// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Compile-time constraint trait for all Nexus protocol identifier types.
//!
//! [`ProtocolId`] is not implemented manually by users.  It is fulfilled
//! automatically by any type that satisfies all supertrait bounds via the
//! blanket `impl` below — typically through `#[derive(...)]`.

use std::{fmt, hash::Hash};

/// Minimal constraint set required of every cross-layer protocol identifier.
///
/// Any type that derives `Debug`, `Clone`, `Copy`, `PartialEq`, `Eq`, `Hash`,
/// `Serialize`, and `Deserialize` automatically satisfies this bound.
///
/// # Examples
///
/// All core ID types satisfy `ProtocolId`:
/// ```
/// use nexus_primitives::{ValidatorIndex, EpochNumber, Blake3Digest, ProtocolId};
///
/// fn needs_id<T: ProtocolId>(_t: T) {}
///
/// needs_id(ValidatorIndex(0));
/// needs_id(EpochNumber(1));
/// needs_id(Blake3Digest::ZERO);
/// ```
pub trait ProtocolId:
    fmt::Debug
    + Clone
    + Copy
    + PartialEq
    + Eq
    + Hash
    + Send
    + Sync
    + 'static
    + serde::Serialize
    + for<'de> serde::Deserialize<'de>
{
}

/// Blanket implementation: any type satisfying all supertrait bounds is a `ProtocolId`.
impl<T> ProtocolId for T where
    T: fmt::Debug
        + Clone
        + Copy
        + PartialEq
        + Eq
        + Hash
        + Send
        + Sync
        + 'static
        + serde::Serialize
        + for<'de> serde::Deserialize<'de>
{
}

// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        digest::Blake3Digest,
        ids::{CommitSequence, EpochNumber, RoundNumber, ShardId, TimestampMs, ValidatorIndex},
    };

    /// Compile-time helper: will not compile if `T` does not implement `ProtocolId`.
    fn assert_protocol_id<T: ProtocolId>() {}

    // TR-01: All standard primitive types satisfy ProtocolId
    #[test]
    fn all_id_types_satisfy_protocol_id() {
        assert_protocol_id::<ValidatorIndex>();
        assert_protocol_id::<EpochNumber>();
        assert_protocol_id::<RoundNumber>();
        assert_protocol_id::<ShardId>();
        assert_protocol_id::<CommitSequence>();
        assert_protocol_id::<TimestampMs>();
        assert_protocol_id::<Blake3Digest>();
    }

    // ProtocolId implies Send + Sync (compile-time check via generic bound)
    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn id_types_are_send_sync() {
        assert_send_sync::<ValidatorIndex>();
        assert_send_sync::<EpochNumber>();
        assert_send_sync::<Blake3Digest>();
    }
}
