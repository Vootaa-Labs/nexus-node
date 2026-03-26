// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Account shard routing via Jump Consistent Hash.
//!
//! Deterministically maps an [`AccountAddress`] to a [`ShardId`] using
//! the Jump Consistent Hash algorithm (Lamping & Veach, 2014).
//! This is a pure function — no state, no allocation, O(ln n) time.

use nexus_primitives::{AccountAddress, ShardId};

/// Map an account address to a shard using Jump Consistent Hash.
///
/// The algorithm distributes keys evenly across `num_shards` buckets
/// with minimal reassignment when the bucket count changes.
///
/// # Panics
///
/// Never panics.  If `num_shards == 0`, returns `ShardId(0)`.
pub fn jump_consistent_hash(account: &AccountAddress, num_shards: u16) -> ShardId {
    if num_shards <= 1 {
        return ShardId(0);
    }

    // Seed from the account address bytes (use first 8 bytes as u64).
    let bytes = &account.0;
    let mut key = u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]);

    let mut b: i64 = -1;
    let mut j: i64 = 0;
    let n = num_shards as i64;

    while j < n {
        b = j;
        // Pseudo-random step: key = key * 2862933555777941757 + 1
        key = key.wrapping_mul(2_862_933_555_777_941_757).wrapping_add(1);
        // j = (b + 1) * (2^31 / ((key >> 33) + 1))
        let divisor = ((key >> 33) + 1) as f64;
        j = ((b + 1) as f64 * (2_147_483_648.0_f64 / divisor)) as i64;
    }

    ShardId(b as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_for_same_input() {
        let account = AccountAddress([0xAA; 32]);
        let s1 = jump_consistent_hash(&account, 8);
        let s2 = jump_consistent_hash(&account, 8);
        assert_eq!(s1, s2);
    }

    #[test]
    fn different_accounts_spread_across_shards() {
        let num_shards = 16u16;
        let mut shard_counts = vec![0u32; num_shards as usize];
        for i in 0..1000u32 {
            let mut addr = [0u8; 32];
            addr[..4].copy_from_slice(&i.to_le_bytes());
            let shard = jump_consistent_hash(&AccountAddress(addr), num_shards);
            shard_counts[shard.0 as usize] += 1;
        }
        // Every shard should get at least some accounts (uniform-ish).
        for count in &shard_counts {
            assert!(*count > 0, "at least one shard got zero accounts");
        }
    }

    #[test]
    fn single_shard_always_zero() {
        let account = AccountAddress([0xBB; 32]);
        assert_eq!(jump_consistent_hash(&account, 1), ShardId(0));
    }

    #[test]
    fn zero_shards_returns_zero() {
        let account = AccountAddress([0xCC; 32]);
        assert_eq!(jump_consistent_hash(&account, 0), ShardId(0));
    }

    #[test]
    fn shard_id_in_range() {
        for num_shards in [2u16, 4, 8, 16, 32, 64, 128, 256, 512] {
            for i in 0..200u32 {
                let mut addr = [0u8; 32];
                addr[..4].copy_from_slice(&i.to_le_bytes());
                let shard = jump_consistent_hash(&AccountAddress(addr), num_shards);
                assert!(
                    shard.0 < num_shards,
                    "shard {} >= num_shards {} for account {}",
                    shard.0,
                    num_shards,
                    i
                );
            }
        }
    }

    #[test]
    fn minimal_reassignment_on_shard_growth() {
        // When we go from 8 to 9 shards, most accounts should stay put.
        let mut moved = 0u32;
        let total = 1000u32;
        for i in 0..total {
            let mut addr = [0u8; 32];
            addr[..4].copy_from_slice(&i.to_le_bytes());
            let s8 = jump_consistent_hash(&AccountAddress(addr), 8);
            let s9 = jump_consistent_hash(&AccountAddress(addr), 9);
            if s8 != s9 {
                moved += 1;
            }
        }
        // Ideally ~1/9 ≈ 11% move.  Allow up to 20%.
        let move_pct = (moved as f64 / total as f64) * 100.0;
        assert!(
            move_pct < 20.0,
            "too many reassignments: {moved}/{total} ({move_pct:.1}%)"
        );
    }
}
