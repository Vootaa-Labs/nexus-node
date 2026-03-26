//! Move execution sessions.
//!
//! TLD-09 §6.2 defines three session types:
//! - **Publish** — may write modules + package metadata
//! - **Execute** — may mutate resource state
//! - **ReadOnly** — no persistent writes (view functions / simulation)
//!
//! All sessions share the same [`NexusStateView`] but enforce different
//! permission boundaries.

use std::collections::HashMap;

use crate::error::ExecutionResult;
use crate::types::ExecutionStatus;
use nexus_primitives::AccountAddress;

use super::events::EventAccumulator;
use super::gas_meter::{GasMeter, GasSchedule, SimpleGasMeter};
use super::resources::ResourceStore;
use super::state_view::NexusStateView;
use super::VmOutput;

// ── Gas summary (TLD-09 §7.1) ──────────────────────────────────────────

/// Itemised gas breakdown attached to every execution result.
#[derive(Debug, Clone, Default)]
pub(crate) struct MoveGasSummary {
    /// Gas consumed by computation (opcode execution).
    pub execution_gas: u64,
    /// Gas consumed by state reads.
    pub io_gas: u64,
    /// Gas consumed by state writes / storage allocation.
    pub storage_fee: u64,
}

impl MoveGasSummary {
    /// Total gas consumed.
    #[allow(dead_code)]
    pub fn total(&self) -> u64 {
        self.execution_gas
            .saturating_add(self.io_gas)
            .saturating_add(self.storage_fee)
    }
}

// ── Session kinds ───────────────────────────────────────────────────────

/// Permission level for a session — controls which operations are allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionKind {
    /// Module publish + metadata write.
    #[allow(dead_code)]
    Publish,
    /// Resource state mutation.
    Execute,
    /// No persistent writes.
    ReadOnly,
}

// ── Execute session ─────────────────────────────────────────────────────

/// An active execution session that tracks gas, resource writes, and events.
///
/// Created per-transaction by [`NexusMoveVm`](super::nexus_vm::NexusMoveVm)
/// and committed on success or rolled back on failure.
pub(crate) struct ExecuteSession<'a> {
    /// Session kind.
    pub kind: SessionKind,
    /// Transaction sender.
    pub sender: AccountAddress,
    /// Gas meter for this session.
    pub meter: SimpleGasMeter,
    /// Gas schedule for cost lookups.
    pub schedule: GasSchedule,
    /// Resource overlay (reads fall through to state view).
    pub resources: ResourceStore<'a>,
    /// Event accumulator.
    pub events: EventAccumulator,
    /// Itemised gas breakdown.
    pub gas_summary: MoveGasSummary,
}

impl<'a> ExecuteSession<'a> {
    /// Create a new execution session.
    pub fn new(
        kind: SessionKind,
        sender: AccountAddress,
        gas_limit: u64,
        schedule: GasSchedule,
        view: &'a NexusStateView<'a>,
    ) -> Self {
        Self {
            kind,
            sender,
            meter: SimpleGasMeter::new(gas_limit),
            schedule,
            resources: ResourceStore::new(view),
            events: EventAccumulator::new(),
            gas_summary: MoveGasSummary::default(),
        }
    }

    /// Charge execution gas (computation).
    pub fn charge_execution(&mut self, amount: u64) -> Result<(), ()> {
        self.gas_summary.execution_gas = self.gas_summary.execution_gas.saturating_add(amount);
        self.meter.charge(amount).map_err(|_| ())
    }

    /// Charge IO gas (state reads).
    pub fn charge_io(&mut self, amount: u64) -> Result<(), ()> {
        self.gas_summary.io_gas = self.gas_summary.io_gas.saturating_add(amount);
        self.meter.charge(amount).map_err(|_| ())
    }

    /// Charge storage gas (state writes).
    pub fn charge_storage(&mut self, amount: u64) -> Result<(), ()> {
        self.gas_summary.storage_fee = self.gas_summary.storage_fee.saturating_add(amount);
        self.meter.charge(amount).map_err(|_| ())
    }

