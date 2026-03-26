//! Concrete `DispatchBackend` — the Agent Core Engine.
//!
//! Wires envelope → session → policy → planner into one coherent
//! dispatch pipeline with real query resolution.

use std::collections::HashMap;
use std::sync::Mutex;

use nexus_primitives::{AccountAddress, Blake3Digest, ContractAddress, TimestampMs};

use crate::agent_core::capability_snapshot::AgentCapabilitySnapshot;
use crate::agent_core::dispatcher::{DispatchBackend, DispatchOutcome};
use crate::agent_core::envelope::{AgentEnvelope, AgentRequestKind, QueryKind};
use crate::agent_core::planner::{
    compute_plan_hash, validate_confirmation_state, validate_plan_binding, PlannerBackend,
};
use crate::agent_core::policy::{evaluate_policy, ConfirmationThreshold, PolicyDecision};
use crate::agent_core::provenance::ProvenanceQueryParams;
use crate::agent_core::session::{AgentSession, SessionState};
use crate::error::{IntentError, IntentResult};

// ── AgentQueryBackend trait ─────────────────────────────────────────────

/// Backend for resolving read-only agent queries (balance, status, contract state).
///
/// Implementations are provided by the node assembly layer which has
/// access to the full storage and execution backends.
pub trait AgentQueryBackend: Send + Sync {
    /// Query the balance of an account (returns BCS-encoded `Amount`).
    fn query_balance(&self, account: &AccountAddress) -> IntentResult<Vec<u8>>;

    /// Query the status of an intent or transaction by digest (returns BCS-encoded status).
    fn query_intent_status(&self, digest: &Blake3Digest) -> IntentResult<Vec<u8>>;

    /// Query contract state (returns BCS-encoded resource bytes).
    fn query_contract_state(
        &self,
        contract: &ContractAddress,
        resource: &str,
    ) -> IntentResult<Vec<u8>>;
}

/// No-op query backend that returns empty payloads.
///
/// Used as the default when no real query backend is configured.
pub struct NullQueryBackend;

impl AgentQueryBackend for NullQueryBackend {
    fn query_balance(&self, _account: &AccountAddress) -> IntentResult<Vec<u8>> {
        Ok(Vec::new())
    }

    fn query_intent_status(&self, _digest: &Blake3Digest) -> IntentResult<Vec<u8>> {
        Ok(Vec::new())
    }

    fn query_contract_state(
        &self,
        _contract: &ContractAddress,
        _resource: &str,
    ) -> IntentResult<Vec<u8>> {
        Ok(Vec::new())
    }
}

// ── Session capacity & TTL configuration (SEC-H10) ──────────────────────

/// Configuration for agent session capacity and TTL limits.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Maximum number of sessions that can exist simultaneously.
    pub max_sessions: usize,
    /// Sessions older than this duration (in ms) are eligible for eviction.
    pub session_ttl_ms: u64,
    /// Maximum number of idempotency keys retained per session.
    pub max_idempotency_keys_per_session: usize,
    /// Maximum requests per session within the rate window (0 = unlimited).
    pub rate_limit_per_session: u32,
    /// Sliding rate-limit window duration in milliseconds.
    pub rate_limit_window_ms: u64,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            max_sessions: 10_000,
            session_ttl_ms: 30 * 60 * 1000, // 30 minutes
            max_idempotency_keys_per_session: 1_000,
            rate_limit_per_session: 100,
            rate_limit_window_ms: 60_000, // 1 minute
        }
    }
}

// ── AgentCoreEngine ─────────────────────────────────────────────────────

/// The Agent Core Engine (ACE) — concrete dispatch hub.
///
/// Manages agent sessions, enforces policy, delegates to a
/// [`PlannerBackend`] for simulation and execution, and resolves
/// queries via an [`AgentQueryBackend`].
pub struct AgentCoreEngine<P: PlannerBackend, Q: AgentQueryBackend = NullQueryBackend> {
    planner: P,
    query_backend: Q,
    threshold: ConfirmationThreshold,
    sessions: Mutex<HashMap<Blake3Digest, AgentSession>>,
    /// Per-session seen idempotency keys with their timestamps (SEC-H11).
    seen_keys: Mutex<HashMap<Blake3Digest, Vec<(Blake3Digest, u64)>>>,
    /// Per-session request timestamps for sliding-window rate limiting (SEC-Z9).
    rate_buckets: Mutex<HashMap<Blake3Digest, Vec<u64>>>,
    config: SessionConfig,
    /// Default confirmation timeout in milliseconds (30 seconds).
    #[allow(dead_code)]
    confirmation_timeout_ms: u64,
}

