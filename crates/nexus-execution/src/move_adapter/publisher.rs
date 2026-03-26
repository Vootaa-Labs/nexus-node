//! Module publisher — contract address derivation and module storage.
//!
//! Implements the full module publish flow:
//!
//! 1. **Contract address derivation** per Solutions/06 §ContractAddress:
//!    `BLAKE3("nexus::contract::address::v1" || deployer_addr || bytecode_hash)`
//!
//! 2. **Module storage** — stores each module's bytecode and hash under
//!    the derived contract address, plus deployer metadata.
//!
//! 3. **Overwrite protection** — prevents re-publishing to an address that
//!    already has code (modules are immutable once deployed).
//!
//! The publisher is invoked *after* bytecode verification succeeds.

use std::collections::HashMap;

use crate::error::ExecutionResult;
use crate::types::{ExecutionStatus, StateChange};
use nexus_primitives::AccountAddress;

use super::gas_meter::{GasMeter, GasSchedule, SimpleGasMeter};
use super::state_view::NexusStateView;
use super::VmOutput;

// ── Domain separation ───────────────────────────────────────────────────

/// Domain tag for contract address derivation (Solutions/06 spec).
const CONTRACT_ADDRESS_DOMAIN: &[u8] = b"nexus::contract::address::v1";

// ── Storage key constants ───────────────────────────────────────────────

/// Key for the published module bytecode under the contract address.
pub(crate) const MODULE_CODE_KEY: &[u8] = b"code";

/// Key for the BLAKE3 hash of the published bytecode.
pub(crate) const MODULE_CODE_HASH_KEY: &[u8] = b"code_hash";

/// Key for the deployer's account address (stored under the contract address).
pub(crate) const MODULE_DEPLOYER_KEY: &[u8] = b"deployer";

/// Key for the number of modules in a published package.
pub(crate) const MODULE_COUNT_KEY: &[u8] = b"module_count";

// ── Contract address derivation ─────────────────────────────────────────

/// Derive a contract address from the deployer and bytecode.
///
/// ```text
/// address = BLAKE3("nexus::contract::address::v1" || deployer_addr || bytecode_hash)
/// ```
///
/// The `bytecode_hash` is the BLAKE3 hash of the concatenated module bytecodes.
pub(crate) fn derive_contract_address(
    deployer: &AccountAddress,
    bytecode_hash: &blake3::Hash,
) -> AccountAddress {
    let mut hasher = blake3::Hasher::new();
    hasher.update(CONTRACT_ADDRESS_DOMAIN);
    hasher.update(&deployer.0);
    hasher.update(bytecode_hash.as_bytes());
    let digest: [u8; 32] = *hasher.finalize().as_bytes();
    AccountAddress(digest)
}

// ── ModulePublisher ─────────────────────────────────────────────────────

/// Handles the storage side of module publishing.
///
/// Given verified bytecode modules, the publisher:
/// 1. Computes the bytecode hash and derives the contract address
/// 2. Checks that no code already exists at the derived address
/// 3. Charges gas for storage
/// 4. Writes module bytecode, hash, deployer metadata, and module count
pub(crate) struct ModulePublisher<'a> {
    schedule: &'a GasSchedule,
}

impl<'a> ModulePublisher<'a> {
    /// Create a new publisher with the given gas schedule.
    pub fn new(schedule: &'a GasSchedule) -> Self {
        Self { schedule }
    }

