// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Cross-shard balance aggregation.
//!
//! [`BalanceAggregator`] maintains an in-memory cache of account balances
//! keyed by `(AccountAddress, TokenId)`.  In production this would be
//! backed by cross-shard state queries via `nexus-storage`; for now the
//! cache is the source of truth (populated via `set_balance`).

use dashmap::DashMap;
use nexus_primitives::{AccountAddress, Amount, TokenId};

/// Thread-safe balance cache for cross-shard aggregation.
///
/// Uses `DashMap` for lock-free concurrent reads.  The key is
/// `(AccountAddress, TokenId)` and the value is the aggregate balance.
pub struct BalanceAggregator {
    balances: DashMap<(AccountAddress, TokenId), Amount>,
}

impl BalanceAggregator {
    /// Create an empty balance aggregator.
    pub fn new() -> Self {
        Self {
            balances: DashMap::new(),
        }
    }

    /// Create a balance aggregator pre-sized for `capacity` accounts.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            balances: DashMap::with_capacity(capacity),
        }
    }

    /// Look up the aggregate balance for an account and token.
    ///
    /// Returns `None` if the account is unknown.
    pub fn get(&self, account: &AccountAddress, token: &TokenId) -> Option<Amount> {
        self.balances.get(&(*account, *token)).map(|r| *r.value())
    }

    /// Set the balance for an account/token pair (upsert).
    pub fn set_balance(&self, account: AccountAddress, token: TokenId, amount: Amount) {
        self.balances.insert((account, token), amount);
    }

    /// Remove an account/token entry.
    pub fn remove(&self, account: &AccountAddress, token: &TokenId) {
        self.balances.remove(&(*account, *token));
    }

    /// Check whether an account has any known balance entry.
    pub fn account_exists(&self, account: &AccountAddress) -> bool {
        self.balances.iter().any(|entry| entry.key().0 == *account)
    }

    /// Number of cached balance entries.
    pub fn len(&self) -> usize {
        self.balances.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.balances.is_empty()
    }
}

impl Default for BalanceAggregator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_primitives::ContractAddress;

    fn alice() -> AccountAddress {
        AccountAddress([0xAA; 32])
    }
    fn bob() -> AccountAddress {
        AccountAddress([0xBB; 32])
    }

    #[test]
    fn empty_aggregator() {
        let agg = BalanceAggregator::new();
        assert!(agg.is_empty());
        assert_eq!(agg.len(), 0);
        assert_eq!(agg.get(&alice(), &TokenId::Native), None);
    }

    #[test]
    fn set_and_get_native() {
        let agg = BalanceAggregator::new();
        agg.set_balance(alice(), TokenId::Native, Amount(1000));
        assert_eq!(agg.get(&alice(), &TokenId::Native), Some(Amount(1000)));
        assert!(!agg.is_empty());
        assert_eq!(agg.len(), 1);
    }

    #[test]
    fn distinct_tokens() {
        let agg = BalanceAggregator::new();
        let usdc = TokenId::Contract(ContractAddress([0x01; 32]));
        agg.set_balance(alice(), TokenId::Native, Amount(100));
        agg.set_balance(alice(), usdc, Amount(200));
        assert_eq!(agg.get(&alice(), &TokenId::Native), Some(Amount(100)));
        assert_eq!(agg.get(&alice(), &usdc), Some(Amount(200)));
        assert_eq!(agg.len(), 2);
    }

    #[test]
    fn distinct_accounts() {
        let agg = BalanceAggregator::new();
        agg.set_balance(alice(), TokenId::Native, Amount(100));
        agg.set_balance(bob(), TokenId::Native, Amount(200));
        assert_eq!(agg.get(&alice(), &TokenId::Native), Some(Amount(100)));
        assert_eq!(agg.get(&bob(), &TokenId::Native), Some(Amount(200)));
    }

    #[test]
    fn upsert_overwrites() {
        let agg = BalanceAggregator::new();
        agg.set_balance(alice(), TokenId::Native, Amount(100));
        agg.set_balance(alice(), TokenId::Native, Amount(999));
        assert_eq!(agg.get(&alice(), &TokenId::Native), Some(Amount(999)));
        assert_eq!(agg.len(), 1);
    }

    #[test]
    fn remove_entry() {
        let agg = BalanceAggregator::new();
        agg.set_balance(alice(), TokenId::Native, Amount(100));
        agg.remove(&alice(), &TokenId::Native);
        assert!(agg.is_empty());
        assert_eq!(agg.get(&alice(), &TokenId::Native), None);
    }

    #[test]
    fn account_exists_check() {
        let agg = BalanceAggregator::new();
        assert!(!agg.account_exists(&alice()));
        agg.set_balance(alice(), TokenId::Native, Amount(0));
        assert!(agg.account_exists(&alice()));
    }

    #[test]
    fn with_capacity() {
        let agg = BalanceAggregator::with_capacity(1000);
        assert!(agg.is_empty());
        agg.set_balance(alice(), TokenId::Native, Amount(42));
        assert_eq!(agg.get(&alice(), &TokenId::Native), Some(Amount(42)));
    }
}