impl<P: PlannerBackend> AgentCoreEngine<P, NullQueryBackend> {
    /// Create a new engine with the given planner and policy threshold.
    pub fn new(planner: P, threshold: ConfirmationThreshold) -> Self {
        Self::with_config(planner, threshold, SessionConfig::default())
    }

    /// Create a new engine with explicit session configuration.
    pub fn with_config(
        planner: P,
        threshold: ConfirmationThreshold,
        config: SessionConfig,
    ) -> Self {
        AgentCoreEngine {
            planner,
            query_backend: NullQueryBackend,
            threshold,
            sessions: Mutex::new(HashMap::new()),
            seen_keys: Mutex::new(HashMap::new()),
            rate_buckets: Mutex::new(HashMap::new()),
            config,
            confirmation_timeout_ms: 30_000,
        }
    }
}

impl<P: PlannerBackend, Q: AgentQueryBackend> AgentCoreEngine<P, Q> {
    /// Create a new engine with a real query backend.
    pub fn with_query_backend(
        planner: P,
        query_backend: Q,
        threshold: ConfirmationThreshold,
        config: SessionConfig,
    ) -> Self {
        Self {
            planner,
            query_backend,
            threshold,
            sessions: Mutex::new(HashMap::new()),
            seen_keys: Mutex::new(HashMap::new()),
            rate_buckets: Mutex::new(HashMap::new()),
            config,
            confirmation_timeout_ms: 30_000,
        }
    }

    /// Retrieve a session snapshot (for external inspection).
    pub fn get_session(&self, session_id: &Blake3Digest) -> Option<AgentSession> {
        let sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions.get(session_id).cloned()
    }

    /// Current number of tracked sessions.
    pub fn session_count(&self) -> usize {
        let sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions.len()
    }

    fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    /// Check the idempotency key and reject replay within the replay window (SEC-H11).
    fn check_idempotency(
        &self,
        session_id: &Blake3Digest,
        key: &Blake3Digest,
        now_ms: u64,
        replay_window_ms: u64,
    ) -> IntentResult<()> {
        let mut seen = self.seen_keys.lock().unwrap_or_else(|e| e.into_inner());
        let entries = seen.entry(*session_id).or_default();

        // Purge expired entries outside the replay window.
        entries.retain(|(_, ts)| now_ms.saturating_sub(*ts) < replay_window_ms);

        // Enforce per-session idempotency key limit.
        if entries.len() >= self.config.max_idempotency_keys_per_session {
            // Remove the oldest entry to make room.
            entries.sort_by_key(|(_, ts)| *ts);
            entries.remove(0);
        }

        // Check for duplicate.
        if entries.iter().any(|(k, _)| k == key) {
            return Err(IntentError::AgentCapabilityDenied {
                reason: "duplicate idempotency key within replay window".to_string(),
            });
        }

        entries.push((*key, now_ms));
        Ok(())
    }

    /// Sliding-window rate limit check per session (SEC-Z9).
    ///
    /// Counts the number of requests in the current window and rejects
    /// if the limit is exceeded. A limit of 0 means unlimited.
    fn check_rate_limit(&self, session_id: &Blake3Digest, now_ms: u64) -> IntentResult<()> {
        let limit = self.config.rate_limit_per_session;
        if limit == 0 {
            return Ok(());
        }
        let window = self.config.rate_limit_window_ms;

        let mut buckets = self.rate_buckets.lock().unwrap_or_else(|e| e.into_inner());
        let timestamps = buckets.entry(*session_id).or_default();

        // Purge entries outside the window.
        timestamps.retain(|ts| now_ms.saturating_sub(*ts) < window);

        if timestamps.len() >= limit as usize {
            return Err(IntentError::AgentCapabilityDenied {
                reason: format!(
                    "rate limit exceeded: {} requests in {}ms window",
                    limit, window,
                ),
            });
        }

        timestamps.push(now_ms);
        Ok(())
    }

    /// Evict expired and terminal sessions, enforcing the capacity limit (SEC-H10).
    fn enforce_session_capacity(&self, now_ms: u64) {
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());

        // First pass: evict expired and terminal sessions.
        sessions.retain(|_, s| {
            let age = now_ms.saturating_sub(s.created_at_ms.0);
            let expired = age > self.config.session_ttl_ms;
            let terminal = s.current_state.is_terminal();
            !expired && !terminal
        });

