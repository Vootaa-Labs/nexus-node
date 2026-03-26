/// Move Prover specification for the staking module.
///
/// FV-6: Minimal verification — key invariants for validator lifecycle,
/// stake accounting, and pool consistency.
///
/// Status: COMPLETE
/// Anchor: contracts/staking/sources/staking.move
/// Object: VO-ST-STAKING
///
/// Run:  move prove --path contracts/staking --named-addresses staking_addr=0xBEEF
///
/// These specs can also be placed inline in staking.move, but a separate
/// spec file keeps the contract source clean and the prover artifacts
/// co-located with other formal verification proofs.
spec staking_addr::staking {

    // ═══════════════════════════════════════════════════════════════
    // Module-Level Invariants (checked after every public function)
    // ═══════════════════════════════════════════════════════════════

    /// INV-1: Penalty can never exceed bonded stake.
    /// This is the core safety invariant — it prevents negative effective
    /// stake and ensures economic soundness.
    invariant forall addr: address where exists<ValidatorStake>(addr):
        global<ValidatorStake>(addr).penalty_total <= global<ValidatorStake>(addr).bonded;

    /// INV-2: Status is always a valid lifecycle value (0, 1, or 2).
    invariant forall addr: address where exists<ValidatorStake>(addr):
        global<ValidatorStake>(addr).status == 0
        || global<ValidatorStake>(addr).status == 1
        || global<ValidatorStake>(addr).status == 2;

    /// INV-3: Unbond epoch is zero unless the validator is in
    /// STATUS_UNBONDING or STATUS_WITHDRAWN.
    invariant forall addr: address where exists<ValidatorStake>(addr):
        (global<ValidatorStake>(addr).status == 0)
            ==> (global<ValidatorStake>(addr).unbond_epoch == 0);

    /// INV-4: Active validators always have stake at or above minimum.
    /// (After slashing, effective stake may go below minimum, but the
    ///  election policy filters them out — the resource itself stays.)
    invariant forall addr: address where exists<ValidatorStake>(addr):
        (global<ValidatorStake>(addr).status == 0)
            ==> (global<ValidatorStake>(addr).bonded >= 1_000_000_000);

    // ═══════════════════════════════════════════════════════════════
    // Function-Level Specifications
    // ═══════════════════════════════════════════════════════════════

    // ─── initialize ─────────────────────────────────────────────────

    spec initialize {
        let addr = signer::address_of(deployer);
        aborts_if exists<StakingAdmin>(addr);
        ensures exists<StakingAdmin>(addr);
        ensures exists<StakingPool>(addr);
        ensures global<StakingPool>(addr).total_validators == 0;
        ensures global<StakingPool>(addr).active_validators == 0;
        ensures global<StakingPool>(addr).total_effective_stake == 0;
    }

    // ─── register_validator ─────────────────────────────────────────

    spec register_validator {
        let addr = signer::address_of(account);
        let post vs = global<ValidatorStake>(addr);

        aborts_if exists<ValidatorStake>(addr);
        aborts_if initial_stake == 0;
        aborts_if initial_stake < 1_000_000_000;

        ensures exists<ValidatorStake>(addr);
        ensures vs.bonded == initial_stake;
        ensures vs.penalty_total == 0;
        ensures vs.status == 0;
        ensures vs.registered_epoch == current_epoch;
        ensures vs.unbond_epoch == 0;
        ensures vs.metadata_tag == metadata_tag;
    }

    // ─── bond ───────────────────────────────────────────────────────

    spec bond {
        let addr = signer::address_of(account);
        let pre_vs = global<ValidatorStake>(addr);
        let post post_vs = global<ValidatorStake>(addr);

        aborts_if !exists<ValidatorStake>(addr);
        aborts_if amount == 0;
        aborts_if pre_vs.status != 0;

        /// Bonded amount increases by exactly the deposit.
        ensures post_vs.bonded == pre_vs.bonded + amount;
        /// Penalty is untouched.
        ensures post_vs.penalty_total == pre_vs.penalty_total;
        /// Status unchanged.
        ensures post_vs.status == pre_vs.status;
    }

    // ─── begin_unbond ───────────────────────────────────────────────

    spec begin_unbond {
        let addr = signer::address_of(account);
        let pre_vs = global<ValidatorStake>(addr);
        let post post_vs = global<ValidatorStake>(addr);

        aborts_if !exists<ValidatorStake>(addr);
        aborts_if pre_vs.status != 0;

        /// Transitions to STATUS_UNBONDING.
        ensures post_vs.status == 1;
        /// Records the unbond epoch.
        ensures post_vs.unbond_epoch == current_epoch;
        /// Bonded amount is unchanged.
        ensures post_vs.bonded == pre_vs.bonded;
    }

    // ─── withdraw_unbonded ──────────────────────────────────────────

    spec withdraw_unbonded {
        let addr = signer::address_of(account);
        let pre_vs = global<ValidatorStake>(addr);
        let post post_vs = global<ValidatorStake>(addr);

        aborts_if !exists<ValidatorStake>(addr);
        aborts_if pre_vs.status != 1;
        aborts_if current_epoch < pre_vs.unbond_epoch + 2;

        /// Transition to STATUS_WITHDRAWN.
        ensures post_vs.status == 2;
        /// Bonded preserved for audit trail.
        ensures post_vs.bonded == pre_vs.bonded;
    }

    // ─── apply_penalty ──────────────────────────────────────────────

    spec apply_penalty {
        let admin_addr = signer::address_of(admin);
        let pre_vs = global<ValidatorStake>(validator_addr);
        let post post_vs = global<ValidatorStake>(validator_addr);

        aborts_if !exists<StakingAdmin>(admin_addr);
        aborts_if global<StakingAdmin>(admin_addr).admin != admin_addr;
        aborts_if !exists<ValidatorStake>(validator_addr);
        aborts_if penalty_amount == 0;
        aborts_if pre_vs.penalty_total + penalty_amount > pre_vs.bonded;

        /// Penalty accumulates exactly.
        ensures post_vs.penalty_total == pre_vs.penalty_total + penalty_amount;
        /// Bonded stake is unchanged (penalty is a separate accounting field).
        ensures post_vs.bonded == pre_vs.bonded;
        /// Status unchanged by penalty alone.
        ensures post_vs.status == pre_vs.status;
    }

    // ─── set_validator_metadata ─────────────────────────────────────

    spec set_validator_metadata {
        let addr = signer::address_of(account);
        let post vs = global<ValidatorStake>(addr);

        aborts_if !exists<ValidatorStake>(addr);

        ensures vs.metadata_tag == new_tag;
        /// All other fields unchanged.
        ensures vs.bonded == old(global<ValidatorStake>(addr)).bonded;
        ensures vs.status == old(global<ValidatorStake>(addr)).status;
    }

    // ─── View functions: verified to never abort for registered validators ──

    spec effective_stake {
        aborts_if false;
    }

    spec is_eligible {
        aborts_if false;
    }

    spec is_registered {
        aborts_if false;
    }

    spec get_validator_stake {
        aborts_if !exists<ValidatorStake>(addr);
    }

    spec validator_status {
        aborts_if !exists<ValidatorStake>(addr);
    }

    spec pool_stats {
        aborts_if !exists<StakingPool>(@staking_addr);
    }
}
