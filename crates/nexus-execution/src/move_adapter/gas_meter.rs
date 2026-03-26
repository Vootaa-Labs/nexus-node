// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Gas metering and limit control for Move VM execution.
//!
//! Provides a [`GasMeter`] trait and a [`SimpleGasMeter`] implementation
//! that tracks gas consumption against a budget using saturating arithmetic.
//!
//! The gas costs are derived from [`VmConfig`](super::VmConfig) via the
//! [`GasSchedule`] struct, which names every unit cost used by the VM.

// Items in this module are foundational for T-2005/T-2006; not yet consumed.
#![allow(dead_code)]

use super::vm_config::VmConfig;

// ── Error type ──────────────────────────────────────────────────────────

/// Error returned when a gas charge exceeds the remaining budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GasExhausted {
    /// Gas units the operation needed.
    pub needed: u64,
    /// Gas units that were available.
    pub available: u64,
}

impl std::fmt::Display for GasExhausted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "out of gas: needed {} but only {} available",
            self.needed, self.available
        )
    }
}

impl std::error::Error for GasExhausted {}

// ── GasMeter trait ──────────────────────────────────────────────────────

/// Trait for tracking gas consumption during transaction execution.
///
/// Implementors must use **saturating arithmetic** so that overflow never
/// causes a panic.  Charging more gas than available returns
/// [`GasExhausted`] but leaves the meter in a well-defined state.
pub(crate) trait GasMeter: Send + Sync {
    /// Attempt to consume `amount` gas units.
    ///
    /// Returns `Ok(())` if sufficient gas remains, or [`GasExhausted`]
    /// otherwise.  On exhaustion the meter's consumed count remains
    /// unchanged (charge-or-nothing semantics).
    fn charge(&mut self, amount: u64) -> Result<(), GasExhausted>;

    /// Remaining gas budget.
    fn remaining(&self) -> u64;

    /// Total gas consumed so far.
    fn consumed(&self) -> u64;

    /// Original gas limit (consumed + remaining).
    fn limit(&self) -> u64;
}

// ── SimpleGasMeter ──────────────────────────────────────────────────────

/// A straightforward gas meter that tracks consumed gas against a fixed limit.
///
/// ```text
///   ┌────────────────────────────────────────┐
///   │  consumed  │       remaining           │
///   └────────────────────────────────────────┘
///   0           consumed                    limit
/// ```
pub(crate) struct SimpleGasMeter {
    /// Maximum gas allowed for this execution.
    limit: u64,
    /// Gas consumed so far.
    consumed: u64,
}

impl SimpleGasMeter {
    /// Create a new meter with the given gas limit.
    pub fn new(limit: u64) -> Self {
        Self { limit, consumed: 0 }
    }
}

impl GasMeter for SimpleGasMeter {
    fn charge(&mut self, amount: u64) -> Result<(), GasExhausted> {
        let new_consumed = self.consumed.saturating_add(amount);
        if new_consumed > self.limit {
            Err(GasExhausted {
                needed: amount,
                available: self.remaining(),
            })
        } else {
            self.consumed = new_consumed;
            Ok(())
        }
    }

    #[inline]
    fn remaining(&self) -> u64 {
        self.limit.saturating_sub(self.consumed)
    }

    #[inline]
    fn consumed(&self) -> u64 {
        self.consumed
    }

    #[inline]
    fn limit(&self) -> u64 {
        self.limit
    }
}

// ── GasSchedule ─────────────────────────────────────────────────────────

/// Named gas costs for all metered operations.
///
/// Derived from [`VmConfig`] and used by the executor and Move VM adapter
/// to charge the correct amounts through a [`GasMeter`].
#[derive(Debug, Clone)]
pub(crate) struct GasSchedule {
    /// Base gas for a simple native transfer.
    pub transfer_base: u64,
    /// Base gas for a Move function call.
    pub call_base: u64,
    /// Base gas for publishing modules.
    pub publish_base: u64,
    /// Per-byte gas for storing module bytecode.
    pub publish_per_byte: u64,
    /// Per-byte gas for reading from state.
    pub read_per_byte: u64,
    /// Per-byte gas for writing to state.
    pub write_per_byte: u64,
}

