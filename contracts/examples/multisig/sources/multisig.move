/// Multisig -- 2-of-3 multi-signature approval vault.
///
/// Demonstrates multi-party authorisation, threshold logic,
/// and address-based access control.
/// Inspired by Aptos multisig_account patterns.
module multisig_addr::multisig {
    use std::signer;

    /// Error codes.
    const E_ALREADY_EXISTS: u64 = 1;
    const E_NOT_FOUND: u64 = 2;
    const E_NOT_AUTHORIZED: u64 = 3;
    const E_ALREADY_APPROVED: u64 = 4;
    const E_ALREADY_EXECUTED: u64 = 5;
    const E_THRESHOLD_NOT_MET: u64 = 6;

    /// Status constants.
    const STATUS_PENDING: u8 = 0;
    const STATUS_EXECUTED: u8 = 1;

    /// A 2-of-3 multisig vault with a pending value to be set.
    /// Members are owner + signer_a + signer_b.
    /// Any 2 approvals (including the owner's implicit approval
    /// at creation) meet threshold.
    struct Vault has key, store {
        owner: address,
        signer_a: address,
        signer_b: address,
        /// The value that will be "executed" once threshold is met.
        proposed_value: u64,
        /// Approval flags: owner, signer_a, signer_b.
        owner_approved: bool,
        a_approved: bool,
        b_approved: bool,
        approval_count: u64,
        status: u8,
    }

    /// Create a vault with a proposed value.
    /// Owner is implicitly the first approver.
    public entry fun create_vault(
        owner: &signer,
        signer_a: address,
        signer_b: address,
        proposed_value: u64,
    ) {
        let addr = signer::address_of(owner);
        assert!(!exists<Vault>(addr), E_ALREADY_EXISTS);
        move_to(owner, Vault {
            owner: addr,
            signer_a,
            signer_b,
            proposed_value,
            owner_approved: true,
            a_approved: false,
            b_approved: false,
            approval_count: 1,
            status: STATUS_PENDING,
        });
    }

    /// Approve the vault's proposed action.
    /// Caller must be signer_a or signer_b.
    public entry fun approve(
        approver: &signer,
        vault_owner: address,
    ) acquires Vault {
        let approver_addr = signer::address_of(approver);
        assert!(exists<Vault>(vault_owner), E_NOT_FOUND);
        let vault = borrow_global_mut<Vault>(vault_owner);
        assert!(vault.status == STATUS_PENDING, E_ALREADY_EXECUTED);

        if (approver_addr == vault.signer_a) {
            assert!(!vault.a_approved, E_ALREADY_APPROVED);
            vault.a_approved = true;
            vault.approval_count = vault.approval_count + 1;
        } else if (approver_addr == vault.signer_b) {
            assert!(!vault.b_approved, E_ALREADY_APPROVED);
            vault.b_approved = true;
            vault.approval_count = vault.approval_count + 1;
        } else {
            abort E_NOT_AUTHORIZED
        };
    }

    /// Execute the vault action once threshold (2) is met.
    /// Only the owner may execute.
    public entry fun execute(owner: &signer) acquires Vault {
        let addr = signer::address_of(owner);
        assert!(exists<Vault>(addr), E_NOT_FOUND);
        let vault = borrow_global_mut<Vault>(addr);
        assert!(vault.owner == addr, E_NOT_AUTHORIZED);
        assert!(vault.status == STATUS_PENDING, E_ALREADY_EXECUTED);
        assert!(vault.approval_count >= 2, E_THRESHOLD_NOT_MET);
        vault.status = STATUS_EXECUTED;
    }

    #[view]
    /// Get vault status: (proposed_value, approval_count, status).
    public fun get_vault(owner: address): (u64, u64, u8) acquires Vault {
        assert!(exists<Vault>(owner), E_NOT_FOUND);
        let v = borrow_global<Vault>(owner);
        (v.proposed_value, v.approval_count, v.status)
    }

    #[view]
    /// Check if the vault is ready to execute (threshold met).
    public fun is_ready(owner: address): bool acquires Vault {
        assert!(exists<Vault>(owner), E_NOT_FOUND);
        let v = borrow_global<Vault>(owner);
        v.approval_count >= 2 && v.status == STATUS_PENDING
    }
}
