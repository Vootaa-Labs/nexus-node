/// Test suite for the staking module.
///
/// Covers all P-6 verification conditions plus boundary and error cases.
#[test_only]
module staking_addr::staking_tests {
    use staking_addr::staking;
    use std::signer;

    // -- Helpers ----------------------------------------------------------

    /// 1 NXS in voo.
    const ONE_NXS: u64 = 1_000_000_000;

    fun create_signer(addr: address): signer {
        // In Move test mode, create_signer_for_testing is available.
        std::signer::create_signer_for_testing(addr)
    }

    // -- Test 1: Registration & State Query ------------------------------

    #[test]
    fun test_register_and_query_stake() {
        let admin = create_signer(@staking_addr);
        staking::initialize(&admin);

        let validator = create_signer(@0x1);
        staking::register_validator(&validator, 2 * ONE_NXS, 0, 42);

        // Query should return correct values.
        let (bonded, penalty, effective, status, reg_epoch, unbond_epoch, tag) =
            staking::get_validator_stake(@0x1);
        assert!(bonded == 2 * ONE_NXS, 100);
        assert!(penalty == 0, 101);
        assert!(effective == 2 * ONE_NXS, 102);
        assert!(status == 0, 103); // STATUS_ACTIVE
        assert!(reg_epoch == 0, 104);
        assert!(unbond_epoch == 0, 105);
        assert!(tag == 42, 106);

        // Should be registered and eligible.
        assert!(staking::is_registered(@0x1), 107);
        assert!(staking::is_eligible(@0x1), 108);
        assert!(staking::effective_stake(@0x1) == 2 * ONE_NXS, 109);
    }

    // -- Test 2: Multiple Bonds Accumulate -------------------------------

    #[test]
    fun test_multiple_bonds_accumulate() {
        let admin = create_signer(@staking_addr);
        staking::initialize(&admin);

        let validator = create_signer(@0x2);
        staking::register_validator(&validator, ONE_NXS, 0, 0);

        // Bond additional stake twice.
        staking::bond(&validator, ONE_NXS);
        staking::bond(&validator, 3 * ONE_NXS);

        // Total should be 1 + 1 + 3 = 5 NXS.
        let (bonded, _penalty, effective, _status, _reg, _unbond, _tag) =
            staking::get_validator_stake(@0x2);
        assert!(bonded == 5 * ONE_NXS, 200);
        assert!(effective == 5 * ONE_NXS, 201);

        // Pool stats should reflect the total.
        let (_total_v, _active_v, total_stake) = staking::pool_stats();
        assert!(total_stake == 5 * ONE_NXS, 202);
    }

    // -- Test 3: Unbonding Lifecycle -------------------------------------

    #[test]
    fun test_unbond_and_withdraw_lifecycle() {
        let admin = create_signer(@staking_addr);
        staking::initialize(&admin);

        let validator = create_signer(@0x3);
        staking::register_validator(&validator, 2 * ONE_NXS, 5, 0);

        // Begin unbonding at epoch 10.
        staking::begin_unbond(&validator, 10);

        let status = staking::validator_status(@0x3);
        assert!(status == 1, 300); // STATUS_UNBONDING

        // Not eligible after unbonding.
        assert!(!staking::is_eligible(@0x3), 301);
        assert!(staking::effective_stake(@0x3) == 0, 302);

        // Pool should reflect removal from active set.
        let (_total_v, active_v, _total_stake) = staking::pool_stats();
        assert!(active_v == 0, 303);
    }

    #[test]
    #[expected_failure(abort_code = 7)] // E_UNBONDING_NOT_COMPLETE
    fun test_withdraw_too_early_fails() {
        let admin = create_signer(@staking_addr);
        staking::initialize(&admin);

        let validator = create_signer(@0x4);
        staking::register_validator(&validator, 2 * ONE_NXS, 0, 0);
        staking::begin_unbond(&validator, 10);

        // Try withdrawing at epoch 11 -- unbonding period is 2,
        // so earliest is epoch 12.
        staking::withdraw_unbonded(&validator, 11);
    }

    #[test]
    fun test_withdraw_after_unbonding_period() {
        let admin = create_signer(@staking_addr);
        staking::initialize(&admin);

        let validator = create_signer(@0x5);
        staking::register_validator(&validator, 2 * ONE_NXS, 0, 0);
        staking::begin_unbond(&validator, 10);

        // Withdraw at epoch 12 (unbond_epoch 10 + period 2).
        staking::withdraw_unbonded(&validator, 12);

        let status = staking::validator_status(@0x5);
        assert!(status == 2, 400); // STATUS_WITHDRAWN
    }

    // -- Test 4: Error Cases ---------------------------------------------

    #[test]
    #[expected_failure(abort_code = 1)] // E_ALREADY_REGISTERED
    fun test_duplicate_registration_fails() {
        let admin = create_signer(@staking_addr);
        staking::initialize(&admin);

        let validator = create_signer(@0x6);
        staking::register_validator(&validator, ONE_NXS, 0, 0);
        // Double register should fail.
        staking::register_validator(&validator, ONE_NXS, 0, 0);
    }

    #[test]
    #[expected_failure(abort_code = 3)] // E_ZERO_AMOUNT
    fun test_zero_stake_registration_fails() {
        let admin = create_signer(@staking_addr);
        staking::initialize(&admin);

        let validator = create_signer(@0x7);
        staking::register_validator(&validator, 0, 0, 0);
    }