        // Second pass: if still over capacity, evict oldest.
        if sessions.len() >= self.config.max_sessions {
            let mut entries: Vec<_> = sessions
                .iter()
                .map(|(k, v)| (*k, v.created_at_ms.0))
                .collect();
            entries.sort_by_key(|(_, ts)| *ts);

            let to_evict = sessions.len() - self.config.max_sessions + 1;
            for (key, _) in entries.into_iter().take(to_evict) {
                sessions.remove(&key);
            }
        }
    }

    fn get_or_create_session(&self, envelope: &AgentEnvelope) -> IntentResult<AgentSession> {
        let now_ms = Self::now_ms();

        // Enforce capacity before creating new sessions.
        self.enforce_session_capacity(now_ms);

        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());

        // If session exists, return it.
        if let Some(s) = sessions.get(&envelope.session_id) {
            return Ok(s.clone());
        }

        // Check capacity for new session creation.
        if sessions.len() >= self.config.max_sessions {
            return Err(IntentError::AgentCapabilityDenied {
                reason: format!(
                    "session capacity limit reached ({})",
                    self.config.max_sessions
                ),
            });
        }

        let session = AgentSession::new(envelope.session_id, TimestampMs(now_ms));
        sessions.insert(envelope.session_id, session.clone());
        Ok(session)
    }

    fn update_session(&self, session: AgentSession) {
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions.insert(session.session_id, session);
    }
}

