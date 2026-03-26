/// Token -- simple fungible token with mint, burn, and transfer.
///
/// Demonstrates resource management, capability-based access control
/// (mint authority), and cross-account state mutation.
module token_addr::token {
    use std::signer;

    /// Error codes.
    const E_NOT_AUTHORIZED: u64 = 1;
    const E_INSUFFICIENT_BALANCE: u64 = 2;
    const E_ALREADY_INITIALISED: u64 = 3;
    const E_NOT_INITIALISED: u64 = 4;

    /// Balance resource held by each account.
    struct Balance has key, store {
        value: u64,
    }

    /// Mint capability -- only the deployer may mint.
    struct MintCapability has key, store {
        total_supply: u64,
    }

    /// Initialise the token module: create the mint capability.
    public entry fun initialize(deployer: &signer) {
        let addr = signer::address_of(deployer);
        assert!(!exists<MintCapability>(addr), E_ALREADY_INITIALISED);
        move_to(deployer, MintCapability { total_supply: 0 });
        // Give deployer a zero balance.
        move_to(deployer, Balance { value: 0 });
    }

    /// Mint `amount` tokens to `recipient`.  Caller must be the deployer.
    public entry fun mint(
        deployer: &signer,
        recipient: address,
        amount: u64,
    ) acquires MintCapability, Balance {
        let deployer_addr = signer::address_of(deployer);
        assert!(exists<MintCapability>(deployer_addr), E_NOT_AUTHORIZED);

        // Update total supply.
        let cap = borrow_global_mut<MintCapability>(deployer_addr);
        cap.total_supply = cap.total_supply + amount;

        // Credit recipient (create balance if needed -- simplified).
        if (exists<Balance>(recipient)) {
            let bal = borrow_global_mut<Balance>(recipient);
            bal.value = bal.value + amount;
        };
        // Note: in real impl, move_to for new accounts requires signer.
    }

    /// Transfer `amount` tokens from sender to recipient.
    public entry fun transfer(
        sender: &signer,
        recipient: address,
        amount: u64,
    ) acquires Balance {
        let sender_addr = signer::address_of(sender);
        assert!(exists<Balance>(sender_addr), E_NOT_INITIALISED);

        // Debit sender.
        let sender_bal = borrow_global_mut<Balance>(sender_addr);
        assert!(sender_bal.value >= amount, E_INSUFFICIENT_BALANCE);
        sender_bal.value = sender_bal.value - amount;

        // Credit recipient.
        if (exists<Balance>(recipient)) {
            let recv_bal = borrow_global_mut<Balance>(recipient);
            recv_bal.value = recv_bal.value + amount;
        };
    }

    /// Burn `amount` tokens from the sender's balance.
    public entry fun burn(account: &signer, amount: u64) acquires Balance, MintCapability {
        let addr = signer::address_of(account);
        assert!(exists<Balance>(addr), E_NOT_INITIALISED);
        let bal = borrow_global_mut<Balance>(addr);
        assert!(bal.value >= amount, E_INSUFFICIENT_BALANCE);
        bal.value = bal.value - amount;

        // Decrease total supply if deployer has MintCapability.
        if (exists<MintCapability>(addr)) {
            let cap = borrow_global_mut<MintCapability>(addr);
            cap.total_supply = cap.total_supply - amount;
        };
    }

    #[view]
    /// Get balance (view function).
    public fun balance_of(addr: address): u64 acquires Balance {
        if (exists<Balance>(addr)) {
            borrow_global<Balance>(addr).value
        } else {
            0
        }
    }

    #[view]
    /// Get total supply (view function).
    public fun total_supply(deployer: address): u64 acquires MintCapability {
        assert!(exists<MintCapability>(deployer), E_NOT_AUTHORIZED);
        borrow_global<MintCapability>(deployer).total_supply
    }
}