    #[test]
    #[expected_failure(abort_code = 4)] // E_BELOW_MINIMUM_STAKE
    fun test_below_minimum_stake_fails() {
        let admin = create_signer(@staking_addr);
        staking::initialize(&admin);

        let validator = create_signer(@0x8);
        // 999_999_999 voo -- just below 1 NXS minimum.
        staking::register_validator(&validator, 999_999_999, 0, 0);
    }

    #[test]
    #[expected_failure(abort_code = 3)] // E_ZERO_AMOUNT
    fun test_zero_bond_fails() {
        let admin = create_signer(@staking_addr);
        staking::initialize(&admin);

        let validator = create_signer(@0x9);
        staking::register_validator(&validator, ONE_NXS, 0, 0);
        staking::bond(&validator, 0);
    }

    #[test]
    #[expected_failure(abort_code = 6)] // E_NOT_UNBONDING
    fun test_withdraw_when_not_unbonding_fails() {
        let admin = create_signer(@staking_addr);
        staking::initialize(&admin);

        let validator = create_signer(@0xA);
        staking::register_validator(&validator, ONE_NXS, 0, 0);
        // Try withdrawing without unbonding first.
        staking::withdraw_unbonded(&validator, 100);
    }

    // -- Test 5: Penalty/Slash Effects -----------------------------------

    #[test]
    fun test_penalty_reduces_effective_stake() {
        let admin = create_signer(@staking_addr);
        staking::initialize(&admin);

        let validator = create_signer(@0xB);
        staking::register_validator(&validator, 5 * ONE_NXS, 0, 0);

        // Apply penalty of 2 NXS.
        staking::apply_penalty(&admin, @0xB, 2 * ONE_NXS);

        let effective = staking::effective_stake(@0xB);
        assert!(effective == 3 * ONE_NXS, 500);

        // Pool stats should reflect reduced stake.
        let (_total_v, _active_v, total_stake) = staking::pool_stats();
        assert!(total_stake == 3 * ONE_NXS, 501);

        // Still eligible (3 NXS > 1 NXS minimum).
        assert!(staking::is_eligible(@0xB), 502);
    }

    #[test]
    fun test_penalty_makes_ineligible() {
        let admin = create_signer(@staking_addr);
        staking::initialize(&admin);

        let validator = create_signer(@0xC);
        staking::register_validator(&validator, 2 * ONE_NXS, 0, 0);

        // Penalty leaves effective stake below minimum.
        staking::apply_penalty(&admin, @0xC, ONE_NXS + 1);

        // Effective stake is below 1 NXS -- not eligible.
        assert!(!staking::is_eligible(@0xC), 600);
    }

    #[test]
    #[expected_failure(abort_code = 9)] // E_PENALTY_EXCEEDS_STAKE
    fun test_penalty_exceeds_bonded_fails() {
        let admin = create_signer(@staking_addr);
        staking::initialize(&admin);

        let validator = create_signer(@0xD);
        staking::register_validator(&validator, ONE_NXS, 0, 0);

        // Penalty exceeds bonded amount.
        staking::apply_penalty(&admin, @0xD, ONE_NXS + 1);
    }

    #[test]
    #[expected_failure(abort_code = 11)] // E_NOT_AUTHORIZED
    fun test_penalty_unauthorized_fails() {
        let admin = create_signer(@staking_addr);
        staking::initialize(&admin);

        let validator = create_signer(@0xE);
        staking::register_validator(&validator, ONE_NXS, 0, 0);

        // Non-admin tries to apply penalty.
        let impostor = create_signer(@0xFF);
        staking::apply_penalty(&impostor, @0xE, ONE_NXS);
    }

    // -- Metadata Update -------------------------------------------------

    #[test]
    fun test_set_metadata() {
        let admin = create_signer(@staking_addr);
        staking::initialize(&admin);

        let validator = create_signer(@0xF);
        staking::register_validator(&validator, ONE_NXS, 0, 99);

        staking::set_validator_metadata(&validator, 777);

        let (_b, _p, _e, _s, _r, _u, tag) = staking::get_validator_stake(@0xF);
        assert!(tag == 777, 700);
    }

    // -- Pool Statistics -------------------------------------------------

    #[test]
    fun test_pool_stats_multi_validator() {
        let admin = create_signer(@staking_addr);
        staking::initialize(&admin);

        let v1 = create_signer(@0x10);
        let v2 = create_signer(@0x11);
        let v3 = create_signer(@0x12);

        staking::register_validator(&v1, 3 * ONE_NXS, 0, 0);
        staking::register_validator(&v2, 5 * ONE_NXS, 0, 0);
        staking::register_validator(&v3, 2 * ONE_NXS, 0, 0);

        let (total_v, active_v, total_stake) = staking::pool_stats();
        assert!(total_v == 3, 800);
        assert!(active_v == 3, 801);
        assert!(total_stake == 10 * ONE_NXS, 802);

        // Unbond one validator.
        staking::begin_unbond(&v2, 0);

        let (total_v2, active_v2, total_stake2) = staking::pool_stats();
        assert!(total_v2 == 3, 803);  // still registered
        assert!(active_v2 == 2, 804); // one unbonding
        assert!(total_stake2 == 5 * ONE_NXS, 805); // 3 + 2 = 5 (v2's 5 removed)
    }

    // -- Unregistered Address Queries ------------------------------------

    #[test]
    fun test_unregistered_returns_defaults() {
        let admin = create_signer(@staking_addr);
        staking::initialize(&admin);

        // Not registered -- should return safe defaults.
        assert!(!staking::is_registered(@0x99), 900);
        assert!(!staking::is_eligible(@0x99), 901);
        assert!(staking::effective_stake(@0x99) == 0, 902);
    }
}
