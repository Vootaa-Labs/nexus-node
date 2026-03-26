//! Concrete [`PlannerBackend`] bridging `AgentEnvelope` requests to the
//! [`IntentCompiler`] pipeline.
//!
//! Extracts the [`UserIntent`] from the envelope, calls the compiler's
//! `estimate_gas()` for simulation and `compile()` for execution, then
//! maps results into the schema expected by the Agent Core Engine.

use std::collections::HashMap;
use std::sync::Arc;

use nexus_primitives::{Blake3Digest, TimestampMs};
use parking_lot::Mutex;

use crate::agent_core::envelope::{AgentEnvelope, AgentRequestKind};
use crate::agent_core::planner::{
    compute_plan_hash, ConfirmationResponse, ExecutionReceipt, PlannerBackend, SimulationResult,
};
use crate::agent_core::session::AgentSession;
use crate::error::{IntentError, IntentResult};
use crate::traits::IntentCompiler;
use crate::types::UserIntent;

// ── IntentPlannerBridge ────────────────────────────────────────────────

/// Cached simulation data for an agent session.
struct CachedSimulation {
    estimated_gas: u64,
    step_count: usize,
    #[allow(dead_code)]
    intent_summary: String,
}

/// A [`PlannerBackend`] backed by a real [`IntentCompiler`] and
/// [`AccountResolver`].
///
/// `simulate()` runs gas estimation via the compiler and caches the
/// result; `confirm()` binds the confirmation reference to the plan;
/// `execute()` returns an execution receipt.
///
/// # Current Limitation
///
/// The `execute()` method returns deterministic-but-synthetic transaction
/// hashes because the agent-core envelope carries an unsigned `UserIntent`,
/// not a `SignedUserIntent` required by `IntentCompiler::compile()`.
/// Real on-chain execution for the agent path requires a signing
/// pipeline (tracked as future work).  The REST submit path
/// (`/v2/intent/submit`) already produces real on-chain transactions.
pub struct IntentPlannerBridge<C: IntentCompiler> {
    compiler: Arc<C>,
    resolver: Arc<C::Resolver>,
    /// Cache of simulation results keyed by session_id, so that
    /// `confirm()` and `execute()` can access the original estimate.
    sim_cache: Mutex<HashMap<Blake3Digest, CachedSimulation>>,
}

impl<C: IntentCompiler> IntentPlannerBridge<C> {
    /// Create a new bridge.
    pub fn new(compiler: Arc<C>, resolver: Arc<C::Resolver>) -> Self {
        Self {
            compiler,
            resolver,
            sim_cache: Mutex::new(HashMap::new()),
        }
    }
}

impl<C: IntentCompiler> PlannerBackend for IntentPlannerBridge<C> {
    fn simulate(&self, envelope: &AgentEnvelope) -> IntentResult<SimulationResult> {
        let intent = extract_intent(envelope)?;

        // Run async estimate_gas() on the current tokio runtime.
        // `block_in_place` is needed because `PlannerBackend` is sync
        // but the compiler trait is async.
        let handle = tokio::runtime::Handle::current();
        let compiler = self.compiler.clone();
        let resolver = self.resolver.clone();
        let intent_clone = intent.clone();
        let estimate = tokio::task::block_in_place(|| {
            handle.block_on(compiler.estimate_gas(&intent_clone, &resolver))
        })?;

        // Generate a human-readable summary for the confirmation UI.
        let summary = summarize_intent(intent, &estimate);

        // Compute plan hash binding the simulation to this request.
        let sim_data = bcs::to_bytes(&estimate).map_err(|e| IntentError::Codec(e.to_string()))?;
        let plan_hash = compute_plan_hash(&envelope.request_id, &sim_data)?;

        // Cache simulation data for confirm()/execute().
        self.sim_cache.lock().insert(
            envelope.session_id,
            CachedSimulation {
                estimated_gas: estimate.gas_units,
                step_count: estimate.shards_touched as usize,
                intent_summary: summary.clone(),
            },
        );

        Ok(SimulationResult {
            session_id: envelope.session_id,
            plan_hash,
            estimated_gas: estimate.gas_units,
            step_count: estimate.shards_touched as usize,
            requires_cross_shard: estimate.requires_cross_shard,
            simulated_at_ms: TimestampMs(now_ms()),
            summary,
        })
    }

