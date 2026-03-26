/// Registry -- on-chain key-value store per account.
///
/// Demonstrates stateful resource with multiple u64 slots,
/// existence checks, and owner-only mutation.
/// Inspired by Aptos SimpleMap / on-chain config patterns.
module registry_addr::registry {
    use std::signer;

    /// Error codes.
    const E_ALREADY_EXISTS: u64 = 1;
    const E_NOT_FOUND: u64 = 2;

    /// A fixed-slot registry (4 named u64 slots) stored per account.
    /// Simplifies to fixed fields to avoid vector/table dependencies.
    struct Registry has key, store {
        slot_0: u64,
        slot_1: u64,
        slot_2: u64,
        slot_3: u64,
        count: u64,
    }

    /// Create a new empty registry for the caller.
    public entry fun create(account: &signer) {
        let addr = signer::address_of(account);
        assert!(!exists<Registry>(addr), E_ALREADY_EXISTS);
        move_to(account, Registry {
            slot_0: 0,
            slot_1: 0,
            slot_2: 0,
            slot_3: 0,
            count: 0,
        });
    }

    /// Set a slot value.  `slot` must be 0..3.
    public entry fun set(account: &signer, slot: u64, value: u64) acquires Registry {
        let addr = signer::address_of(account);
        assert!(exists<Registry>(addr), E_NOT_FOUND);
        let reg = borrow_global_mut<Registry>(addr);
        if (slot == 0) {
            reg.slot_0 = value;
        } else if (slot == 1) {
            reg.slot_1 = value;
        } else if (slot == 2) {
            reg.slot_2 = value;
        } else if (slot == 3) {
            reg.slot_3 = value;
        } else {
            abort 100 // INVALID_SLOT
        };
        reg.count = reg.count + 1;
    }

    #[view]
    /// Read a slot value.
    public fun get(addr: address, slot: u64): u64 acquires Registry {
        assert!(exists<Registry>(addr), E_NOT_FOUND);
        let reg = borrow_global<Registry>(addr);
        if (slot == 0) {
            reg.slot_0
        } else if (slot == 1) {
            reg.slot_1
        } else if (slot == 2) {
            reg.slot_2
        } else if (slot == 3) {
            reg.slot_3
        } else {
            abort 100 // INVALID_SLOT
        }
    }

    #[view]
    /// How many set operations have been performed.
    public fun get_write_count(addr: address): u64 acquires Registry {
        assert!(exists<Registry>(addr), E_NOT_FOUND);
        borrow_global<Registry>(addr).count
    }
}
