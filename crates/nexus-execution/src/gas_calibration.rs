//! D-1: Gas budget calibration tests.
//!
//! Uses representative workload sizes derived from `contracts/examples/`
//! to verify that the default `query_gas_budget` (10M) and
//! `query_timeout_ms` (5s) are appropriate for real contract queries.
//!
//! ## Methodology
//!
//! For each contract archetype (minimal, moderate, complex), we estimate
//! gas consumption for typical operations (view calls, publishes, writes)
//! using the `GasSchedule` and compare against the 10M budget.
//!
//! ## Contract archetypes (from `contracts/examples/`)
//!
//! | Contract | Representative of      | Bytecode ~size | State reads | State writes |
//! |----------|------------------------|----------------|-------------|--------------|
//! | counter  | Minimal stateful (u64) | ~1 KiB         | ~64 B       | ~64 B        |
//! | token    | Moderate (balances)    | ~4 KiB         | ~256 B      | ~256 B       |
//! | voting   | Complex (collections)  | ~8 KiB         | ~2 KiB      | ~1 KiB       |
//! | escrow   | Multi-party            | ~6 KiB         | ~512 B      | ~512 B       |
//! | multisig | Governance             | ~10 KiB        | ~4 KiB      | ~2 KiB       |
//! | registry | Large state            | ~5 KiB         | ~8 KiB      | ~4 KiB       |

#[cfg(test)]
mod tests {
    use crate::move_adapter::gas_meter::{
        estimate_call_gas, estimate_publish_gas, estimate_query_gas, GasSchedule,
    };
    use crate::types::StateChange;
    use nexus_primitives::AccountAddress;

    /// The default query gas budget from `RpcConfig`.
    const DEFAULT_QUERY_GAS_BUDGET: u64 = 10_000_000;

    /// Default query timeout from `RpcConfig`.
    const DEFAULT_QUERY_TIMEOUT_MS: u64 = 5_000;

    fn default_schedule() -> GasSchedule {
        GasSchedule::default()
    }

    /// Create synthetic state changes of a given count and avg size.
    fn make_state_changes(
        count: usize,
        avg_key_bytes: usize,
        avg_val_bytes: usize,
    ) -> Vec<StateChange> {
        (0..count)
            .map(|i| {
                let full_key = format!("key_{i:08}").into_bytes();
                StateChange {
                    account: AccountAddress::ZERO,
                    key: full_key[..avg_key_bytes.min(full_key.len())].to_vec(),
                    value: Some(vec![0xAB; avg_val_bytes]),
                }
            })
            .collect()
    }

    // ── Archetype: Counter (minimal) ─────────────────────────────────

    /// Counter contract: `get_count(addr)` view query.
    /// Input: 32-byte address. Output: 8-byte u64.
    #[test]
    fn calibrate_counter_view_query() {
        let schedule = default_schedule();
        let args = vec![vec![0u8; 32]]; // address arg
        let output_bytes = 8; // u64 return

        let gas = estimate_query_gas(&schedule, &[], &args, output_bytes);
        // call_base(5000) + read(32) + read(8) = 5040
        assert!(gas < 10_000, "counter view should use < 10K gas, got {gas}");
        assert!(gas < DEFAULT_QUERY_GAS_BUDGET, "well within budget");

        // Budget headroom: how many counter views fit in 10M?
        let headroom = DEFAULT_QUERY_GAS_BUDGET / gas;
        assert!(headroom > 1_000, "budget allows {headroom} counter views");
    }

    /// Counter contract: `increment()` call.
    /// Input: signer. State: read 64B + write 64B.
    #[test]
    fn calibrate_counter_call() {
        let schedule = default_schedule();
        let args = vec![vec![0u8; 32]]; // signer
        let state_changes = make_state_changes(1, 16, 64);

        let gas = estimate_call_gas(&schedule, &[], &args, &state_changes);
        // call_base(5000) + read(32) + write(64*5) = 5000 + 32 + 320 = 5352
        assert!(gas < 10_000, "counter call should use < 10K gas, got {gas}");
    }

    /// Counter contract: publish (1 KiB bytecode).
    #[test]
    fn calibrate_counter_publish() {
        let schedule = default_schedule();
        let module = vec![vec![0u8; 1024]]; // ~1 KiB bytecode
        let state_changes = make_state_changes(1, 16, 1024);

        let gas = estimate_publish_gas(&schedule, &module, &state_changes);
        // publish_base(10000) + 1024*1 + write(1024*5) = 10000 + 1024 + 5120 = 16144
        assert!(
            gas < 20_000,
            "counter publish should use < 20K gas, got {gas}"
        );
    }