    /// Publish verified modules.
    ///
    /// # Arguments
    /// - `state` — state view for reading existing modules
    /// - `sender` — deployer account address
    /// - `modules` — verified compiled Move bytecode modules
    /// - `gas_limit` — maximum gas for this publish
    ///
    /// # Returns
    /// A `VmOutput` containing the contract address in the write-set,
    /// or an abort status if publishing fails (out of gas, duplicate address).
    pub fn publish(
        &self,
        state: &NexusStateView<'_>,
        sender: AccountAddress,
        modules: &[Vec<u8>],
        gas_limit: u64,
    ) -> ExecutionResult<PublishOutput> {
        // Concatenate modules and compute hash.
        let total_size: usize = modules.iter().map(|m| m.len()).sum();
        let mut bytecode = Vec::with_capacity(total_size);
        for module in modules {
            bytecode.extend_from_slice(module);
        }
        let code_hash = blake3::hash(&bytecode);

        // Derive contract address.
        let contract_addr = derive_contract_address(&sender, &code_hash);

        // Check that no code already exists at this address (immutable modules).
        let existing = state.has_module(&contract_addr)?;
        if existing {
            return Ok(PublishOutput {
                vm_output: VmOutput {
                    status: ExecutionStatus::MoveAbort {
                        location: "nexus::publish".into(),
                        code: 20, // MODULE_ALREADY_EXISTS
                    },
                    gas_used: self.schedule.publish_base,
                    state_changes: vec![],
                    write_set: HashMap::new(),
                },
                contract_address: contract_addr,
            });
        }

        // Gas metering.
        let mut meter = SimpleGasMeter::new(gas_limit);
        let gas_needed = self
            .schedule
            .publish_base
            .saturating_add((total_size as u64).saturating_mul(self.schedule.publish_per_byte));

        if meter.charge(gas_needed).is_err() {
            return Ok(PublishOutput {
                vm_output: VmOutput {
                    status: ExecutionStatus::OutOfGas,
                    gas_used: gas_limit,
                    state_changes: vec![],
                    write_set: HashMap::new(),
                },
                contract_address: contract_addr,
            });
        }

        // Build write-set and state changes.
        let mut write_set = HashMap::new();
        let mut state_changes = Vec::new();

        // Store bytecode under contract address.
        write_set.insert(
            (contract_addr, MODULE_CODE_KEY.to_vec()),
            Some(bytecode.clone()),
        );
        state_changes.push(StateChange {
            account: contract_addr,
            key: MODULE_CODE_KEY.to_vec(),
            value: Some(bytecode),
        });

        // Store bytecode hash.
        let hash_bytes = code_hash.as_bytes().to_vec();
        write_set.insert(
            (contract_addr, MODULE_CODE_HASH_KEY.to_vec()),
            Some(hash_bytes.clone()),
        );
        state_changes.push(StateChange {
            account: contract_addr,
            key: MODULE_CODE_HASH_KEY.to_vec(),
            value: Some(hash_bytes),
        });

        // Store deployer address.
        let deployer_bytes = sender.0.to_vec();
        write_set.insert(
            (contract_addr, MODULE_DEPLOYER_KEY.to_vec()),
            Some(deployer_bytes.clone()),
        );
        state_changes.push(StateChange {
            account: contract_addr,
            key: MODULE_DEPLOYER_KEY.to_vec(),
            value: Some(deployer_bytes),
        });

        // Store module count.
        let count_bytes = (modules.len() as u32).to_le_bytes().to_vec();
        write_set.insert(
            (contract_addr, MODULE_COUNT_KEY.to_vec()),
            Some(count_bytes.clone()),
        );
        state_changes.push(StateChange {
            account: contract_addr,
            key: MODULE_COUNT_KEY.to_vec(),
            value: Some(count_bytes),
        });

        Ok(PublishOutput {
            vm_output: VmOutput {
                status: ExecutionStatus::Success,
                gas_used: meter.consumed(),
                state_changes,
                write_set,
            },
            contract_address: contract_addr,
        })
    }
}

/// Output of a successful (or failed) publish operation.
#[derive(Debug)]
pub(crate) struct PublishOutput {
    /// The VM execution output.
    pub vm_output: VmOutput,
    /// The derived contract address (available even on failure for diagnostics).
    #[allow(dead_code)]
    pub contract_address: AccountAddress,
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::StateView;
    use std::collections::HashMap as StdHashMap;

    #[cfg(test)]
    use super::super::verifier::make_test_module;

    struct MemState {
        data: StdHashMap<(AccountAddress, Vec<u8>), Vec<u8>>,
    }

    impl MemState {
        fn new() -> Self {
            Self {
                data: StdHashMap::new(),
            }
        }

        fn set(&mut self, account: AccountAddress, key: &[u8], value: Vec<u8>) {
            self.data.insert((account, key.to_vec()), value);
        }
    }

    impl StateView for MemState {
        fn get(&self, account: &AccountAddress, key: &[u8]) -> ExecutionResult<Option<Vec<u8>>> {
            Ok(self.data.get(&(*account, key.to_vec())).cloned())
        }
    }

    fn addr(b: u8) -> AccountAddress {
        AccountAddress([b; 32])
    }

    fn schedule() -> GasSchedule {
        GasSchedule {
            transfer_base: 1_000,
            call_base: 1_000,
            publish_base: 2_000,
            publish_per_byte: 1,
            read_per_byte: 1,
            write_per_byte: 5,
        }
    }

    #[test]
    fn derive_address_deterministic() {
        let deployer = addr(0xAA);
        let hash = blake3::hash(b"some bytecode");
        let a1 = derive_contract_address(&deployer, &hash);
        let a2 = derive_contract_address(&deployer, &hash);
        assert_eq!(a1, a2);
    }

    #[test]
    fn derive_address_different_deployers() {
        let hash = blake3::hash(b"same bytecode");
        let a1 = derive_contract_address(&addr(0xAA), &hash);
        let a2 = derive_contract_address(&addr(0xBB), &hash);
        assert_ne!(a1, a2);
    }