/// Fixed gas cost for a native token transfer.
const DEFAULT_TRANSFER_BASE: u64 = 1_000;

impl GasSchedule {
    /// Derive a gas schedule from a [`VmConfig`].
    pub fn from_config(config: &VmConfig) -> Self {
        Self {
            transfer_base: DEFAULT_TRANSFER_BASE,
            call_base: config.call_base_gas,
            publish_base: config.publish_base_gas,
            publish_per_byte: config.publish_per_byte_gas,
            read_per_byte: config.read_per_byte_gas,
            write_per_byte: config.write_per_byte_gas,
        }
    }
}

impl Default for GasSchedule {
    fn default() -> Self {
        Self::from_config(&VmConfig::default())
    }
}

// ── Gas calculation helpers ─────────────────────────────────────────────

/// Calculate gas for publishing `total_bytes` of module bytecode.
///
/// `gas = publish_base + total_bytes × publish_per_byte`
///
/// Uses saturating arithmetic.
#[inline]
pub(crate) fn publish_gas_cost(schedule: &GasSchedule, total_bytes: u64) -> u64 {
    schedule
        .publish_base
        .saturating_add(total_bytes.saturating_mul(schedule.publish_per_byte))
}

/// Calculate gas for a state write of `size` bytes.
///
/// `gas = size × write_per_byte`
#[inline]
pub(crate) fn write_gas_cost(schedule: &GasSchedule, size: u64) -> u64 {
    size.saturating_mul(schedule.write_per_byte)
}

/// Calculate gas for a state read of `size` bytes.
///
/// `gas = size × read_per_byte`
#[inline]
pub(crate) fn read_gas_cost(schedule: &GasSchedule, size: u64) -> u64 {
    size.saturating_mul(schedule.read_per_byte)
}

#[inline]
fn encoded_chunks_len(chunks: &[Vec<u8>]) -> u64 {
    chunks.iter().fold(0u64, |total, chunk| {
        total.saturating_add(chunk.len() as u64)
    })
}

#[inline]
fn state_change_bytes(state_changes: &[crate::types::StateChange]) -> u64 {
    state_changes.iter().fold(0u64, |total, change| {
        let key_len = change.key.len() as u64;
        let value_len = change
            .value
            .as_ref()
            .map(|value| value.len() as u64)
            .unwrap_or(0);
        total.saturating_add(key_len).saturating_add(value_len)
    })
}

#[inline]
pub(crate) fn clamp_gas_to_limit(estimated: u64, gas_limit: u64) -> u64 {
    estimated.min(gas_limit)
}

#[inline]
pub(crate) fn estimate_call_gas(
    schedule: &GasSchedule,
    type_args: &[Vec<u8>],
    args: &[Vec<u8>],
    state_changes: &[crate::types::StateChange],
) -> u64 {
    let input_bytes = encoded_chunks_len(type_args).saturating_add(encoded_chunks_len(args));
    schedule
        .call_base
        .saturating_add(read_gas_cost(schedule, input_bytes))
        .saturating_add(write_gas_cost(schedule, state_change_bytes(state_changes)))
}

#[inline]
pub(crate) fn estimate_publish_gas(
    schedule: &GasSchedule,
    modules: &[Vec<u8>],
    state_changes: &[crate::types::StateChange],
) -> u64 {
    let module_bytes = encoded_chunks_len(modules);
    publish_gas_cost(schedule, module_bytes)
        .saturating_add(write_gas_cost(schedule, state_change_bytes(state_changes)))
}

