/// Staking -- validator staking lifecycle for Nexus committee rotation.
///
/// Provides the canonical on-chain state that determines which validators
/// are eligible for committee election at each epoch boundary.
///
/// ## Lifecycle
///
/// 1. `register_validator` -- register with an initial bond and optional metadata.
/// 2. `bond`              -- add more stake to an existing registration.
/// 3. `begin_unbond`      -- request withdrawal; starts an unbonding countdown.
/// 4. `withdraw_unbonded` -- claim funds after the unbonding period has elapsed.
/// 5. `set_validator_metadata` -- update human-readable metadata.
///
/// ## Design Decisions
///
/// - All stake amounts are in the smallest unit (voo).  1 NXS = 10^9 voo.
/// - The unbonding period is measured in epochs, not wall time.
/// - Stake changes within an epoch take effect at the **next** election
///   boundary, ensuring deterministic committee derivation from a committed
///   state root.
/// - Slashing is applied externally via `apply_penalty`; the staking module
///   does not decide *when* to slash -- it records the economic consequence.
module staking_addr::staking {
    use std::signer;

    // -- Error Codes ----------------------------------------------------------

    /// Validator already registered at this address.
    const E_ALREADY_REGISTERED: u64 = 1;
    /// Validator not found at the given address.
    const E_NOT_REGISTERED: u64 = 2;
    /// Bond amount is zero.
    const E_ZERO_AMOUNT: u64 = 3;
    /// Bond amount is below the minimum required stake.
    const E_BELOW_MINIMUM_STAKE: u64 = 4;
    /// Validator is already in an unbonding state.
    const E_ALREADY_UNBONDING: u64 = 5;
    /// Validator is not in an unbonding state.
    const E_NOT_UNBONDING: u64 = 6;
    /// Unbonding period has not elapsed.
    const E_UNBONDING_NOT_COMPLETE: u64 = 7;
    /// Validator has been slashed and cannot perform this operation.
    const E_SLASHED: u64 = 8;
    /// Penalty exceeds available stake.
    const E_PENALTY_EXCEEDS_STAKE: u64 = 9;
    /// Validator is not active (wrong status).
    const E_NOT_ACTIVE: u64 = 10;
    /// Caller is not the staking admin (for penalty application).
    const E_NOT_AUTHORIZED: u64 = 11;

    // -- Validator Status -----------------------------------------------------

    /// Validator is registered and actively staked.
    const STATUS_ACTIVE: u8 = 0;
    /// Validator has requested unbonding; stake is locked until countdown ends.
    const STATUS_UNBONDING: u8 = 1;
    /// Validator has fully withdrawn; no longer eligible for election.
    const STATUS_WITHDRAWN: u8 = 2;

    // -- Protocol Parameters --------------------------------------------------

    /// Minimum self-stake required to register (1 NXS = 10^9 voo).
    const MIN_STAKE_VOO: u64 = 1_000_000_000;

    /// Number of epochs a validator must wait after `begin_unbond` before
    /// funds can be withdrawn.
    const UNBONDING_PERIOD_EPOCHS: u64 = 2;

    // -- Resources ------------------------------------------------------------

    /// The primary staking record for a validator, stored under the
    /// validator's own account address.
    ///
    /// **Election rule**: a validator is a valid candidate for the next
    /// committee if and only if `status == STATUS_ACTIVE` and
    /// `effective_stake() > 0`.
    struct ValidatorStake has key, store {
        /// Total bonded stake in voo.
        bonded: u64,
        /// Cumulative penalties (slashing) applied to this validator.
        /// Effective stake = bonded - penalty_total.
        penalty_total: u64,
        /// Current lifecycle status.
        status: u8,
        /// Epoch at which the validator registered.
        registered_epoch: u64,
        /// Epoch at which unbonding was requested (0 if not unbonding).
        unbond_epoch: u64,
        /// Human-readable UTF-8 tag (e.g. moniker or endpoint URL).
        /// Stored as a u64 hash to avoid dynamic-length dependencies.
        metadata_tag: u64,
    }

    /// Admin capability for penalty operations.  Published once by the
    /// module deployer during `initialize`; required to call `apply_penalty`.
    struct StakingAdmin has key, store {
        /// Address of the admin (the deployer).
        admin: address,
    }

    /// Global staking statistics -- single instance under the deployer.
    struct StakingPool has key, store {
        /// Number of currently registered validators (any status).
        total_validators: u64,
        /// Number of validators with STATUS_ACTIVE.
        active_validators: u64,
        /// Sum of all effective stake across active validators.
        total_effective_stake: u64,
    }

    // -- Initialization -------------------------------------------------------

    /// One-time module initialization by the deployer.
    /// Creates the admin capability and the global pool tracker.
    public entry fun initialize(deployer: &signer) {
        let addr = signer::address_of(deployer);
        assert!(!exists<StakingAdmin>(addr), E_ALREADY_REGISTERED);
        move_to(deployer, StakingAdmin { admin: addr });
        move_to(deployer, StakingPool {
            total_validators: 0,
            active_validators: 0,
            total_effective_stake: 0,
        });
    }