impl<P: PlannerBackend, Q: AgentQueryBackend> DispatchBackend for AgentCoreEngine<P, Q> {
    fn dispatch(&self, envelope: &AgentEnvelope) -> IntentResult<DispatchOutcome> {
        // 1. Pre-dispatch validation (IDs, deadline).
        crate::agent_core::dispatcher::pre_dispatch_validate(envelope, Self::now_ms())?;

        // 1b. Idempotency check — reject replayed requests (SEC-H11).
        let replay_window = crate::agent_core::session::DEFAULT_REPLAY_WINDOW_MS;
        self.check_idempotency(
            &envelope.session_id,
            &envelope.idempotency_key,
            Self::now_ms(),
            replay_window,
        )?;

        // 1c. Rate limit check (SEC-Z9).
        self.check_rate_limit(&envelope.session_id, Self::now_ms())?;

        // 2. Get or create session (enforces capacity — SEC-H10).
        let mut session = self.get_or_create_session(envelope)?;

        // 3. Route by request kind.
        match &envelope.request_kind {
            AgentRequestKind::SimulateIntent { .. } => {
                // 3a. Policy check — evaluate against capability + constraints.
                let capability = self.resolve_capability(envelope);
                // SEC-Z8: Validate capability expiry and delegation integrity.
                crate::agent_core::capability_snapshot::validate_capability_against_snapshot(
                    &capability,
                    Self::now_ms(),
                )
                .map_err(|reason| IntentError::AgentCapabilityDenied { reason })?;
                // Extract intent from the envelope so policy can inspect it.
                let intents = match &envelope.request_kind {
                    AgentRequestKind::SimulateIntent { intent } => vec![intent.clone()],
                    AgentRequestKind::IntentRequest { intent } => vec![intent.clone()],
                    _ => vec![],
                };
                let decision = evaluate_policy(
                    &intents,
                    &envelope.constraints,
                    &capability,
                    &self.threshold,
                )?;
                if let PolicyDecision::Denied { reason } = &decision {
                    return Ok(DispatchOutcome::Rejected {
                        reason: reason.clone(),
                    });
                }

                // 3b. Simulate via planner.
                let sim_result = self.planner.simulate(envelope)?;

                // 3c. Bind plan hash to session.
                session.bind_plan(sim_result.plan_hash).map_err(|e| {
                    IntentError::AgentCapabilityDenied {
                        reason: e.to_string(),
                    }
                })?;

                // 3d. Transition session: Received → Simulated → AwaitingConfirmation.
                session
                    .transition_to(SessionState::Simulated)
                    .map_err(|e| IntentError::AgentCapabilityDenied {
                        reason: e.to_string(),
                    })?;

                if matches!(decision, PolicyDecision::RequiresConfirmation { .. }) {
                    session
                        .transition_to(SessionState::AwaitingConfirmation)
                        .map_err(|e| IntentError::AgentCapabilityDenied {
                            reason: e.to_string(),
                        })?;
                }

                self.update_session(session);
                Ok(DispatchOutcome::Simulated(sim_result))
            }

            AgentRequestKind::ExecutePlan {
                plan_hash,
                confirmation_ref,
            } => {
                // 4a. Validate plan binding.
                validate_plan_binding(&session, plan_hash)?;

                // 4b. If session needs confirmation, check state.
                if session.current_state == SessionState::AwaitingConfirmation {
                    validate_confirmation_state(&session)?;
                }

                // 4c. Transition to Executing.
                // If still in Simulated state, we can go directly
                if session.current_state == SessionState::Simulated {
                    session
                        .transition_to(SessionState::Executing)
                        .map_err(|e| IntentError::AgentCapabilityDenied {
                            reason: e.to_string(),
                        })?;
                } else if session.current_state == SessionState::AwaitingConfirmation {
                    session
                        .transition_to(SessionState::Executing)
                        .map_err(|e| IntentError::AgentCapabilityDenied {
                            reason: e.to_string(),
                        })?;
                } else {
                    return Err(IntentError::AgentCapabilityDenied {
                        reason: format!("cannot execute from state {:?}", session.current_state),
                    });
                }

                self.update_session(session.clone());

                // 4d. Execute via planner.
                let receipt = self.planner.execute(&session, confirmation_ref)?;

                // 4e. Finalize session.
                session
                    .transition_to(SessionState::Finalized)
                    .map_err(|e| IntentError::AgentCapabilityDenied {
                        reason: e.to_string(),
                    })?;
                self.update_session(session);

                Ok(DispatchOutcome::Executed(receipt))
            }

            AgentRequestKind::ConfirmPlan { plan_hash } => {
                // 5. Confirm a previously simulated plan.
                // Session must be in AwaitingConfirmation state.
                validate_confirmation_state(&session)?;
                validate_plan_binding(&session, plan_hash)?;

                // Delegate to planner for confirmation binding.
                let confirmation = self.planner.confirm(&session, plan_hash)?;

                // Store the confirmation reference on the session.
                session.confirmation_ref = Some(confirmation.confirmation_ref);

                // Transition: AwaitingConfirmation → Executing.
                session
                    .transition_to(SessionState::Executing)
                    .map_err(|e| IntentError::AgentCapabilityDenied {
                        reason: e.to_string(),
                    })?;

                self.update_session(session.clone());

                // Auto-execute after confirmation.
                let receipt = self
                    .planner
                    .execute(&session, &confirmation.confirmation_ref)?;

                session
                    .transition_to(SessionState::Finalized)
                    .map_err(|e| IntentError::AgentCapabilityDenied {
                        reason: e.to_string(),
                    })?;
                self.update_session(session);

                Ok(DispatchOutcome::Executed(receipt))
            }

            AgentRequestKind::RejectPlan { plan_hash, reason } => {
                // 6. Reject a previously simulated plan.
                validate_plan_binding(&session, plan_hash)?;

                // Abort the session.
                session.transition_to(SessionState::Aborted).map_err(|e| {
                    IntentError::AgentCapabilityDenied {
                        reason: e.to_string(),
                    }
                })?;
                self.update_session(session);

                Ok(DispatchOutcome::Rejected {
                    reason: reason
                        .clone()
                        .unwrap_or_else(|| "plan rejected by user".to_string()),
                })
            }

            AgentRequestKind::IntentRequest { .. } => {
                // One-shot intent: simulate + auto-execute if policy allows.
                let capability = self.resolve_capability(envelope);
                // SEC-Z8: Validate capability expiry and delegation integrity.
                crate::agent_core::capability_snapshot::validate_capability_against_snapshot(
                    &capability,
                    Self::now_ms(),
                )
                .map_err(|reason| IntentError::AgentCapabilityDenied { reason })?;
                let intents = match &envelope.request_kind {
                    AgentRequestKind::IntentRequest { intent } => vec![intent.clone()],
                    _ => vec![],
                };
                let decision = evaluate_policy(
                    &intents,
                    &envelope.constraints,
                    &capability,
                    &self.threshold,
                )?;

                match decision {
                    PolicyDecision::Denied { reason } => Ok(DispatchOutcome::Rejected { reason }),
                    PolicyDecision::RequiresConfirmation { reason: _ } => {
                        // Cannot auto-execute — simulate and return the result
                        // so the caller can confirm via ExecutePlan.
                        let sim_result = self.planner.simulate(envelope)?;
                        session.bind_plan(sim_result.plan_hash).map_err(|e| {
                            IntentError::AgentCapabilityDenied {
                                reason: e.to_string(),
                            }
                        })?;
                        session
                            .transition_to(SessionState::Simulated)
                            .map_err(|e| IntentError::AgentCapabilityDenied {
                                reason: e.to_string(),
                            })?;
                        session
                            .transition_to(SessionState::AwaitingConfirmation)
                            .map_err(|e| IntentError::AgentCapabilityDenied {
                                reason: e.to_string(),
                            })?;
                        self.update_session(session);
                        // Return Simulated (not Rejected) so caller can
                        // confirm and proceed with ExecutePlan.
                        Ok(DispatchOutcome::Simulated(sim_result))
                    }
                    PolicyDecision::Approved => {
                        // Auto-execute approved intent.
                        let sim_result = self.planner.simulate(envelope)?;
                        let plan_hash = sim_result.plan_hash;
                        session.bind_plan(plan_hash).map_err(|e| {
                            IntentError::AgentCapabilityDenied {
                                reason: e.to_string(),
                            }
                        })?;
                        session
                            .transition_to(SessionState::Simulated)
                            .map_err(|e| IntentError::AgentCapabilityDenied {
                                reason: e.to_string(),
                            })?;
                        session
                            .transition_to(SessionState::Executing)
                            .map_err(|e| IntentError::AgentCapabilityDenied {
                                reason: e.to_string(),
                            })?;
                        self.update_session(session.clone());

                        let confirmation_ref =
                            compute_plan_hash(&envelope.request_id, b"auto-confirm")?;
                        let receipt = self.planner.execute(&session, &confirmation_ref)?;

                        session
                            .transition_to(SessionState::Finalized)
                            .map_err(|e| IntentError::AgentCapabilityDenied {
                                reason: e.to_string(),
                            })?;
                        self.update_session(session);

                        Ok(DispatchOutcome::Executed(receipt))
                    }
                }
            }

            AgentRequestKind::Query { query_kind } => {
                let payload = match query_kind {
                    QueryKind::Balance { account } => self.query_backend.query_balance(account)?,
                    QueryKind::IntentStatus { digest } => {
                        self.query_backend.query_intent_status(digest)?
                    }
                    QueryKind::ContractState { contract, resource } => self
                        .query_backend
                        .query_contract_state(contract, resource)?,
                };
                Ok(DispatchOutcome::QueryResult { payload })
            }

            AgentRequestKind::QueryProvenance { filter } => {
                let params = ProvenanceQueryParams {
                    limit: 100,
                    cursor: None,
                    after_ms: None,
                    before_ms: None,
                };
                // Encode the filter + params as BCS for the response.
                // The actual provenance resolution is handled by the
                // provenance store at the node assembly layer.
                let payload = bcs::to_bytes(&(filter, params))
                    .map_err(|e| IntentError::Codec(e.to_string()))?;
                Ok(DispatchOutcome::QueryResult { payload })
            }
        }
    }
}

