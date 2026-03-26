//! Agent Core Engine (ACE) — protocol-neutral agent processing core.
//!
//! This module serves as the **single internal truth source** for all
//! AI-agent interactions with Nexus. External protocol adapters (MCP,
//! A2A, REST) must route through ACE — they may not define independent
//! permission models, session semantics, or plan binding logic.
//!
//! # Architecture
//!
//! ```text
//! External Adapters (MCP / A2A / REST)
//!                 │
//!                 ▼
//!     ┌── Agent Core Engine ──┐
//!     │  envelope.rs          │  canonical request envelope
//!     │  session.rs           │  session lifecycle & replay protection
//!     │  capability_snapshot  │  delegation chain & capability validation
//!     │  planner.rs           │  simulate → plan → confirm → execute
//!     │  dispatcher.rs        │  route to Intent / Query / Execution
//!     │  policy.rs            │  human confirmation & value/contract gates
//!     │  a2a.rs               │  A2A canonical envelope & state machine
//!     │  provenance.rs        │  provenance recording & audit queries
//!     └───────────────────────┘
//!                 │
//!                 ▼
//!     IntentCompiler / ExecutionEngine / Storage
//! ```
//!
//! # Constraints
//!
//! 1. External adapters **MUST NOT** call execution backends directly.
//! 2. External adapters **MUST NOT** define independent permission models.
//! 3. `simulation_result`, `confirmation_ref`, `execute_plan` share one `plan_hash`.
//! 4. All executable requests must first produce an [`AgentEnvelope`] and [`AgentSession`].
//!
//! # Phase 10 Status
//!
//! This module establishes the canonical schema and adapter boundary
//! (T-10014). Operational planner/dispatcher implementation follows
//! in later tasks (T-10015–T-10017).

pub mod a2a;
pub mod a2a_negotiator;
pub mod capability_snapshot;
pub mod dispatcher;
pub mod engine;
pub mod envelope;
pub mod intent_planner_bridge;
pub mod planner;
pub mod policy;
pub mod provenance;
pub mod provenance_store;
pub mod rocks_provenance_store;
pub mod rocks_session_store;
pub mod session;

use crate::types::UserIntent;
use nexus_primitives::Amount;

/// Extract the value (amount) of an action.
///
/// Used by capability checks and orchestration to evaluate agent constraints.
pub(crate) fn action_value(intent: &UserIntent) -> Amount {
    match intent {
        UserIntent::Transfer { amount, .. } => *amount,
        UserIntent::Swap { amount, .. } => *amount,
        UserIntent::Stake { amount, .. } => *amount,
        UserIntent::ContractCall { .. } => Amount::ZERO,
        UserIntent::AgentTask { .. } => Amount::ZERO,
    }
}
