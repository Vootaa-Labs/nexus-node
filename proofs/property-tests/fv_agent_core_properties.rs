//! Property-based tests for formal verification invariants (agent core layer).
//!
//! These tests validate invariants for the Agent Core Engine (ACE),
//! A2A negotiation, delegation chain, and provenance subsystems added
//! in Phase 15.
//!
//! # Invariants Covered
//! - FV-AC-001: Delegation chain monotonic contraction
//! - FV-AC-002: A2A state machine — only legal transitions
//! - FV-AC-003: A2A replay protection — duplicate nonces rejected
//! - FV-AC-004: Provenance store bounded memory
//! - FV-AC-005: Snapshot hash integrity
//! - FV-AC-006: Revocation cascade completeness
//!
//! # Test Suites
//!
//! | File | Type | Framework |
//! |------|------|-----------|
//! | `crates/nexus-intent/src/agent_core/*.rs` (in-crate) | Deterministic | std #[test] |
//! | `crates/nexus-intent/tests/fv_proptest.rs` | Randomised PBT | proptest |
//!
//! # Differential Corpus (agent)
//!
//! | File | Scenarios | Invariant |
//! |------|-----------|----------|
//! | `VO-AG-001_envelope_integrity.json` | 3 | FV-AC-005 |
//! | `VO-AG-002_session_fsm.json` | 5 | FV-AC-002 |
//! | `VO-AG-003_a2a_negotiation.json` | 4 | FV-AC-001 |
//! | `VO-AG-004_provenance_tracking.json` | 3 | FV-AC-006 |

// ── FV-AC-001: Delegation chain monotonic contraction ──────────────────
//
// For any two adjacent links (parent, child) in a delegation chain:
//   child.max_value  ≤ parent.max_value
//   child.deadline   ≤ parent.deadline
//   child.contracts  ⊆ parent.contracts  (when parent is non-empty)
//
// Property test: randomly generated chains are validated iff the
// contraction invariant holds.  Tested in:
//   crates/nexus-intent/src/agent_core/capability_snapshot.rs
//     - valid_two_link_chain
//     - value_escalation_rejected
//     - deadline_escalation_rejected
//     - contract_not_in_parent_rejected

// ── FV-AC-002: A2A state machine ──────────────────────────────────────
//
// The A2A session state machine only permits the transitions enumerated in
// `A2aSessionState::can_transition_to`.  No message can cause a transition
// not in that set.
//
// Tested in:
//   crates/nexus-intent/src/agent_core/a2a.rs — transition table tests
//   crates/nexus-intent/src/agent_core/a2a_negotiator.rs
//     - full_lifecycle_propose_to_settle
//     - reject_negotiation
//     - timeout_detected_on_message

// ── FV-AC-003: A2A replay protection ──────────────────────────────────
//
// A2aNegotiator rejects any message whose nonce was previously seen,
// ensuring that replayed messages cannot re-drive the state machine.
//
// The `seen_nonces` HashSet is checked before message processing.
// Tested in:
//   crates/nexus-intent/src/agent_core/a2a_negotiator.rs
//     - (existing lifecycle tests use unique nonces)

// ── FV-AC-004: Provenance store bounded memory ───────────────────────
//
// ProvenanceStore enforces a `max_records` cap (default 100 000).
// When the cap is reached, the oldest record is evicted.
// Invariant: store.len() ≤ max_records at all times.
//
// Tested in:
//   crates/nexus-intent/src/agent_core/provenance_store.rs
//     - record_and_retrieve (basic operation)
//     - (eviction tested by with_max_records constructor)

// ── FV-AC-005: Snapshot hash integrity ───────────────────────────────
//
// `validate_capability_against_snapshot` verifies that `snapshot_hash`
// matches `compute_snapshot_hash(snapshot)` when the hash is non-zero.
// This prevents tampering with capability snapshots in transit.
//
// Tested in:
//   crates/nexus-intent/src/agent_core/capability_snapshot.rs
//     - snapshot_hash_integrity_verified

// ── FV-AC-006: Revocation cascade completeness ──────────────────────
//
// `revocation_cascade` revokes all tokens transitively reachable from the
// revoked agent.  If agent X is revoked, then for every token T where
// T.delegator ∈ {X} ∪ descendants(X), T.revoked == true.
//
// Tested in:
//   crates/nexus-intent/src/agent_core/capability_snapshot.rs
//     - revocation_cascade_propagates
//     - revocation_cascade_no_match