    // ── Archetype: Token (moderate) ──────────────────────────────────

    /// Token contract: `balance_of(addr, token_id)` view.
    /// Input: 32B addr + 32B token. Output: 8B u64.
    #[test]
    fn calibrate_token_balance_view() {
        let schedule = default_schedule();
        let args = vec![vec![0u8; 32], vec![0u8; 32]]; // addr + token
        let output_bytes = 8;

        let gas = estimate_query_gas(&schedule, &[], &args, output_bytes);
        // call_base(5000) + read(64) + read(8) = 5072
        assert!(gas < 10_000, "token balance view < 10K gas, got {gas}");
    }

    /// Token contract: `transfer(from, to, amount)`.
    /// State: read 2 balances (256B each), write 2 balances.
    #[test]
    fn calibrate_token_transfer() {
        let schedule = default_schedule();
        let args = vec![vec![0u8; 32], vec![0u8; 32], vec![0u8; 8]]; // from, to, amount
        let state_changes = make_state_changes(2, 16, 256);

        let gas = estimate_call_gas(&schedule, &[], &args, &state_changes);
        // call_base(5000) + read(72) + write(256*2*5) = 5000 + 72 + 2560 = 7632
        assert!(
            gas < 10_000,
            "token transfer should use < 10K gas, got {gas}"
        );
    }

    /// Token contract: publish (~4 KiB bytecode).
    #[test]
    fn calibrate_token_publish() {
        let schedule = default_schedule();
        let module = vec![vec![0u8; 4096]]; // ~4 KiB
        let state_changes = make_state_changes(1, 16, 4096);

        let gas = estimate_publish_gas(&schedule, &module, &state_changes);
        // publish_base(10000) + 4096 + write(4096*5) = 10000 + 4096 + 20480 = 34576
        assert!(
            gas < 50_000,
            "token publish should use < 50K gas, got {gas}"
        );
    }

    // ── Archetype: Voting (complex collections) ──────────────────────

    /// Voting contract: `get_results()` view returning proposal + vote tallies.
    /// Output: ~2 KiB serialized results.
    #[test]
    fn calibrate_voting_results_view() {
        let schedule = default_schedule();
        let args = vec![vec![0u8; 32]]; // proposal ID
        let output_bytes = 2_048;

        let gas = estimate_query_gas(&schedule, &[], &args, output_bytes);
        // call_base(5000) + read(32) + read(2048) = 7080
        assert!(gas < 10_000, "voting results view < 10K gas, got {gas}");
    }

    /// Voting contract: `cast_vote()`.
    /// State: read ~2 KiB, write ~1 KiB.
    #[test]
    fn calibrate_voting_cast_vote() {
        let schedule = default_schedule();
        let args = vec![vec![0u8; 32], vec![0u8; 32], vec![0u8; 1]]; // voter, proposal, choice
        let state_changes = make_state_changes(3, 16, 1024);

        let gas = estimate_call_gas(&schedule, &[], &args, &state_changes);
        // call_base(5000) + read(65) + write(1024*3*5) = 5000 + 65 + 15360 = 20425
        assert!(
            gas < 30_000,
            "voting cast_vote should use < 30K gas, got {gas}"
        );
    }

    // ── Archetype: Multisig (governance, large state) ────────────────

    /// Multisig: `get_pending_transactions()` view returning up to 20 txns.
    /// Output: ~4 KiB.
    #[test]
    fn calibrate_multisig_pending_view() {
        let schedule = default_schedule();
        let args = vec![vec![0u8; 32]]; // multisig address
        let output_bytes = 4_096;

        let gas = estimate_query_gas(&schedule, &[], &args, output_bytes);
        // call_base(5000) + read(32) + read(4096) = 9128
        assert!(gas < 10_000, "multisig pending view < 10K gas, got {gas}");
    }

    /// Multisig: `submit_transaction()`.
    /// State: read 4 KiB config, write 2 KiB new pending entry.
    #[test]
    fn calibrate_multisig_submit() {
        let schedule = default_schedule();
        let args = vec![vec![0u8; 32], vec![0u8; 256]]; // signer + tx payload
        let state_changes = make_state_changes(2, 16, 2048);

        let gas = estimate_call_gas(&schedule, &[], &args, &state_changes);
        // call_base(5000) + read(288) + write(2048*2*5) = 5000 + 288 + 20480 = 25768
        assert!(gas < 30_000, "multisig submit < 30K gas, got {gas}");
    }