    /// Read a resource, charging IO gas.
    pub fn read_resource(
        &mut self,
        account: &AccountAddress,
        type_tag: &str,
    ) -> ExecutionResult<Option<Vec<u8>>> {
        let val = self.resources.get(account, type_tag)?;
        let read_cost = val
            .as_ref()
            .map(|v| (v.len() as u64).saturating_mul(self.schedule.read_per_byte))
            .unwrap_or(0);
        // IO gas charge is best-effort; failure = OOG handled by caller.
        let _ = self.charge_io(read_cost);
        Ok(val)
    }

    /// Write a resource, charging storage gas.
    ///
    /// Returns `Err(WriteError::ReadOnly)` if the session forbids writes,
    /// or `Err(WriteError::OutOfGas)` if the gas budget is exceeded.
    pub fn write_resource(
        &mut self,
        account: AccountAddress,
        type_tag: &str,
        value: Vec<u8>,
    ) -> Result<(), WriteError> {
        if self.kind == SessionKind::ReadOnly {
            return Err(WriteError::ReadOnly);
        }
        let cost = (value.len() as u64).saturating_mul(self.schedule.write_per_byte);
        self.charge_storage(cost)
            .map_err(|_| WriteError::OutOfGas)?;
        self.resources.set(account, type_tag, value);
        Ok(())
    }

    /// Commit the session: consume and return a VmOutput.
    pub fn commit(self, status: ExecutionStatus) -> VmOutput {
        let (write_set, state_changes) = self.resources.into_changes();
        VmOutput {
            status,
            gas_used: self.meter.consumed(),
            state_changes,
            write_set,
        }
    }

    /// Abort the session: return a VmOutput with the given abort status,
    /// discarding any pending writes.
    pub fn abort(self, status: ExecutionStatus) -> VmOutput {
        VmOutput {
            status,
            gas_used: self.meter.consumed(),
            state_changes: vec![],
            write_set: HashMap::new(),
        }
    }
}

/// Error returned by [`ExecuteSession::write_resource`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WriteError {
    /// The session is read-only and cannot mutate state.
    ReadOnly,
    /// The gas budget has been exceeded.
    OutOfGas,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::StateView;

    struct EmptyState;
    impl StateView for EmptyState {
        fn get(&self, _account: &AccountAddress, _key: &[u8]) -> ExecutionResult<Option<Vec<u8>>> {
            Ok(None)
        }
    }

    fn addr(b: u8) -> AccountAddress {
        AccountAddress([b; 32])
    }

    #[test]
    fn session_charges_gas_correctly() {
        let state = EmptyState;
        let view = NexusStateView::new(&state);
        let schedule = GasSchedule::default();
        let mut session =
            ExecuteSession::new(SessionKind::Execute, addr(1), 100_000, schedule, &view);

        assert!(session.charge_execution(1_000).is_ok());
        assert!(session.charge_io(500).is_ok());
        assert!(session.charge_storage(200).is_ok());

        assert_eq!(session.gas_summary.execution_gas, 1_000);
        assert_eq!(session.gas_summary.io_gas, 500);
        assert_eq!(session.gas_summary.storage_fee, 200);
        assert_eq!(session.meter.consumed(), 1_700);
    }

    #[test]
    fn readonly_session_rejects_writes() {
        let state = EmptyState;
        let view = NexusStateView::new(&state);
        let schedule = GasSchedule::default();
        let mut session =
            ExecuteSession::new(SessionKind::ReadOnly, addr(1), 100_000, schedule, &view);

        let result = session.write_resource(addr(1), "counter::Counter", vec![0]);
        assert_eq!(result.unwrap_err(), WriteError::ReadOnly);
    }

    #[test]
    fn commit_produces_write_set() {
        let state = EmptyState;
        let view = NexusStateView::new(&state);
        let schedule = GasSchedule::default();
        let mut session =
            ExecuteSession::new(SessionKind::Execute, addr(1), 100_000, schedule, &view);

        session
            .write_resource(addr(0xBB), "counter::Counter", vec![42])
            .unwrap();
        let output = session.commit(ExecutionStatus::Success);

        assert_eq!(output.status, ExecutionStatus::Success);
        assert_eq!(output.state_changes.len(), 1);
        assert_eq!(output.write_set.len(), 1);
    }

    #[test]
    fn gas_summary_total() {
        let summary = MoveGasSummary {
            execution_gas: 100,
            io_gas: 50,
            storage_fee: 25,
        };
        assert_eq!(summary.total(), 175);
    }
}