    fn confirm(
        &self,
        session: &AgentSession,
        plan_hash: &Blake3Digest,
    ) -> IntentResult<ConfirmationResponse> {
        // Verify the plan hash matches what we simulated.
        let bound =
            session
                .plan_hash
                .as_ref()
                .ok_or_else(|| IntentError::AgentCapabilityDenied {
                    reason: "session has no bound plan hash — simulate first".into(),
                })?;
        if bound != plan_hash {
            return Err(IntentError::AgentCapabilityDenied {
                reason: format!(
                    "plan hash mismatch: session bound {:?}, supplied {:?}",
                    bound, plan_hash,
                ),
            });
        }

        // Derive a deterministic confirmation reference from the plan hash
        // and session ID. This ensures replay protection: the same
        // (session, plan) produces the same confirmation_ref.
        let confirmation_ref = {
            let mut hasher = blake3::Hasher::new();
            hasher.update(b"nexus::intent_planner_bridge::confirm::v1");
            hasher.update(&plan_hash.0);
            hasher.update(&session.session_id.0);
            Blake3Digest(*hasher.finalize().as_bytes())
        };

        Ok(ConfirmationResponse {
            session_id: session.session_id,
            plan_hash: *plan_hash,
            confirmation_ref,
            confirmed_at_ms: TimestampMs(now_ms()),
        })
    }

    fn execute(
        &self,
        session: &AgentSession,
        _confirmation_ref: &Blake3Digest,
    ) -> IntentResult<ExecutionReceipt> {
        let plan_hash = session.plan_hash.unwrap_or(Blake3Digest([0u8; 32]));

        // Retrieve cached simulation data (gas estimate, step count).
        let cached = self.sim_cache.lock().remove(&session.session_id);
        let (estimated_gas, step_count) = match cached {
            Some(c) => (c.estimated_gas, c.step_count),
            None => (0, 1), // Fallback if simulate wasn't called
        };

        // Derive deterministic synthetic tx hashes — one per step.
        // NOTE: These are NOT real on-chain tx digests. The REST submit
        // path produces real transactions. The agent-core path needs a
        // signing pipeline to produce real SignedTransactions.
        let tx_hashes: Vec<Blake3Digest> = (0..step_count.max(1))
            .map(|i| {
                let mut hasher = blake3::Hasher::new();
                hasher.update(b"nexus::intent_planner_bridge::tx::v1");
                hasher.update(&plan_hash.0);
                hasher.update(&session.session_id.0);
                hasher.update(&(i as u32).to_le_bytes());
                Blake3Digest(*hasher.finalize().as_bytes())
            })
            .collect();

        Ok(ExecutionReceipt {
            session_id: session.session_id,
            plan_hash,
            tx_hashes,
            gas_used: estimated_gas,
            completed_at_ms: TimestampMs(now_ms()),
        })
    }
}

// ── Helpers ────────────────────────────────────────────────────────────

/// Extract the [`UserIntent`] from an envelope's request kind.
fn extract_intent(envelope: &AgentEnvelope) -> IntentResult<&UserIntent> {
    match &envelope.request_kind {
        AgentRequestKind::SimulateIntent { intent } => Ok(intent),
        AgentRequestKind::IntentRequest { intent } => Ok(intent),
        other => Err(IntentError::AgentSpecError {
            reason: format!(
                "expected intent-bearing request, got {:?}",
                std::mem::discriminant(other)
            ),
        }),
    }
}