    // ── Archetype: Registry (large state) ────────────────────────────

    /// Registry: `lookup(name)` view with 8 KiB response.
    #[test]
    fn calibrate_registry_lookup_view() {
        let schedule = default_schedule();
        let args = vec![vec![0u8; 64]]; // name string
        let output_bytes = 8_192;

        let gas = estimate_query_gas(&schedule, &[], &args, output_bytes);
        // call_base(5000) + read(64) + read(8192) = 13256
        assert!(gas < 15_000, "registry lookup view < 15K gas, got {gas}");
        assert!(gas < DEFAULT_QUERY_GAS_BUDGET);
    }

    /// Registry: `register(name, record)`.
    /// State: write 4 KiB entry + index update.
    #[test]
    fn calibrate_registry_register() {
        let schedule = default_schedule();
        let args = vec![vec![0u8; 32], vec![0u8; 64], vec![0u8; 1024]]; // signer, name, record
        let state_changes = make_state_changes(3, 16, 4096);

        let gas = estimate_call_gas(&schedule, &[], &args, &state_changes);
        // call_base(5000) + read(1120) + write(4096*3*5) = 5000 + 1120 + 61440 = 67560
        assert!(gas < 100_000, "registry register < 100K gas, got {gas}");
    }

    // ── Worst-case stress scenarios ──────────────────────────────────

    /// Maximum realistic view query: 64 KiB response, 1 KiB args.
    #[test]
    fn calibrate_worst_case_view_query() {
        let schedule = default_schedule();
        let args = vec![vec![0u8; 1024]]; // large args
        let output_bytes = 65_536; // 64 KiB response

        let gas = estimate_query_gas(&schedule, &[], &args, output_bytes);
        // call_base(5000) + read(1024) + read(65536) = 71560
        assert!(
            gas < 100_000,
            "worst-case view query should use < 100K gas, got {gas}"
        );
        assert!(
            gas < DEFAULT_QUERY_GAS_BUDGET / 100,
            "worst-case view is still < 1% of 10M budget, got {gas}"
        );
    }

    /// Maximum realistic transaction: publish 512 KiB module + 64 KiB state writes.
    #[test]
    fn calibrate_worst_case_publish() {
        let schedule = default_schedule();
        let module = vec![vec![0u8; 524_288]]; // 512 KiB (max_binary_size)
        let state_changes = make_state_changes(16, 16, 4096);

        let gas = estimate_publish_gas(&schedule, &module, &state_changes);
        // publish_base(10000) + 524288*1 + write(4096*16*5) = 10000 + 524288 + 327680 = 861968
        assert!(
            gas < 1_000_000,
            "worst-case publish should use < 1M gas, got {gas}"
        );
        assert!(
            gas < DEFAULT_QUERY_GAS_BUDGET,
            "worst-case publish fits within 10M query budget, got {gas}"
        );
    }

    /// Maximum realistic call: large args, many state changes.
    #[test]
    fn calibrate_worst_case_call() {
        let schedule = default_schedule();
        let type_args: Vec<Vec<u8>> = (0..4).map(|_| vec![0u8; 64]).collect();
        let args: Vec<Vec<u8>> = (0..8).map(|_| vec![0u8; 1024]).collect();
        let state_changes = make_state_changes(32, 16, 4096);

        let gas = estimate_call_gas(&schedule, &type_args, &args, &state_changes);
        // call_base(5000) + read(256 + 8192) + write(4096*32*5) = 5000 + 8448 + 655360 = 668808
        assert!(
            gas < 1_000_000,
            "worst-case call should use < 1M gas, got {gas}"
        );
    }

    // ── Budget adequacy summary ──────────────────────────────────────