#[inline]
pub(crate) fn estimate_script_gas(
    schedule: &GasSchedule,
    bytecode: &[u8],
    type_args: &[Vec<u8>],
    args: &[Vec<u8>],
) -> u64 {
    let input_bytes = (bytecode.len() as u64)
        .saturating_add(encoded_chunks_len(type_args))
        .saturating_add(encoded_chunks_len(args));
    schedule
        .call_base
        .saturating_add(read_gas_cost(schedule, input_bytes))
}

/// Estimate gas for a read-only view function query.
///
/// `gas = call_base + read_per_byte × (input_bytes + output_bytes)`
///
/// This provides meaningful cost attribution for queries without
/// requiring full VM-level instruction metering.
#[inline]
pub(crate) fn estimate_query_gas(
    schedule: &GasSchedule,
    type_args: &[Vec<u8>],
    args: &[Vec<u8>],
    output_bytes: u64,
) -> u64 {
    let input_bytes = encoded_chunks_len(type_args).saturating_add(encoded_chunks_len(args));
    schedule
        .call_base
        .saturating_add(read_gas_cost(schedule, input_bytes))
        .saturating_add(read_gas_cost(schedule, output_bytes))
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── SimpleGasMeter tests ────────────────────────────────────────

    #[test]
    fn new_meter_starts_empty() {
        let meter = SimpleGasMeter::new(100);
        assert_eq!(meter.consumed(), 0);
        assert_eq!(meter.remaining(), 100);
        assert_eq!(meter.limit(), 100);
    }

    #[test]
    fn charge_success() {
        let mut meter = SimpleGasMeter::new(1_000);
        assert!(meter.charge(300).is_ok());
        assert_eq!(meter.consumed(), 300);
        assert_eq!(meter.remaining(), 700);
    }

    #[test]
    fn charge_exact_limit() {
        let mut meter = SimpleGasMeter::new(500);
        assert!(meter.charge(500).is_ok());
        assert_eq!(meter.consumed(), 500);
        assert_eq!(meter.remaining(), 0);
    }

    #[test]
    fn charge_exceeds_limit() {
        let mut meter = SimpleGasMeter::new(100);
        let err = meter.charge(150).unwrap_err();
        assert_eq!(err.needed, 150);
        assert_eq!(err.available, 100);
        // Consumed should remain unchanged (charge-or-nothing).
        assert_eq!(meter.consumed(), 0);
    }

    #[test]
    fn multiple_charges() {
        let mut meter = SimpleGasMeter::new(1_000);
        assert!(meter.charge(200).is_ok());
        assert!(meter.charge(300).is_ok());
        assert_eq!(meter.consumed(), 500);
        assert_eq!(meter.remaining(), 500);
        // Exhaust remaining.
        let err = meter.charge(600).unwrap_err();
        assert_eq!(err.needed, 600);
        assert_eq!(err.available, 500);
        assert_eq!(meter.consumed(), 500); // unchanged
    }

    #[test]
    fn zero_limit_rejects_nonzero_charge() {
        let mut meter = SimpleGasMeter::new(0);
        let err = meter.charge(1).unwrap_err();
        assert_eq!(err.needed, 1);
        assert_eq!(err.available, 0);
    }

    #[test]
    fn zero_charge_always_succeeds() {
        let mut meter = SimpleGasMeter::new(0);
        assert!(meter.charge(0).is_ok());
        assert_eq!(meter.consumed(), 0);
    }

    #[test]
    fn saturating_arithmetic_no_panic() {
        let mut meter = SimpleGasMeter::new(u64::MAX - 10);
        assert!(meter.charge(u64::MAX - 11).is_ok());
        // Remaining is 1, but charging 2 would overflow without saturating.
        let err = meter.charge(2).unwrap_err();
        assert_eq!(err.needed, 2);
        assert_eq!(err.available, 1);
    }

    // ── GasSchedule tests ───────────────────────────────────────────

    #[test]
    fn default_schedule_matches_config() {
        let schedule = GasSchedule::default();
        let config = VmConfig::default();
        assert_eq!(schedule.call_base, config.call_base_gas);
        assert_eq!(schedule.publish_base, config.publish_base_gas);
        assert_eq!(schedule.publish_per_byte, config.publish_per_byte_gas);
        assert_eq!(schedule.read_per_byte, config.read_per_byte_gas);
        assert_eq!(schedule.write_per_byte, config.write_per_byte_gas);
        assert_eq!(schedule.transfer_base, DEFAULT_TRANSFER_BASE);
    }

    #[test]
    fn schedule_from_custom_config() {
        let config = VmConfig {
            call_base_gas: 7_777,
            publish_base_gas: 15_000,
            publish_per_byte_gas: 3,
            read_per_byte_gas: 2,
            write_per_byte_gas: 10,
            ..VmConfig::default()
        };
        let schedule = GasSchedule::from_config(&config);
        assert_eq!(schedule.call_base, 7_777);
        assert_eq!(schedule.publish_base, 15_000);
        assert_eq!(schedule.publish_per_byte, 3);
    }

    // ── Gas calculation helper tests ────────────────────────────────

    #[test]
    fn publish_gas_cost_calculation() {
        let schedule = GasSchedule::default();
        // 100 bytes: 10_000 + 100 × 1 = 10_100
        assert_eq!(publish_gas_cost(&schedule, 100), 10_100);
        // 0 bytes: just base
        assert_eq!(publish_gas_cost(&schedule, 0), 10_000);
    }

    #[test]
    fn publish_gas_cost_saturates() {
        let schedule = GasSchedule {
            publish_base: u64::MAX - 10,
            publish_per_byte: u64::MAX,
            ..GasSchedule::default()
        };
        // Should saturate to u64::MAX, not overflow.
        assert_eq!(publish_gas_cost(&schedule, 1), u64::MAX);
    }

    #[test]
    fn write_gas_cost_calculation() {
        let schedule = GasSchedule::default();
        assert_eq!(write_gas_cost(&schedule, 100), 500); // 100 × 5
        assert_eq!(write_gas_cost(&schedule, 0), 0);
    }

    #[test]
    fn read_gas_cost_calculation() {
        let schedule = GasSchedule::default();
        assert_eq!(read_gas_cost(&schedule, 100), 100); // 100 × 1
    }

    // ── GasExhausted display ────────────────────────────────────────

    #[test]
    fn gas_exhausted_display() {
        let err = GasExhausted {
            needed: 500,
            available: 100,
        };
        let msg = format!("{err}");
        assert!(msg.contains("500"));
        assert!(msg.contains("100"));
    }

    // ── GasMeter trait object safety ────────────────────────────────

    #[test]
    fn gas_meter_is_object_safe() {
        fn _accepts(_: &dyn GasMeter) {}
    }

    // ── estimate_query_gas tests ────────────────────────────────────

    #[test]
    fn estimate_query_gas_includes_call_base_and_io() {
        let schedule = GasSchedule::default();
        // No args, 100 bytes output: call_base + read_per_byte * 100
        let gas = estimate_query_gas(&schedule, &[], &[], 100);
        assert_eq!(gas, schedule.call_base + schedule.read_per_byte * 100);
    }

    #[test]
    fn estimate_query_gas_with_args() {
        let schedule = GasSchedule::default();
        let args = vec![vec![0u8; 50]]; // 50 bytes
        let gas = estimate_query_gas(&schedule, &[], &args, 200);
        assert_eq!(
            gas,
            schedule.call_base + schedule.read_per_byte * 50 + schedule.read_per_byte * 200
        );
    }

    #[test]
    fn estimate_query_gas_zero_io() {
        let schedule = GasSchedule::default();
        // No I/O: just call_base
        let gas = estimate_query_gas(&schedule, &[], &[], 0);
        assert_eq!(gas, schedule.call_base);
    }
}