    #[test]
    fn derive_address_different_bytecode() {
        let deployer = addr(0xAA);
        let h1 = blake3::hash(b"bytecode_v1");
        let h2 = blake3::hash(b"bytecode_v2");
        let a1 = derive_contract_address(&deployer, &h1);
        let a2 = derive_contract_address(&deployer, &h2);
        assert_ne!(a1, a2);
    }

    #[test]
    fn publish_success_stores_all_keys() {
        let sched = schedule();
        let publisher = ModulePublisher::new(&sched);
        let state = MemState::new();
        let view = NexusStateView::new(&state);
        let modules = vec![make_test_module(16)];

        let result = publisher
            .publish(&view, addr(0xAA), &modules, 50_000)
            .unwrap();
        assert_eq!(result.vm_output.status, ExecutionStatus::Success);

        // Should have 4 state changes: code, code_hash, deployer, module_count.
        assert_eq!(result.vm_output.state_changes.len(), 4);

        // Verify write-set contains expected keys.
        let ws = &result.vm_output.write_set;
        let ca = result.contract_address;
        assert!(ws.contains_key(&(ca, MODULE_CODE_KEY.to_vec())));
        assert!(ws.contains_key(&(ca, MODULE_CODE_HASH_KEY.to_vec())));
        assert!(ws.contains_key(&(ca, MODULE_DEPLOYER_KEY.to_vec())));
        assert!(ws.contains_key(&(ca, MODULE_COUNT_KEY.to_vec())));

        // Deployer should be sender's address bytes.
        let deployer_stored = ws.get(&(ca, MODULE_DEPLOYER_KEY.to_vec())).unwrap();
        assert_eq!(deployer_stored.as_ref().unwrap(), &addr(0xAA).0.to_vec());

        // Module count should be 1.
        let count_stored = ws.get(&(ca, MODULE_COUNT_KEY.to_vec())).unwrap();
        let count = u32::from_le_bytes(count_stored.as_ref().unwrap()[..4].try_into().unwrap());
        assert_eq!(count, 1);
    }

    #[test]
    fn publish_stores_under_derived_address_not_sender() {
        let sched = schedule();
        let publisher = ModulePublisher::new(&sched);
        let state = MemState::new();
        let view = NexusStateView::new(&state);
        let modules = vec![make_test_module(16)];

        let result = publisher
            .publish(&view, addr(0xAA), &modules, 50_000)
            .unwrap();

        // Contract address should NOT be the sender address.
        assert_ne!(result.contract_address, addr(0xAA));

        // All writes should be under the contract address.
        for (acct, _) in result.vm_output.write_set.keys() {
            assert_eq!(*acct, result.contract_address);
        }
    }

    #[test]
    fn publish_duplicate_address_rejected() {
        let sched = schedule();
        let publisher = ModulePublisher::new(&sched);
        let modules = vec![make_test_module(16)];

        // Pre-populate state with code at the derived address.
        let bytecode: Vec<u8> = modules.iter().flat_map(|m| m.iter().copied()).collect();
        let code_hash = blake3::hash(&bytecode);
        let contract_addr = derive_contract_address(&addr(0xAA), &code_hash);

        let mut state = MemState::new();
        state.set(contract_addr, MODULE_CODE_KEY, vec![0x01]);

        let view = NexusStateView::new(&state);
        let result = publisher
            .publish(&view, addr(0xAA), &modules, 50_000)
            .unwrap();

        assert!(matches!(
            result.vm_output.status,
            ExecutionStatus::MoveAbort { code: 20, .. }
        ));
    }

    #[test]
    fn publish_out_of_gas() {
        let sched = schedule();
        let publisher = ModulePublisher::new(&sched);
        let state = MemState::new();
        let view = NexusStateView::new(&state);
        let modules = vec![make_test_module(100)];

        let result = publisher.publish(&view, addr(0xAA), &modules, 10).unwrap();
        assert_eq!(result.vm_output.status, ExecutionStatus::OutOfGas);
    }

    #[test]
    fn publish_multiple_modules_gas() {
        let sched = schedule();
        let publisher = ModulePublisher::new(&sched);
        let state = MemState::new();
        let view = NexusStateView::new(&state);
        let modules = vec![make_test_module(10), make_test_module(20)];

        let total_size: usize = modules.iter().map(|m| m.len()).sum();
        let expected_gas = sched.publish_base + (total_size as u64) * sched.publish_per_byte;

        let result = publisher
            .publish(&view, addr(0xAA), &modules, 50_000)
            .unwrap();
        assert_eq!(result.vm_output.status, ExecutionStatus::Success);
        assert_eq!(result.vm_output.gas_used, expected_gas);
    }
}