    // -- Registration ---------------------------------------------------------

    /// Register a new validator with an initial stake.
    ///
    /// The caller becomes the validator; `initial_stake` must be at least
    /// `MIN_STAKE_VOO`.  `current_epoch` is provided by the runtime so
    /// the contract can track when the validator joined.
    public entry fun register_validator(
        account: &signer,
        initial_stake: u64,
        current_epoch: u64,
        metadata_tag: u64,
    ) acquires StakingPool {
        let addr = signer::address_of(account);
        assert!(!exists<ValidatorStake>(addr), E_ALREADY_REGISTERED);
        assert!(initial_stake > 0, E_ZERO_AMOUNT);
        assert!(initial_stake >= MIN_STAKE_VOO, E_BELOW_MINIMUM_STAKE);

        move_to(account, ValidatorStake {
            bonded: initial_stake,
            penalty_total: 0,
            status: STATUS_ACTIVE,
            registered_epoch: current_epoch,
            unbond_epoch: 0,
            metadata_tag,
        });

        // Update global pool.
        let pool = borrow_global_mut<StakingPool>(@staking_addr);
        pool.total_validators = pool.total_validators + 1;
        pool.active_validators = pool.active_validators + 1;
        pool.total_effective_stake = pool.total_effective_stake + initial_stake;
    }

    // -- Bonding --------------------------------------------------------------

    /// Add additional stake to an existing active validator registration.
    public entry fun bond(
        account: &signer,
        amount: u64,
    ) acquires ValidatorStake, StakingPool {
        let addr = signer::address_of(account);
        assert!(exists<ValidatorStake>(addr), E_NOT_REGISTERED);
        assert!(amount > 0, E_ZERO_AMOUNT);

        let vs = borrow_global_mut<ValidatorStake>(addr);
        assert!(vs.status == STATUS_ACTIVE, E_NOT_ACTIVE);

        vs.bonded = vs.bonded + amount;

        // Update pool effective stake.
        let pool = borrow_global_mut<StakingPool>(@staking_addr);
        pool.total_effective_stake = pool.total_effective_stake + amount;
    }

    // -- Unbonding ------------------------------------------------------------

    /// Request to unbond.  The validator enters the unbonding period and
    /// becomes ineligible for future committee elections immediately.
    /// Funds cannot be withdrawn until `UNBONDING_PERIOD_EPOCHS` have
    /// passed since `current_epoch`.
    public entry fun begin_unbond(
        account: &signer,
        current_epoch: u64,
    ) acquires ValidatorStake, StakingPool {
        let addr = signer::address_of(account);
        assert!(exists<ValidatorStake>(addr), E_NOT_REGISTERED);

        let vs = borrow_global_mut<ValidatorStake>(addr);
        assert!(vs.status == STATUS_ACTIVE, E_NOT_ACTIVE);
        // Cannot unbond if already unbonding.
        assert!(vs.status != STATUS_UNBONDING, E_ALREADY_UNBONDING);

        let effective = vs.bonded - vs.penalty_total;

        vs.status = STATUS_UNBONDING;
        vs.unbond_epoch = current_epoch;

        // Update pool: validator is no longer active.
        let pool = borrow_global_mut<StakingPool>(@staking_addr);
        pool.active_validators = pool.active_validators - 1;
        pool.total_effective_stake = pool.total_effective_stake - effective;
    }

    /// Withdraw unbonded funds after the unbonding period has elapsed.
    /// Validator transitions to WITHDRAWN and is permanently removed
    /// from election eligibility.
    public entry fun withdraw_unbonded(
        account: &signer,
        current_epoch: u64,
    ) acquires ValidatorStake, StakingPool {
        let addr = signer::address_of(account);
        assert!(exists<ValidatorStake>(addr), E_NOT_REGISTERED);

        let vs = borrow_global_mut<ValidatorStake>(addr);
        assert!(vs.status == STATUS_UNBONDING, E_NOT_UNBONDING);
        assert!(
            current_epoch >= vs.unbond_epoch + UNBONDING_PERIOD_EPOCHS,
            E_UNBONDING_NOT_COMPLETE,
        );

        vs.status = STATUS_WITHDRAWN;
        // bonded stays for audit; effective stake is 0 due to status.

        // Pool counts were already decremented in begin_unbond.
        // Decrement total (not active) to reflect full exit.
        let pool = borrow_global_mut<StakingPool>(@staking_addr);
        pool.total_validators = pool.total_validators - 1;
    }

    // -- Metadata -------------------------------------------------------------

    /// Update the validator's metadata tag.
    public entry fun set_validator_metadata(
        account: &signer,
        new_tag: u64,
    ) acquires ValidatorStake {
        let addr = signer::address_of(account);
        assert!(exists<ValidatorStake>(addr), E_NOT_REGISTERED);
        let vs = borrow_global_mut<ValidatorStake>(addr);
        vs.metadata_tag = new_tag;
    }

    // -- Penalty / Slash Interface --------------------------------------------

