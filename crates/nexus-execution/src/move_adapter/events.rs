//! Move event → Nexus event bridge.
//!
//! TLD-09 §7.2: Move events must be normalized to Nexus DTOs before
//! reaching the API layer. This module defines the canonical
//! [`ContractEvent`] and conversion helpers.

use nexus_primitives::AccountAddress;
use serde::{Deserialize, Serialize};

// ── Nexus event DTO ─────────────────────────────────────────────────────

/// A normalised contract event emitted during Move execution.
///
/// This is the canonical DTO that flows into receipts, RPC, WebSocket,
/// and provenance — no raw VM event format leaks beyond this boundary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ContractEvent {
    /// The contract that emitted the event.
    pub emitter: AccountAddress,
    /// Event type tag (e.g. `"counter::IncrementEvent"`).
    pub type_tag: String,
    /// Sequence number within the emitting account (monotonic).
    pub sequence_number: u64,
    /// BCS-encoded event payload.
    pub data: Vec<u8>,
}

/// Accumulator for events produced during a single transaction execution.
#[derive(Debug, Default)]
pub(crate) struct EventAccumulator {
    events: Vec<ContractEvent>,
    /// Per-account sequence counters.
    seq: std::collections::HashMap<AccountAddress, u64>,
}

impl EventAccumulator {
    /// Create an empty accumulator.
    pub fn new() -> Self {
        Self::default()
    }

    /// Emit an event and assign a sequence number.
    pub fn emit(
        &mut self,
        emitter: AccountAddress,
        type_tag: String,
        data: Vec<u8>,
    ) -> &ContractEvent {
        let seq = self.seq.entry(emitter).or_insert(0);
        let event = ContractEvent {
            emitter,
            type_tag,
            sequence_number: *seq,
            data,
        };
        *seq = seq.saturating_add(1);
        self.events.push(event);
        self.events.last().expect("just pushed")
    }

    /// Drain all accumulated events.
    #[allow(dead_code)]
    pub fn drain(self) -> Vec<ContractEvent> {
        self.events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(b: u8) -> AccountAddress {
        AccountAddress([b; 32])
    }

    #[test]
    fn emit_assigns_sequence_numbers() {
        let mut acc = EventAccumulator::new();
        acc.emit(addr(1), "counter::Increment".into(), vec![1]);
        acc.emit(addr(1), "counter::Increment".into(), vec![2]);
        acc.emit(addr(2), "token::Transfer".into(), vec![3]);

        let events = acc.drain();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].sequence_number, 0);
        assert_eq!(events[1].sequence_number, 1);
        // Different emitter starts at 0.
        assert_eq!(events[2].sequence_number, 0);
    }

    #[test]
    fn event_round_trip_bcs() {
        let event = ContractEvent {
            emitter: addr(0xAA),
            type_tag: "counter::Increment".into(),
            sequence_number: 42,
            data: vec![1, 2, 3],
        };
        let encoded = bcs::to_bytes(&event).unwrap();
        let decoded: ContractEvent = bcs::from_bytes(&encoded).unwrap();
        assert_eq!(event, decoded);
    }
}
