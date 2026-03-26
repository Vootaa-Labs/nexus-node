//! Contract registry — maps [`ContractAddress`] → [`ContractLocation`].
//!
//! Provides a concurrent in-memory lookup table for deployed contracts.
//! In production the registry would be populated from on-chain state
//! via periodic refresh; for now it is pre-loaded via `register()`.

use dashmap::DashMap;
use nexus_primitives::ContractAddress;

use crate::types::ContractLocation;

/// Thread-safe contract location registry backed by `DashMap`.
pub struct ContractRegistry {
    contracts: DashMap<ContractAddress, ContractLocation>,
}

impl ContractRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            contracts: DashMap::new(),
        }
    }

    /// Create a registry pre-sized for `capacity` contracts.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            contracts: DashMap::with_capacity(capacity),
        }
    }

    /// Register a contract's location (upsert).
    pub fn register(&self, address: ContractAddress, location: ContractLocation) {
        self.contracts.insert(address, location);
    }

    /// Look up where a contract is deployed.
    pub fn lookup(&self, address: &ContractAddress) -> Option<ContractLocation> {
        self.contracts.get(address).map(|r| r.value().clone())
    }

    /// Remove a contract entry.
    pub fn remove(&self, address: &ContractAddress) {
        self.contracts.remove(address);
    }

    /// Number of registered contracts.
    pub fn len(&self) -> usize {
        self.contracts.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.contracts.is_empty()
    }
}

impl Default for ContractRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_primitives::ShardId;

    fn contract_a() -> ContractAddress {
        ContractAddress([0x0A; 32])
    }
    fn contract_b() -> ContractAddress {
        ContractAddress([0x0B; 32])
    }

    fn location(shard: u16, module: &str) -> ContractLocation {
        ContractLocation {
            shard_id: ShardId(shard),
            contract_addr: ContractAddress([0x00; 32]),
            module_name: module.to_string(),
            verified: false,
        }
    }

    #[test]
    fn empty_registry() {
        let reg = ContractRegistry::new();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
        assert_eq!(reg.lookup(&contract_a()), None);
    }

    #[test]
    fn register_and_lookup() {
        let reg = ContractRegistry::new();
        reg.register(contract_a(), location(3, "token"));
        let loc = reg.lookup(&contract_a()).unwrap();
        assert_eq!(loc.shard_id, ShardId(3));
        assert_eq!(loc.module_name, "token");
    }

    #[test]
    fn upsert_overwrites() {
        let reg = ContractRegistry::new();
        reg.register(contract_a(), location(3, "old_mod"));
        reg.register(contract_a(), location(5, "new_mod"));
        let loc = reg.lookup(&contract_a()).unwrap();
        assert_eq!(loc.shard_id, ShardId(5));
        assert_eq!(loc.module_name, "new_mod");
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn distinct_contracts() {
        let reg = ContractRegistry::new();
        reg.register(contract_a(), location(1, "mod_a"));
        reg.register(contract_b(), location(2, "mod_b"));
        assert_eq!(reg.len(), 2);
        assert_eq!(reg.lookup(&contract_a()).unwrap().shard_id, ShardId(1));
        assert_eq!(reg.lookup(&contract_b()).unwrap().shard_id, ShardId(2));
    }

    #[test]
    fn remove_contract() {
        let reg = ContractRegistry::new();
        reg.register(contract_a(), location(1, "mod"));
        reg.remove(&contract_a());
        assert!(reg.is_empty());
        assert_eq!(reg.lookup(&contract_a()), None);
    }

    #[test]
    fn with_capacity() {
        let reg = ContractRegistry::with_capacity(500);
        assert!(reg.is_empty());
        reg.register(contract_a(), location(0, "mod"));
        assert_eq!(reg.len(), 1);
    }
}