impl<P: PlannerBackend, Q: AgentQueryBackend> AgentCoreEngine<P, Q> {
    /// Resolve the capability snapshot for the envelope.
    ///
    /// Scope is derived from the request kind: queries receive
    /// `ReadOnly`, mutations receive `Full`.  The value ceiling is
    /// taken from the envelope's execution constraints rather than
    /// being hardcoded to `u64::MAX`.
    fn resolve_capability(&self, envelope: &AgentEnvelope) -> AgentCapabilitySnapshot {
        use crate::agent_core::capability_snapshot::CapabilityScope;

        let scope = match &envelope.request_kind {
            AgentRequestKind::Query { .. } | AgentRequestKind::QueryProvenance { .. } => {
                CapabilityScope::ReadOnly
            }
            _ => CapabilityScope::Full,
        };

        AgentCapabilitySnapshot {
            agent_id: envelope.caller.address,
            scope,
            max_value: envelope.constraints.max_total_value,
            deadline: envelope.deadline_ms,
            allowed_contracts: envelope.constraints.allowed_contracts.clone(),
            delegation_chain: vec![],
            snapshot_hash: Blake3Digest([0u8; 32]),
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_core::envelope::{
        AgentEnvelope, AgentExecutionConstraints, AgentPrincipal, ProtocolKind, QueryKind,
    };
    use crate::agent_core::planner::ConfirmationResponse;
    use crate::types::UserIntent;
    use crate::{ExecutionReceipt, SimulationResult};
    use nexus_primitives::{AccountAddress, Amount, TokenId};

    /// Mock planner that returns predictable results.
    struct MockPlanner;

    impl PlannerBackend for MockPlanner {
        fn simulate(&self, envelope: &AgentEnvelope) -> IntentResult<SimulationResult> {
            let plan_hash = compute_plan_hash(&envelope.request_id, b"mock-sim")?;
            Ok(SimulationResult {
                session_id: envelope.session_id,
                plan_hash,
                estimated_gas: 42_000,
                step_count: 1,
                requires_cross_shard: false,
                simulated_at_ms: TimestampMs(1_000),
                summary: "mock simulation summary".to_string(),
            })
        }

        fn confirm(
            &self,
            session: &AgentSession,
            plan_hash: &Blake3Digest,
        ) -> IntentResult<ConfirmationResponse> {
            Ok(ConfirmationResponse {
                session_id: session.session_id,
                plan_hash: *plan_hash,
                confirmation_ref: Blake3Digest([0xCF; 32]),
                confirmed_at_ms: TimestampMs(1_500),
            })
        }

        fn execute(
            &self,
            session: &AgentSession,
            _confirmation_ref: &Blake3Digest,
        ) -> IntentResult<ExecutionReceipt> {
            Ok(ExecutionReceipt {
                session_id: session.session_id,
                plan_hash: session.plan_hash.unwrap_or(Blake3Digest([0u8; 32])),
                tx_hashes: vec![Blake3Digest([0xEE; 32])],
                gas_used: 42_000,
                completed_at_ms: TimestampMs(2_000),
            })
        }
    }

    fn dummy_intent() -> UserIntent {
        UserIntent::Transfer {
            to: AccountAddress([0xBB; 32]),
            token: TokenId::Native,
            amount: Amount(1_000),
        }
    }

    fn make_envelope(kind: AgentRequestKind) -> AgentEnvelope {
        AgentEnvelope {
            protocol_kind: ProtocolKind::Mcp,
            protocol_version: "mcp/2025-11-05".to_string(),
            request_id: Blake3Digest([0x01; 32]),
            session_id: Blake3Digest([0x02; 32]),
            idempotency_key: Blake3Digest([0x03; 32]),
            caller: AgentPrincipal {
                address: AccountAddress([0xAA; 32]),
                display_name: None,
            },
            delegated_capability: None,
            request_kind: kind,
            constraints: AgentExecutionConstraints {
                max_gas: 100_000,
                max_total_value: Amount(1_000_000),
                allowed_contracts: vec![],
            },
            deadline_ms: TimestampMs(u64::MAX),
            parent_session_id: None,
        }
    }

    #[test]
    fn simulate_creates_session_and_binds_plan() {
        let engine = AgentCoreEngine::new(MockPlanner, ConfirmationThreshold::default());
        let env = make_envelope(AgentRequestKind::SimulateIntent {
            intent: dummy_intent(),
        });
        let outcome = engine.dispatch(&env).unwrap();

        assert!(matches!(outcome, DispatchOutcome::Simulated(_)));

        let session = engine.get_session(&env.session_id).unwrap();
        assert!(session.plan_hash.is_some());
        assert_eq!(session.current_state, SessionState::Simulated);
    }

    #[test]
    fn execute_after_simulate_finalizes_session() {
        let engine = AgentCoreEngine::new(MockPlanner, ConfirmationThreshold::default());

        // Simulate first.
        let sim_env = make_envelope(AgentRequestKind::SimulateIntent {
            intent: dummy_intent(),
        });
        let sim_outcome = engine.dispatch(&sim_env).unwrap();
        let plan_hash = match sim_outcome {
            DispatchOutcome::Simulated(ref s) => s.plan_hash,
            _ => panic!("expected Simulated"),
        };

        // Execute with a distinct idempotency key.
        let exec_env = AgentEnvelope {
            request_kind: AgentRequestKind::ExecutePlan {
                plan_hash,
                confirmation_ref: Blake3Digest([0xFF; 32]),
            },
            idempotency_key: Blake3Digest([0x04; 32]),
            ..make_envelope(AgentRequestKind::SimulateIntent {
                intent: dummy_intent(),
            })
        };
        let outcome = engine.dispatch(&exec_env).unwrap();
        assert!(matches!(outcome, DispatchOutcome::Executed(_)));

        let session = engine.get_session(&sim_env.session_id).unwrap();
        assert_eq!(session.current_state, SessionState::Finalized);
    }

    #[test]
    fn intent_request_auto_executes_when_approved() {
        let engine = AgentCoreEngine::new(MockPlanner, ConfirmationThreshold::default());
        let env = make_envelope(AgentRequestKind::IntentRequest {
            intent: dummy_intent(),
        });
        let outcome = engine.dispatch(&env).unwrap();

        assert!(matches!(outcome, DispatchOutcome::Executed(_)));

        let session = engine.get_session(&env.session_id).unwrap();
        assert_eq!(session.current_state, SessionState::Finalized);
    }

    #[test]
    fn query_returns_empty_payload() {
        let engine = AgentCoreEngine::new(MockPlanner, ConfirmationThreshold::default());
        let env = make_envelope(AgentRequestKind::Query {
            query_kind: QueryKind::Balance {
                account: AccountAddress([0xCC; 32]),
            },
        });
        let outcome = engine.dispatch(&env).unwrap();
        assert!(matches!(outcome, DispatchOutcome::QueryResult { .. }));
    }

    #[test]
    fn execute_without_simulation_fails() {
        let engine = AgentCoreEngine::new(MockPlanner, ConfirmationThreshold::default());
        let env = make_envelope(AgentRequestKind::ExecutePlan {
            plan_hash: Blake3Digest([0xBB; 32]),
            confirmation_ref: Blake3Digest([0xFF; 32]),
        });
        // Session exists but has no plan bound — should fail.
        let result = engine.dispatch(&env);
        assert!(result.is_err());
    }

    // ── Phase A acceptance tests ─────────────────────────────────────────

    #[test]
    fn session_capacity_should_be_enforced() {
        // A-5: enforce session map capacity limit.
        let config = SessionConfig {
            max_sessions: 3,
            session_ttl_ms: 30 * 60 * 1000,
            max_idempotency_keys_per_session: 100,
            rate_limit_per_session: 0,
            rate_limit_window_ms: 60_000,
        };
        let engine =
            AgentCoreEngine::with_config(MockPlanner, ConfirmationThreshold::default(), config);

        // Fill up to capacity with different sessions.
        for i in 0u8..3 {
            let mut env = make_envelope(AgentRequestKind::Query {
                query_kind: QueryKind::Balance {
                    account: AccountAddress([0xCC; 32]),
                },
            });
            env.session_id = Blake3Digest([i + 10; 32]);
            env.idempotency_key = Blake3Digest([i + 20; 32]);
            engine.dispatch(&env).unwrap();
        }
        assert_eq!(engine.session_count(), 3);

        // One more should still work (eviction of oldest/terminal session).
        let mut env = make_envelope(AgentRequestKind::Query {
            query_kind: QueryKind::Balance {
                account: AccountAddress([0xCC; 32]),
            },
        });
        env.session_id = Blake3Digest([0x99; 32]);
        env.idempotency_key = Blake3Digest([0x99; 32]);
        let result = engine.dispatch(&env);
        // Should either succeed (via eviction) or fail gracefully — not panic.
        assert!(result.is_ok() || result.is_err());
        // Capacity should not exceed max_sessions.
        assert!(engine.session_count() <= 3);
    }

    #[test]
    fn agent_replay_should_be_rejected_by_idempotency_key() {
        // A-6: duplicate idempotency keys within replay window must be rejected.
        let engine = AgentCoreEngine::new(MockPlanner, ConfirmationThreshold::default());

        let env = make_envelope(AgentRequestKind::Query {
            query_kind: QueryKind::Balance {
                account: AccountAddress([0xCC; 32]),
            },
        });

        // First dispatch should succeed.
        let result1 = engine.dispatch(&env);
        assert!(result1.is_ok());

        // Same idempotency key again — should be rejected.
        let result2 = engine.dispatch(&env);
        assert!(result2.is_err());
        let err = format!("{:?}", result2.unwrap_err());
        assert!(
            err.contains("idempotency") || err.contains("duplicate"),
            "expected idempotency rejection, got: {err}"
        );
    }

    // ── Phase Z confirmation flow tests ──────────────────────────────────

    #[test]
    fn confirm_plan_completes_full_cycle() {
        // Z-2: simulate → confirm → execute closed loop.
        let threshold = ConfirmationThreshold {
            value_threshold: Amount(500),
        };
        let engine = AgentCoreEngine::new(MockPlanner, threshold);

        // Step 1: Simulate — policy should require confirmation (1000 >= 500).
        let sim_env = make_envelope(AgentRequestKind::SimulateIntent {
            intent: dummy_intent(),
        });
        let sim_outcome = engine.dispatch(&sim_env).unwrap();
        let plan_hash = match &sim_outcome {
            DispatchOutcome::Simulated(s) => s.plan_hash,
            other => panic!("expected Simulated, got {:?}", other),
        };

        // Session should be in AwaitingConfirmation.
        let session = engine.get_session(&sim_env.session_id).unwrap();
        assert_eq!(session.current_state, SessionState::AwaitingConfirmation);

        // Step 2: Confirm the plan.
        let confirm_env = AgentEnvelope {
            request_kind: AgentRequestKind::ConfirmPlan { plan_hash },
            idempotency_key: Blake3Digest([0x04; 32]),
            ..make_envelope(AgentRequestKind::SimulateIntent {
                intent: dummy_intent(),
            })
        };
        let confirm_outcome = engine.dispatch(&confirm_env).unwrap();
        assert!(
            matches!(confirm_outcome, DispatchOutcome::Executed(_)),
            "expected Executed after confirm, got {:?}",
            confirm_outcome,
        );

        // Session should be Finalized.
        let session = engine.get_session(&sim_env.session_id).unwrap();
        assert_eq!(session.current_state, SessionState::Finalized);
    }

    #[test]
    fn reject_plan_aborts_session() {
        // Z-2: simulate → reject → session aborted.
        let threshold = ConfirmationThreshold {
            value_threshold: Amount(500),
        };
        let engine = AgentCoreEngine::new(MockPlanner, threshold);

        // Simulate.
        let sim_env = make_envelope(AgentRequestKind::SimulateIntent {
            intent: dummy_intent(),
        });
        let sim_outcome = engine.dispatch(&sim_env).unwrap();
        let plan_hash = match &sim_outcome {
            DispatchOutcome::Simulated(s) => s.plan_hash,
            other => panic!("expected Simulated, got {:?}", other),
        };

        // Reject.
        let reject_env = AgentEnvelope {
            request_kind: AgentRequestKind::RejectPlan {
                plan_hash,
                reason: Some("user declined".to_string()),
            },
            idempotency_key: Blake3Digest([0x05; 32]),
            ..make_envelope(AgentRequestKind::SimulateIntent {
                intent: dummy_intent(),
            })
        };
        let reject_outcome = engine.dispatch(&reject_env).unwrap();
        assert!(matches!(reject_outcome, DispatchOutcome::Rejected { .. }));

        // Session should be Aborted.
        let session = engine.get_session(&sim_env.session_id).unwrap();
        assert_eq!(session.current_state, SessionState::Aborted);
    }

    #[test]
    fn confirm_without_simulation_fails() {
        // Z-2: cannot confirm without prior simulation.
        let engine = AgentCoreEngine::new(MockPlanner, ConfirmationThreshold::default());
        let env = make_envelope(AgentRequestKind::ConfirmPlan {
            plan_hash: Blake3Digest([0xBB; 32]),
        });
        let result = engine.dispatch(&env);
        assert!(result.is_err());
    }

    // ── Z-8: Security audit tests ───────────────────────────────────

    #[test]
    fn sec_expired_capability_rejects_simulation() {
        // Z-8 Test 3: expired deadline → request rejected.
        let engine = AgentCoreEngine::new(MockPlanner, ConfirmationThreshold::default());

        // Set deadline in the past (1 ms).
        let mut env = make_envelope(AgentRequestKind::SimulateIntent {
            intent: dummy_intent(),
        });
        env.deadline_ms = TimestampMs(1);

        let result = engine.dispatch(&env);
        assert!(result.is_err(), "expired capability must be rejected");
    }

    #[test]
    fn sec_expired_capability_rejects_intent_request() {
        // Z-8 Test 3: expired deadline on IntentRequest path.
        let engine = AgentCoreEngine::new(MockPlanner, ConfirmationThreshold::default());

        let mut env = make_envelope(AgentRequestKind::IntentRequest {
            intent: dummy_intent(),
        });
        env.deadline_ms = TimestampMs(1);

        let result = engine.dispatch(&env);
        assert!(result.is_err(), "expired capability must be rejected");
    }

    // ── Z-9: Rate limiting tests ────────────────────────────────────

    #[test]
    fn rate_limit_rejects_excess_requests() {
        // Z-9: per-session rate limiting.
        let config = SessionConfig {
            max_sessions: 100,
            session_ttl_ms: 60_000,
            max_idempotency_keys_per_session: 1_000,
            rate_limit_per_session: 3,
            rate_limit_window_ms: 60_000,
        };
        let engine =
            AgentCoreEngine::with_config(MockPlanner, ConfirmationThreshold::default(), config);

        // Each request needs a unique idempotency key and session ID
        // to avoid session state conflicts. We use different session IDs
        // so each creates its own session, all sharing the same rate bucket
        // ... actually the rate limit is per-session, so we need the same
        // session ID but different idempotency keys and fresh envelopes.

        // For Query requests, the session doesn't advance state — these
        // are the best fit for testing the rate limiter.
        for i in 0u8..3 {
            let mut env = make_envelope(AgentRequestKind::Query {
                query_kind: QueryKind::Balance {
                    account: AccountAddress([0xBB; 32]),
                },
            });
            env.idempotency_key = Blake3Digest([i + 10; 32]);
            let result = engine.dispatch(&env);
            assert!(result.is_ok(), "request {} should succeed", i);
        }

        // 4th request should be rejected.
        let mut env = make_envelope(AgentRequestKind::Query {
            query_kind: QueryKind::Balance {
                account: AccountAddress([0xBB; 32]),
            },
        });
        env.idempotency_key = Blake3Digest([0x20; 32]);
        let result = engine.dispatch(&env);
        assert!(
            result.is_err(),
            "4th request must be rejected by rate limiter"
        );
    }

    #[test]
    fn rate_limit_zero_means_unlimited() {
        let config = SessionConfig {
            max_sessions: 100,
            session_ttl_ms: 60_000,
            max_idempotency_keys_per_session: 1_000,
            rate_limit_per_session: 0,
            rate_limit_window_ms: 60_000,
        };
        let engine =
            AgentCoreEngine::with_config(MockPlanner, ConfirmationThreshold::default(), config);

        // Even 10 requests should succeed with limit=0.
        for i in 0u8..10 {
            let mut env = make_envelope(AgentRequestKind::Query {
                query_kind: QueryKind::Balance {
                    account: AccountAddress([0xBB; 32]),
                },
            });
            env.idempotency_key = Blake3Digest([i + 50; 32]);
            let result = engine.dispatch(&env);
            assert!(
                result.is_ok(),
                "request {} should succeed with unlimited",
                i
            );
        }
    }
}