/// Current wall-clock time in milliseconds (monotonic not required).
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Generate a human-readable summary of a user intent for confirmation display.
fn summarize_intent(intent: &UserIntent, estimate: &crate::types::GasEstimate) -> String {
    let action = match intent {
        UserIntent::Transfer { to, token, amount } => {
            format!("Transfer {} {:?} to {}", amount.0, token, hex_short(&to.0))
        }
        UserIntent::Swap {
            from_token,
            to_token,
            amount,
            max_slippage_bps,
        } => {
            format!(
                "Swap {} {:?} → {:?} (max slippage {}bps)",
                amount.0, from_token, to_token, max_slippage_bps,
            )
        }
        UserIntent::ContractCall {
            contract,
            function,
            gas_budget,
            ..
        } => {
            format!(
                "Call {}.{}() on {} (gas budget {})",
                hex_short(&contract.0),
                function,
                hex_short(&contract.0),
                gas_budget,
            )
        }
        UserIntent::Stake { validator, amount } => {
            format!("Stake {} with validator {}", amount.0, validator.0)
        }
        UserIntent::AgentTask { spec } => {
            format!("Agent task ({})", spec.version)
        }
    };

    let cross_shard = if estimate.requires_cross_shard {
        " [cross-shard]"
    } else {
        ""
    };

    format!(
        "{}{} — estimated gas: {}, shards: {}",
        action, cross_shard, estimate.gas_units, estimate.shards_touched,
    )
}

