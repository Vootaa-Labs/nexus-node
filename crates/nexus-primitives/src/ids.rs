// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Newtype wrappers for all Nexus protocol identifier types.
//!
//! These are zero-cost abstractions over primitive integers that provide
//! compile-time type safety — you cannot accidentally pass an `EpochNumber`
//! where a `ValidatorIndex` is expected, even though both wrap a `u64`.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Validator index — zero-based position in the current validator committee.
///
/// Serialized as 4 bytes (u32 little-endian) in BCS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ValidatorIndex(
    /// Zero-based position in the validator set.
    pub u32,
);

impl fmt::Display for ValidatorIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Epoch number — each epoch lasts approximately 24 hours.
///
/// Serialized as 8 bytes (u64 little-endian) in BCS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EpochNumber(
    /// Epoch counter, starting from 0 at genesis.
    pub u64,
);

impl fmt::Display for EpochNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// DAG round number — each round lasts approximately 200 ms.
///
/// Monotonically increasing within an epoch.
/// Serialized as 8 bytes (u64 little-endian) in BCS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RoundNumber(
    /// Round counter, reset to 0 at the start of each epoch.
    pub u64,
);

impl fmt::Display for RoundNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Execution shard identifier — dynamically allocated, zero-based.
///
/// Serialized as 2 bytes (u16 little-endian) in BCS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ShardId(
    /// Shard index.
    pub u16,
);

impl fmt::Display for ShardId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Global commit sequence number — Shoal++ output, strictly monotone.
///
/// Uniquely identifies a committed block across all epochs.
/// Serialized as 8 bytes (u64 little-endian) in BCS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CommitSequence(
    /// Globally unique, strictly increasing commit counter.
    pub u64,
);

impl fmt::Display for CommitSequence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Wall-clock timestamp with millisecond precision.
///
/// Represents milliseconds elapsed since the Unix epoch (1970-01-01T00:00:00Z).
/// Serialized as 8 bytes (u64 little-endian) in BCS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TimestampMs(
    /// Milliseconds since the Unix epoch.
    pub u64,
);

impl fmt::Display for TimestampMs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl TimestampMs {
    /// Returns the current wall-clock time as a UTC millisecond timestamp.
    ///
    /// Returns `TimestampMs(0)` on the (practically impossible) event that the
    /// system clock reports a time before the Unix epoch.
    pub fn now() -> Self {
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u64::MAX as u128) as u64;
        Self(ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // ID-01: newtype wrapping / unwrapping roundtrip
    #[test]
    fn validator_index_roundtrip() {
        let v = ValidatorIndex(42);
        assert_eq!(v.0, 42);
    }

    // ID-02: ordering is correct
    #[test]
    fn epoch_number_ordering() {
        assert!(EpochNumber(1) < EpochNumber(2));
        assert!(EpochNumber(100) > EpochNumber(99));
        assert_eq!(EpochNumber(5), EpochNumber(5));
    }

    #[test]
    fn round_number_ordering() {
        assert!(RoundNumber(0) < RoundNumber(1));
        assert!(RoundNumber(u64::MAX - 1) < RoundNumber(u64::MAX));
    }

    #[test]
    fn commit_sequence_ordering() {
        assert!(CommitSequence(100) < CommitSequence(200));
    }

    // ID-03: types work as HashMap keys
    #[test]
    fn shard_id_as_hashmap_key() {
        let mut map = HashMap::new();
        map.insert(ShardId(0), "shard-0");
        map.insert(ShardId(1), "shard-1");
        assert_eq!(map[&ShardId(0)], "shard-0");
        assert_eq!(map[&ShardId(1)], "shard-1");
    }

    #[test]
    fn validator_index_as_hashmap_key() {
        let mut map = HashMap::new();
        for i in 0u32..10 {
            map.insert(ValidatorIndex(i), i * 2);
        }
        assert_eq!(map[&ValidatorIndex(5)], 10);
    }

    // ID-04: BCS serialization roundtrip
    #[test]
    fn validator_index_bcs_roundtrip() {
        let original = ValidatorIndex(99);
        let bytes = bcs::to_bytes(&original).expect("bcs serialize");
        let decoded: ValidatorIndex = bcs::from_bytes(&bytes).expect("bcs deserialize");
        assert_eq!(original, decoded);
    }

    #[test]
    fn epoch_number_bcs_roundtrip() {
        let original = EpochNumber(12345);
        let bytes = bcs::to_bytes(&original).expect("bcs serialize");
        let decoded: EpochNumber = bcs::from_bytes(&bytes).expect("bcs deserialize");
        assert_eq!(original, decoded);
    }

    #[test]
    fn shard_id_bcs_roundtrip() {
        let original = ShardId(7);
        let bytes = bcs::to_bytes(&original).expect("bcs serialize");
        let decoded: ShardId = bcs::from_bytes(&bytes).expect("bcs deserialize");
        assert_eq!(original, decoded);
    }

    // ID-05: JSON serialization roundtrip
    #[test]
    fn round_number_json_roundtrip() {
        let original = RoundNumber(42);
        let json = serde_json::to_string(&original).expect("json serialize");
        let decoded: RoundNumber = serde_json::from_str(&json).expect("json deserialize");
        assert_eq!(original, decoded);
    }

    #[test]
    fn commit_sequence_json_roundtrip() {
        let original = CommitSequence(999_999);
        let json = serde_json::to_string(&original).expect("json serialize");
        let decoded: CommitSequence = serde_json::from_str(&json).expect("json deserialize");
        assert_eq!(original, decoded);
    }

    // ID-06: Copy semantics — both instances remain valid after the copy
    #[test]
    fn id_types_are_copy() {
        let a = ValidatorIndex(7);
        let b = a; // copy, not move
        assert_eq!(a, b);

        let e1 = EpochNumber(3);
        let e2 = e1;
        assert_eq!(e1, e2);

        let r1 = RoundNumber(1000);
        let r2 = r1;
        assert_eq!(r1, r2);
    }

    // ID-07: TimestampMs::now() returns a plausible value
    #[test]
    fn timestamp_ms_now_is_reasonable() {
        let ts = TimestampMs::now();
        // Lower bound: 2025-01-01 00:00:00 UTC in ms
        let lower_bound_ms: u64 = 1_735_689_600_000;
        // Upper bound: 2100-01-01 00:00:00 UTC in ms
        let upper_bound_ms: u64 = 4_102_444_800_000;
        assert!(ts.0 >= lower_bound_ms, "timestamp too old: {} ms", ts.0);
        assert!(
            ts.0 <= upper_bound_ms,
            "timestamp too far in future: {} ms",
            ts.0
        );
    }
}