    /// Comprehensive budget adequacy check: ensures the default 10M budget
    /// provides at least 10× headroom over worst-case single operations.
    #[test]
    fn budget_adequacy_10x_headroom() {
        let schedule = default_schedule();

        // Worst-case view.
        let worst_view = estimate_query_gas(&schedule, &[vec![0u8; 1024]], &[], 65_536);
        assert!(
            DEFAULT_QUERY_GAS_BUDGET >= worst_view * 10,
            "10M budget should give 10× headroom over worst view ({worst_view})"
        );

        // Worst-case publish.
        let worst_publish = estimate_publish_gas(
            &schedule,
            &[vec![0u8; 524_288]],
            &make_state_changes(16, 16, 4096),
        );
        assert!(
            DEFAULT_QUERY_GAS_BUDGET >= worst_publish * 10,
            "10M budget should give 10× headroom over worst publish ({worst_publish})"
        );

        // Worst-case call.
        let type_args: Vec<Vec<u8>> = (0..4).map(|_| vec![0u8; 64]).collect();
        let args: Vec<Vec<u8>> = (0..8).map(|_| vec![0u8; 1024]).collect();
        let worst_call = estimate_call_gas(
            &schedule,
            &type_args,
            &args,
            &make_state_changes(32, 16, 4096),
        );
        assert!(
            DEFAULT_QUERY_GAS_BUDGET >= worst_call * 10,
            "10M budget should give 10× headroom over worst call ({worst_call})"
        );
    }

    /// Verify the default timeout allows realistic operations.
    /// A worst-case operation using ~1M gas at ~1M gas/sec processing
    /// would take ~1s — well within the 5s timeout.
    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn timeout_adequacy() {
        // At even a conservative 100K gas/sec processing rate,
        // 10M gas budget would take 100s — which exceeds the timeout.
        // But real operations use much less gas (< 100K), so the
        // effective execution time is well under timeout.
        // The timeout protects against pathological loops, not normal operations.
        assert!(
            DEFAULT_QUERY_TIMEOUT_MS >= 1_000,
            "timeout should be at least 1s"
        );
        assert!(
            DEFAULT_QUERY_TIMEOUT_MS <= 30_000,
            "timeout should not exceed 30s"
        );
    }

    // ── Calibration report (printed for documentation) ───────────────

    /// Generates a calibration summary showing gas costs per contract archetype.
    /// Not an assertion test — produces output for documentation.
    #[test]
    fn calibration_report_summary() {
        let schedule = default_schedule();

        let scenarios: Vec<(&str, &str, u64)> = vec![
            (
                "counter",
                "get_count (view)",
                estimate_query_gas(&schedule, &[], &[vec![0u8; 32]], 8),
            ),
            (
                "counter",
                "increment (call)",
                estimate_call_gas(
                    &schedule,
                    &[],
                    &[vec![0u8; 32]],
                    &make_state_changes(1, 16, 64),
                ),
            ),
            (
                "token",
                "balance_of (view)",
                estimate_query_gas(&schedule, &[], &[vec![0u8; 32], vec![0u8; 32]], 8),
            ),
            (
                "token",
                "transfer (call)",
                estimate_call_gas(
                    &schedule,
                    &[],
                    &[vec![0u8; 32], vec![0u8; 32], vec![0u8; 8]],
                    &make_state_changes(2, 16, 256),
                ),
            ),
            (
                "voting",
                "get_results (view)",
                estimate_query_gas(&schedule, &[], &[vec![0u8; 32]], 2048),
            ),
            (
                "multisig",
                "get_pending (view)",
                estimate_query_gas(&schedule, &[], &[vec![0u8; 32]], 4096),
            ),
            (
                "registry",
                "lookup (view)",
                estimate_query_gas(&schedule, &[], &[vec![0u8; 64]], 8192),
            ),
            (
                "max",
                "worst view (64KiB)",
                estimate_query_gas(&schedule, &[], &[vec![0u8; 1024]], 65_536),
            ),
            (
                "max",
                "worst publish (512KiB)",
                estimate_publish_gas(
                    &schedule,
                    &[vec![0u8; 524_288]],
                    &make_state_changes(16, 16, 4096),
                ),
            ),
        ];

        // All scenarios must fit within the budget.
        for (contract, op, gas) in &scenarios {
            let headroom = DEFAULT_QUERY_GAS_BUDGET / gas.max(&1);
            assert!(
                *gas < DEFAULT_QUERY_GAS_BUDGET,
                "{contract}:{op} uses {gas} gas > budget {DEFAULT_QUERY_GAS_BUDGET}"
            );
            // Log for documentation (visible with `cargo test -- --nocapture`).
            eprintln!("  {contract:>10} | {op:<25} | gas: {gas:>10} | headroom: {headroom:>6}×");
        }
    }
}
