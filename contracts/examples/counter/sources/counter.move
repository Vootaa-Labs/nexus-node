/// Counter -- minimal stateful contract.
///
/// Stores a single `u64` counter per deployer account.
/// Demonstrates basic Move resource publish, increment, and read.
module counter_addr::counter {
    use std::signer;

    /// The counter resource stored under the deployer's account.
    struct Counter has key, store {
        value: u64,
    }

    /// Initialise the counter to zero.
    /// Called once after module publication.
    public entry fun initialize(account: &signer) {
        let addr = signer::address_of(account);
        assert!(!exists<Counter>(addr), 1); // ALREADY_INITIALISED
        move_to(account, Counter { value: 0 });
    }

    /// Increment the counter by 1 and return the new value.
    public entry fun increment(account: &signer) acquires Counter {
        let addr = signer::address_of(account);
        assert!(exists<Counter>(addr), 2); // NOT_INITIALISED
        let counter = borrow_global_mut<Counter>(addr);
        counter.value = counter.value + 1;
    }

    #[view]
    /// Read the current counter value (view function).
    public fun get_count(addr: address): u64 acquires Counter {
        assert!(exists<Counter>(addr), 2);
        borrow_global<Counter>(addr).value
    }

    /// Reset the counter to zero.
    public entry fun reset(account: &signer) acquires Counter {
        let addr = signer::address_of(account);
        assert!(exists<Counter>(addr), 2);
        let counter = borrow_global_mut<Counter>(addr);
        counter.value = 0;
    }
}
