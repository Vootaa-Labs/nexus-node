//! [`AccountResolver`] implementation and supporting sub-modules.
//!
//! * [`shard_lookup`] — Jump Consistent Hash for deterministic shard routing.
//! * [`balance_agg`] — DashMap-backed balance cache.
//! * [`contract_registry`] — DashMap-backed contract location registry.
//! * [`AccountResolverImpl`] — Concrete implementation wiring everything together.

pub mod balance_agg;
pub mod contract_registry;
pub mod shard_lookup;

use crate::error::{IntentError, IntentResult};
use crate::traits::AccountResolver;
use crate::types::ContractLocation;
use balance_agg::BalanceAggregator;
use contract_registry::ContractRegistry;
use nexus_primitives::{AccountAddress, Amount, ContractAddress, ShardId, TokenId};

/// In-memory [`AccountResolver`] backed by DashMap caches and
/// Jump Consistent Hash for shard assignment.
///
/// # Thread Safety
///
/// All internal structures use lock-free concurrent access (`DashMap`),
/// so `AccountResolverImpl` is `Send + Sync` without external locking.
pub struct AccountResolverImpl {
    shard_count: u16,
    balances: BalanceAggregator,
    contracts: ContractRegistry,
}

impl AccountResolverImpl {
    /// Create a new resolver with the given shard count.
    ///
    /// # Panics
    ///
    /// None — a shard count of 0 is tolerated; `primary_shard` will
    /// return `ShardId(0)` for every account in that case.
    pub fn new(shard_count: u16) -> Self {
        Self {
            shard_count,
            balances: BalanceAggregator::new(),
            contracts: ContractRegistry::new(),
        }
    }

    /// Direct access to the balance cache (for pre-loading state).
    pub fn balances(&self) -> &BalanceAggregator {
        &self.balances
    }

    /// Direct access to the contract registry (for pre-loading state).
    pub fn contracts(&self) -> &ContractRegistry {
        &self.contracts
    }

    /// The shard count this resolver was initialised with.
    pub fn shard_count(&self) -> u16 {
        self.shard_count
    }
}

impl AccountResolver for AccountResolverImpl {
    async fn balance(&self, account: &AccountAddress, token: &TokenId) -> IntentResult<Amount> {
        self.balances
            .get(account, token)
            .ok_or(IntentError::AccountNotFound { account: *account })
    }

    async fn primary_shard(&self, account: &AccountAddress) -> IntentResult<ShardId> {
        Ok(shard_lookup::jump_consistent_hash(
            account,
            self.shard_count,
        ))
    }

    async fn contract_location(
        &self,
        contract: &ContractAddress,
    ) -> IntentResult<ContractLocation> {
        self.contracts
            .lookup(contract)
            .ok_or(IntentError::ContractNotFound {
                contract: *contract,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_primitives::ContractAddress;

    fn alice() -> AccountAddress {
        AccountAddress([0xAA; 32])
    }

    fn sample_contract() -> ContractAddress {
        ContractAddress([0xCC; 32])
    }

    fn sample_location() -> ContractLocation {
        ContractLocation {
            shard_id: ShardId(5),
            contract_addr: sample_contract(),
            module_name: "token".to_string(),
            verified: true,
        }
    }

    #[tokio::test]
    async fn balance_found() {
        let resolver = AccountResolverImpl::new(16);
        resolver
            .balances()
            .set_balance(alice(), TokenId::Native, Amount(500));
        let bal = resolver.balance(&alice(), &TokenId::Native).await.unwrap();
        assert_eq!(bal, Amount(500));
    }

    #[tokio::test]
    async fn balance_account_not_found() {
        let resolver = AccountResolverImpl::new(16);
        let err = resolver.balance(&alice(), &TokenId::Native).await;
        assert!(matches!(err, Err(IntentError::AccountNotFound { .. })));
    }

    #[tokio::test]
    async fn primary_shard_deterministic() {
        let resolver = AccountResolverImpl::new(64);
        let s1 = resolver.primary_shard(&alice()).await.unwrap();
        let s2 = resolver.primary_shard(&alice()).await.unwrap();
        assert_eq!(s1, s2);
        assert!(s1.0 < 64);
    }

    #[tokio::test]
    async fn contract_location_found() {
        let resolver = AccountResolverImpl::new(16);
        resolver
            .contracts()
            .register(sample_contract(), sample_location());
        let loc = resolver
            .contract_location(&sample_contract())
            .await
            .unwrap();
        assert_eq!(loc.shard_id, ShardId(5));
        assert_eq!(loc.module_name, "token");
        assert!(loc.verified);
    }

    #[tokio::test]
    async fn contract_location_not_found() {
        let resolver = AccountResolverImpl::new(16);
        let err = resolver.contract_location(&sample_contract()).await;
        assert!(matches!(err, Err(IntentError::ContractNotFound { .. })));
    }

    #[tokio::test]
    async fn shard_count_accessor() {
        let resolver = AccountResolverImpl::new(128);
        assert_eq!(resolver.shard_count(), 128);
    }

    #[tokio::test]
    async fn zero_shard_count() {
        let resolver = AccountResolverImpl::new(0);
        let s = resolver.primary_shard(&alice()).await.unwrap();
        assert_eq!(s, ShardId(0));
    }
}
