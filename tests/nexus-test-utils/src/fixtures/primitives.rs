//! Primitive-level test fixtures: digests, addresses, identifiers.

use nexus_primitives::{
    AccountAddress, Amount, Blake3Digest, ContractAddress, EpochNumber, RoundNumber, ShardId,
    TimestampMs, ValidatorIndex,
};

/// Create a [`Blake3Digest`] filled with a single repeated byte value.
///
/// Useful for generating distinct, recognisable digests in tests.
///
/// ```
/// # use nexus_test_utils::fixtures::primitives::make_digest;
/// let d = make_digest(0xAB);
/// assert_eq!(d.as_bytes()[0], 0xAB);
/// ```
pub fn make_digest(fill: u8) -> Blake3Digest {
    Blake3Digest::from_bytes([fill; 32])
}

/// Create a deterministic [`AccountAddress`] from an index.
///
/// Index `0` → `[0, 0, …, 0]`, `1` → `[1, 0, …, 0]`, etc.
pub fn make_account_address(index: u8) -> AccountAddress {
    let mut bytes = [0u8; 32];
    bytes[0] = index;
    AccountAddress(bytes)
}

/// Create a deterministic [`ContractAddress`] from an index.
pub fn make_contract_address(index: u8) -> ContractAddress {
    let mut bytes = [0u8; 32];
    bytes[0] = index;
    ContractAddress(bytes)
}

/// Standard test token ID (native NXS).
pub fn make_native_token() -> nexus_primitives::TokenId {
    nexus_primitives::TokenId::Native
}

/// Create a non-zero [`Amount`] from a raw `u64` value.
pub fn make_amount(val: u64) -> Amount {
    Amount(val)
}

/// Create a [`ValidatorIndex`] for test use.
pub fn make_validator_index(idx: u32) -> ValidatorIndex {
    ValidatorIndex(idx)
}

/// Create a test [`EpochNumber`].
pub fn make_epoch(n: u64) -> EpochNumber {
    EpochNumber(n)
}

/// Create a test [`RoundNumber`].
pub fn make_round(n: u64) -> RoundNumber {
    RoundNumber(n)
}

/// Create a test [`ShardId`].
pub fn make_shard(id: u16) -> ShardId {
    ShardId(id)
}

/// Create a test [`TimestampMs`].
pub fn make_timestamp(ms: u64) -> TimestampMs {
    TimestampMs(ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_fill() {
        let d = make_digest(0xFF);
        assert!(d.as_bytes().iter().all(|&b| b == 0xFF));
    }

    #[test]
    fn address_deterministic() {
        let a = make_account_address(42);
        let b = make_account_address(42);
        assert_eq!(a, b);
        assert_ne!(make_account_address(1), make_account_address(2));
    }

    #[test]
    fn amount_roundtrip() {
        let a = make_amount(1_000_000_000);
        assert_eq!(a.0, 1_000_000_000);
    }
}