    /// Apply an economic penalty to a validator's stake.
    ///
    /// Called by the staking admin (consensus bridge) when slashing or
    /// offline penalties are determined.  The penalty is recorded as a
    /// cumulative deduction from the validator's bonded amount.
    ///
    /// **Invariant**: `penalty_total <= bonded` at all times.
    ///
    /// If the penalty reduces effective stake below `MIN_STAKE_VOO`, the
    /// validator remains registered but will be filtered out by the
    /// election policy (effective stake too low).
    public entry fun apply_penalty(
        admin: &signer,
        validator_addr: address,
        penalty_amount: u64,
    ) acquires StakingAdmin, ValidatorStake, StakingPool {
        // Authorisation: caller must be the staking admin.
        let admin_addr = signer::address_of(admin);
        assert!(exists<StakingAdmin>(admin_addr), E_NOT_AUTHORIZED);
        let sa = borrow_global<StakingAdmin>(admin_addr);
        assert!(sa.admin == admin_addr, E_NOT_AUTHORIZED);

        assert!(exists<ValidatorStake>(validator_addr), E_NOT_REGISTERED);
        assert!(penalty_amount > 0, E_ZERO_AMOUNT);

        let vs = borrow_global_mut<ValidatorStake>(validator_addr);
        // Penalty cannot exceed remaining bonded amount.
        assert!(vs.penalty_total + penalty_amount <= vs.bonded, E_PENALTY_EXCEEDS_STAKE);

        let old_effective = vs.bonded - vs.penalty_total;
        vs.penalty_total = vs.penalty_total + penalty_amount;
        let new_effective = vs.bonded - vs.penalty_total;

        // Update pool effective stake if validator is active.
        if (vs.status == STATUS_ACTIVE) {
            let pool = borrow_global_mut<StakingPool>(@staking_addr);
            pool.total_effective_stake = pool.total_effective_stake - (old_effective - new_effective);
        };
    }

    // -- View Functions (Read-Only ABI) ---------------------------------------

    #[view]
    /// Get the full staking record for a validator.
    /// Returns: (bonded, penalty_total, effective_stake, status,
    ///           registered_epoch, unbond_epoch, metadata_tag)
    public fun get_validator_stake(
        addr: address,
    ): (u64, u64, u64, u8, u64, u64, u64) acquires ValidatorStake {
        assert!(exists<ValidatorStake>(addr), E_NOT_REGISTERED);
        let vs = borrow_global<ValidatorStake>(addr);
        let effective = if (vs.bonded > vs.penalty_total) {
            vs.bonded - vs.penalty_total
        } else {
            0
        };
        (
            vs.bonded,
            vs.penalty_total,
            effective,
            vs.status,
            vs.registered_epoch,
            vs.unbond_epoch,
            vs.metadata_tag,
        )
    }

    #[view]
    /// Get the effective stake for a validator (bonded - penalty).
    /// Returns 0 if the validator is not active or not registered.
    public fun effective_stake(addr: address): u64 acquires ValidatorStake {
        if (!exists<ValidatorStake>(addr)) {
            return 0
        };
        let vs = borrow_global<ValidatorStake>(addr);
        if (vs.status != STATUS_ACTIVE) {
            return 0
        };
        if (vs.bonded > vs.penalty_total) {
            vs.bonded - vs.penalty_total
        } else {
            0
        }
    }

    #[view]
    /// Check whether a validator is active and eligible for election.
    /// A validator is eligible if status == ACTIVE and effective stake
    /// meets the minimum threshold.
    public fun is_eligible(addr: address): bool acquires ValidatorStake {
        if (!exists<ValidatorStake>(addr)) {
            return false
        };
        let vs = borrow_global<ValidatorStake>(addr);
        if (vs.status != STATUS_ACTIVE) {
            return false
        };
        let effective = if (vs.bonded > vs.penalty_total) {
            vs.bonded - vs.penalty_total
        } else {
            0
        };
        effective >= MIN_STAKE_VOO
    }

    #[view]
    /// Get the validator's current status code.
    public fun validator_status(addr: address): u8 acquires ValidatorStake {
        assert!(exists<ValidatorStake>(addr), E_NOT_REGISTERED);
        borrow_global<ValidatorStake>(addr).status
    }

    #[view]
    /// Get global staking pool statistics.
    /// Returns: (total_validators, active_validators, total_effective_stake)
    public fun pool_stats(): (u64, u64, u64) acquires StakingPool {
        let pool = borrow_global<StakingPool>(@staking_addr);
        (pool.total_validators, pool.active_validators, pool.total_effective_stake)
    }

    #[view]
    /// Check whether a validator address is registered (any status).
    public fun is_registered(addr: address): bool {
        exists<ValidatorStake>(addr)
    }

    // -- Test Helpers ---------------------------------------------------------

    #[test_only]
    /// Return the minimum stake constant for test assertions.
    public fun min_stake(): u64 {
        MIN_STAKE_VOO
    }

    #[test_only]
    /// Return the unbonding period constant for test assertions.
    public fun unbonding_period(): u64 {
        UNBONDING_PERIOD_EPOCHS
    }
}
