// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! `nexus-consensus` — Narwhal DAG + Shoal++ BFT consensus engine for Nexus.
//!
//! Implements the two-phase consensus protocol:
//! 1. **Narwhal** — DAG-structured mempool with reliable broadcast (160k+ TPS target)
//! 2. **Shoal++** — BFT total-order finalization with <1 s latency target
//!
//! # Modules
//! - [`error`]     — `ConsensusError` unified error type
//! - [`types`]     — `NarwhalBatch`, `NarwhalCertificate`, `ShoalVote`, `ShoalAnchor`,
//!                   `CommittedBatch`, `ValidatorBitset`, `ValidatorInfo`, `ReputationScore`
//! - [`traits`]    — `BatchProposer`, `CertificateDag`, `BftOrderer`, `ValidatorRegistry`
//! - [`validator`] — `Committee` — PoS committee management (implements `ValidatorRegistry`)
//! - [`dag`]       — `InMemoryDag` — Narwhal DAG data structure (implements `CertificateDag`)
//! - [`certificate`] — `CertificateBuilder`, `CertificateVerifier` — construction & verification
//! - [`shoal`]       — `ShoalOrderer` — Shoal++ BFT total-order finalization (implements `BftOrderer`)
//! - [`engine`]      — `ConsensusEngine` — async actor tying the full pipeline together

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod certificate;
pub mod dag;
pub mod engine;
pub mod epoch_manager;
pub mod error;
pub mod persist;
pub mod shoal;
pub mod traits;
pub mod types;
pub mod validator;

// ── Convenience re-exports ───────────────────────────────────────────────────

pub use certificate::{compute_cert_digest, CertificateBuilder, CertificateVerifier};
pub use dag::InMemoryDag;
pub use engine::ConsensusEngine;
pub use epoch_manager::EpochManager;
pub use error::{ConsensusError, ConsensusResult};
pub use persist::{DagPersistSync, DagPersistence, PersistError};
pub use shoal::ShoalOrderer;
pub use traits::{BatchProposer, BftOrderer, CertificateDag, ValidatorRegistry};
pub use types::{
    BatchStatus, CommittedBatch, EpochConfig, EpochTransition, EpochTransitionTrigger,
    NarwhalBatch, NarwhalCertificate, PersistentCommittee, ReputationScore, ShoalAnchor, ShoalVote,
    ValidatorBitset, ValidatorInfo,
};
pub use validator::Committee;
