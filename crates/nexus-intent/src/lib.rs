//! `nexus-intent` — User-intent routing layer for Nexus.
//!
//! Accepts high-level user intents (semantic transaction specifications)
//! and compiles them into concrete multi-shard transaction sequences.
//! Users never need to specify shard IDs — the intent layer resolves
//! routing transparently via [`AccountResolver`] and [`IntentCompiler`].
//!
//! # Architecture
//!
//! ```text
//! User / RPC (Layer 5)
//!       │  [SignedUserIntent]
//!       ▼
//! ┌──────────────────────────┐
//! │     Intent Layer         │
//! │  IntentCompiler          │
//! │  + AccountResolver       │
//! │  + ContractRegistry      │
//! └──────────────────────────┘
//!       │  [CompiledIntentPlan → SignedTransaction sequence]
//!       ▼
//! Consensus (Layer 3) → Execution (Layer 2)
//! ```
//!
//! # Modules
//!
//! - [`config`]  — [`IntentConfig`] for timeouts, limits, and cache policies
//! - [`error`]   — [`IntentError`] unified error type
//! - [`traits`]  — [`IntentCompiler`] + [`AccountResolver`] trait contracts
//! - [`types`]   — [`UserIntent`], [`SignedUserIntent`], [`CompiledIntentPlan`], etc.
//! - [`resolver`] — [`AccountResolverImpl`] and supporting caches
//! - [`compiler`] — [`IntentCompilerImpl`], parser, and validator

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod agent_core;
pub mod compiler;
pub mod config;
pub mod error;
pub mod metrics;
pub mod resolver;
pub mod service;
pub mod traits;
pub mod types;

// Re-exports for ergonomic use.
pub use compiler::IntentCompilerImpl;
pub use config::IntentConfig;
pub use error::{IntentError, IntentResult};
pub use metrics::IntentMetrics;
pub use resolver::AccountResolverImpl;
pub use service::{IntentHandle, IntentService};
pub use traits::{AccountResolver, IntentCompiler};
pub use types::{
    AgentConstraints, AgentIntentSpec, AgentTask, CompiledIntentPlan, ContractLocation,
    GasEstimate, HumanApproval, IntentStatus, IntentStep, SignedUserIntent, UserIntent,
};

// ── Agent Core Engine re-exports ────────────────────────────────────────
pub use agent_core::a2a::{A2aNegotiation, A2aSessionState};
pub use agent_core::capability_snapshot::{AgentCapabilitySnapshot, CapabilityScope};
pub use agent_core::envelope::{AgentEnvelope, AgentPrincipal, ProtocolKind};
pub use agent_core::planner::{ExecutionReceipt, SimulationResult};
pub use agent_core::policy::PolicyDecision;
pub use agent_core::provenance::{
    compute_anchor_digest, verify_anchor, AnchorBatch, AnchorReceipt, ProvenanceRecord,
    ProvenanceStatus,
};
pub use agent_core::provenance_store::ProvenanceStore;
pub use agent_core::rocks_provenance_store::RocksProvenanceStore;
pub use agent_core::rocks_session_store::RocksSessionStore;
pub use agent_core::session::{AgentSession, SessionState};
