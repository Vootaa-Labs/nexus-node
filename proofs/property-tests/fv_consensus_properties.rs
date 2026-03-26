//! Property-based tests for formal verification invariants (consensus layer).
//!
//! These tests validate invariants registered in Solutions/21
//! (Formal Verification Object Register). Each test function
//! references its invariant ID in a doc comment.
//!
//! # Placement
//! This file lives in `proofs/property-tests/` for organizational clarity,
//! but the actual runnable tests are inside `nexus-consensus` as integration
//! tests or in-crate `#[cfg(test)]` modules. See `crates/nexus-consensus/tests/`.
//!
//! # Invariants Covered
//! - FV-CO-004: CommitSequence strict monotonicity
//! - FV-CO-006: Quorum threshold formula
//! - FV-CO-007: Certificate digest domain separation
//! - FV-CR-001: Domain separation tag uniqueness (consensus subset)
//!
//! # Test Suites
//!
//! | File | Type | Framework |
//! |------|------|-----------|
//! | `crates/nexus-consensus/tests/fv_property_tests.rs` | Deterministic | std #[test] |
//! | `crates/nexus-consensus/tests/fv_proptest.rs` | Randomised PBT | proptest |
//!
//! # Differential Corpus (consensus)
//!
//! | File | Scenarios | Invariant |
//! |------|-----------|----------|
//! | `VO-CO-001_dag_causality.json` | 2 | FV-CO-002 |
//! | `VO-CO-002_certificate_quorum.json` | 4 | FV-CO-006 |
//! | `VO-CO-003_anchor_election.json` | 3 | FV-CO-004 |
//! | `VO-CO-006_commit_sequence.json` | 3 | FV-CO-004 |
//! | `VO-CO-007_cert_domain_separation.json` | 3 | FV-CO-007 |
//! | `VO-CO-008_reputation_scoring.json` | 3 | FV-CO-003 |