/// Abbreviate a 32-byte address to a short hex string (e.g. "0xaabb..ccdd").
fn hex_short(bytes: &[u8; 32]) -> String {
    format!(
        "0x{:02x}{:02x}..{:02x}{:02x}",
        bytes[0], bytes[1], bytes[30], bytes[31],
    )
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_core::envelope::{
        AgentEnvelope, AgentExecutionConstraints, AgentPrincipal, ProtocolKind,
    };
    use crate::agent_core::session::SessionState;
    use crate::traits::AccountResolver;
    use crate::types::{CompiledIntentPlan, GasEstimate, SignedUserIntent};
    use nexus_primitives::{AccountAddress, Amount, EpochNumber, ShardId, TokenId};

    // ── Fake compiler + resolver ────────────────────────────────────

    struct FakeResolver;

    impl AccountResolver for FakeResolver {
        async fn balance(
            &self,
            _account: &AccountAddress,
            _token: &TokenId,
        ) -> IntentResult<Amount> {
            Ok(Amount(1_000_000))
        }

        async fn primary_shard(&self, _account: &AccountAddress) -> IntentResult<ShardId> {
            Ok(ShardId(0))
        }

        async fn contract_location(
            &self,
            _contract: &nexus_primitives::ContractAddress,
        ) -> IntentResult<crate::types::ContractLocation> {
            Err(IntentError::ContractNotFound {
                contract: nexus_primitives::ContractAddress([0xFF; 32]),
            })
        }
    }

    struct FakeCompiler;

    impl IntentCompiler for FakeCompiler {
        type Resolver = FakeResolver;

        async fn compile(
            &self,
            _intent: &SignedUserIntent,
            _resolver: &FakeResolver,
        ) -> IntentResult<CompiledIntentPlan> {
            Ok(CompiledIntentPlan {
                intent_id: Blake3Digest([0x01; 32]),
                steps: vec![],
                requires_htlc: false,
                estimated_gas: 10_000,
                expires_at: EpochNumber(100),
            })
        }

        async fn estimate_gas(
            &self,
            _intent: &UserIntent,
            _resolver: &FakeResolver,
        ) -> IntentResult<GasEstimate> {
            Ok(GasEstimate {
                gas_units: 42_000,
                shards_touched: 1,
                requires_cross_shard: false,
            })
        }
    }

    // ── Helpers ─────────────────────────────────────────────────────

    fn make_bridge() -> IntentPlannerBridge<FakeCompiler> {
        IntentPlannerBridge::new(Arc::new(FakeCompiler), Arc::new(FakeResolver))
    }

    fn make_envelope(kind: AgentRequestKind) -> AgentEnvelope {
        AgentEnvelope {
            protocol_kind: ProtocolKind::Mcp,
            protocol_version: "test/v1".to_string(),
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

    fn dummy_intent() -> UserIntent {
        UserIntent::Transfer {
            to: AccountAddress([0xBB; 32]),
            token: TokenId::Native,
            amount: Amount(1_000),
        }
    }

    // ── Tests ───────────────────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn simulate_returns_plan_hash_and_gas() {
        let bridge = make_bridge();
        let env = make_envelope(AgentRequestKind::SimulateIntent {
            intent: dummy_intent(),
        });
        let result = bridge.simulate(&env).unwrap();
        assert_eq!(result.estimated_gas, 42_000);
        assert_eq!(result.step_count, 1);
        assert!(!result.requires_cross_shard);
        // Plan hash must be deterministic.
        let result2 = bridge.simulate(&env).unwrap();
        assert_eq!(result.plan_hash, result2.plan_hash);
    }

    #[tokio::test]
    async fn execute_returns_receipt_with_tx_hashes() {
        let bridge = make_bridge();
        let session = AgentSession {
            session_id: Blake3Digest([0x02; 32]),
            created_at_ms: TimestampMs(1_000),
            replay_window_ms: 300_000,
            current_state: SessionState::Executing,
            plan_hash: Some(Blake3Digest([0xAA; 32])),
            confirmation_ref: Some(Blake3Digest([0xFF; 32])),
        };
        let receipt = bridge.execute(&session, &Blake3Digest([0xFF; 32])).unwrap();
        assert_eq!(receipt.session_id, session.session_id);
        assert_eq!(receipt.plan_hash, Blake3Digest([0xAA; 32]));
        assert_eq!(receipt.tx_hashes.len(), 1);
    }

    #[tokio::test]
    async fn simulate_rejects_non_intent_requests() {
        let bridge = make_bridge();
        let env = make_envelope(AgentRequestKind::ExecutePlan {
            plan_hash: Blake3Digest([0x01; 32]),
            confirmation_ref: Blake3Digest([0x02; 32]),
        });
        assert!(bridge.simulate(&env).is_err());
    }

    #[tokio::test]
    async fn execute_deterministic_tx_hash() {
        let bridge = make_bridge();
        let session = AgentSession {
            session_id: Blake3Digest([0x02; 32]),
            created_at_ms: TimestampMs(1_000),
            replay_window_ms: 300_000,
            current_state: SessionState::Executing,
            plan_hash: Some(Blake3Digest([0xCC; 32])),
            confirmation_ref: Some(Blake3Digest([0xFF; 32])),
        };
        let r1 = bridge.execute(&session, &Blake3Digest([0xFF; 32])).unwrap();
        let r2 = bridge.execute(&session, &Blake3Digest([0xFF; 32])).unwrap();
        assert_eq!(r1.tx_hashes, r2.tx_hashes);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn confirm_returns_deterministic_ref() {
        let bridge = make_bridge();
        let env = make_envelope(AgentRequestKind::SimulateIntent {
            intent: dummy_intent(),
        });
        let sim = bridge.simulate(&env).unwrap();

        let session = AgentSession {
            session_id: Blake3Digest([0x02; 32]),
            created_at_ms: TimestampMs(1_000),
            replay_window_ms: 300_000,
            current_state: SessionState::AwaitingConfirmation,
            plan_hash: Some(sim.plan_hash),
            confirmation_ref: None,
        };

        let resp1 = bridge.confirm(&session, &sim.plan_hash).unwrap();
        let resp2 = bridge.confirm(&session, &sim.plan_hash).unwrap();
        assert_eq!(resp1.confirmation_ref, resp2.confirmation_ref);
        assert_eq!(resp1.plan_hash, sim.plan_hash);
    }

    #[tokio::test]
    async fn confirm_rejects_mismatched_plan_hash() {
        let bridge = make_bridge();
        let session = AgentSession {
            session_id: Blake3Digest([0x02; 32]),
            created_at_ms: TimestampMs(1_000),
            replay_window_ms: 300_000,
            current_state: SessionState::AwaitingConfirmation,
            plan_hash: Some(Blake3Digest([0xAA; 32])),
            confirmation_ref: None,
        };
        let wrong_hash = Blake3Digest([0xBB; 32]);
        assert!(bridge.confirm(&session, &wrong_hash).is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn simulate_summary_contains_intent_info() {
        let bridge = make_bridge();
        let env = make_envelope(AgentRequestKind::SimulateIntent {
            intent: dummy_intent(),
        });
        let sim = bridge.simulate(&env).unwrap();
        assert!(!sim.summary.is_empty());
        assert!(sim.summary.contains("Transfer"));
        assert!(sim.summary.contains("1000"));
    }
}
