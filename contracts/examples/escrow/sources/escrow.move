/// Escrow -- two-party escrow with time-locked release.
///
/// Demonstrates multi-party coordination, conditional release,
/// and deadline-based timeout patterns.
module escrow_addr::escrow {
    use std::signer;

    /// Error codes.
    const E_ALREADY_EXISTS: u64 = 1;
    const E_NOT_FOUND: u64 = 2;
    const E_NOT_AUTHORIZED: u64 = 3;
    const E_ALREADY_RELEASED: u64 = 4;
    const E_DEADLINE_NOT_REACHED: u64 = 5;
    const E_DEADLINE_PASSED: u64 = 6;

    /// Status of an escrow.
    const STATUS_PENDING: u8 = 0;
    const STATUS_RELEASED: u8 = 1;
    const STATUS_REFUNDED: u8 = 2;

    /// An escrow holding funds between two parties.
    struct Escrow has key, store {
        /// The party that deposited funds.
        depositor: address,
        /// The party that will receive funds on release.
        beneficiary: address,
        /// Amount held in escrow.
        amount: u64,
        /// Epoch deadline -- after this the depositor may reclaim.
        deadline_epoch: u64,
        /// Current status.
        status: u8,
    }

    /// Create a new escrow.  The depositor locks `amount` until
    /// `deadline_epoch`.
    public entry fun create(
        depositor: &signer,
        beneficiary: address,
        amount: u64,
        deadline_epoch: u64,
    ) {
        let addr = signer::address_of(depositor);
        assert!(!exists<Escrow>(addr), E_ALREADY_EXISTS);
        move_to(depositor, Escrow {
            depositor: addr,
            beneficiary,
            amount,
            deadline_epoch,
            status: STATUS_PENDING,
        });
    }

    /// Release escrowed funds to the beneficiary.
    /// Only the depositor may trigger release before the deadline.
    public entry fun release(depositor: &signer) acquires Escrow {
        let addr = signer::address_of(depositor);
        assert!(exists<Escrow>(addr), E_NOT_FOUND);
        let escrow = borrow_global_mut<Escrow>(addr);
        assert!(escrow.depositor == addr, E_NOT_AUTHORIZED);
        assert!(escrow.status == STATUS_PENDING, E_ALREADY_RELEASED);
        escrow.status = STATUS_RELEASED;
    }

    /// Refund escrowed funds to the depositor after the deadline.
    public entry fun refund(depositor: &signer, current_epoch: u64) acquires Escrow {
        let addr = signer::address_of(depositor);
        assert!(exists<Escrow>(addr), E_NOT_FOUND);
        let escrow = borrow_global_mut<Escrow>(addr);
        assert!(escrow.depositor == addr, E_NOT_AUTHORIZED);
        assert!(escrow.status == STATUS_PENDING, E_ALREADY_RELEASED);
        assert!(current_epoch >= escrow.deadline_epoch, E_DEADLINE_NOT_REACHED);
        escrow.status = STATUS_REFUNDED;
    }

    #[view]
    /// View escrow details.
    public fun get_escrow(addr: address): (address, address, u64, u64, u8) acquires Escrow {
        assert!(exists<Escrow>(addr), E_NOT_FOUND);
        let e = borrow_global<Escrow>(addr);
        (e.depositor, e.beneficiary, e.amount, e.deadline_epoch, e.status)
    }
}
