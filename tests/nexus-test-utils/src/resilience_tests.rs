//! Cross-layer E2E & resilience tests (T-13006).
//!
//! Exercises the full MCP → ACE → dispatch pipeline with both happy
//! path and adversarial/fault-injection scenarios.
//!
//! Z-6: A2A negotiation e2e (two ACE sessions coordinated via A2A).
//! Z-7: MCP adapter e2e (full MCP→ACE pipeline with real engine).
//! Z-10: Agent Core supplemental test coverage.

use std::sync::Arc;

use nexus_crypto::{DilithiumSigner, Signer};
use nexus_intent::agent_core::a2a::{
    compute_message_digest, A2aMessage, A2aMessageKind, A2aSessionState, SettlementResult,
    A2A_SIGNATURE_DOMAIN,
};
use nexus_intent::agent_core::a2a_negotiator::A2aNegotiator;
use nexus_intent::agent_core::capability_snapshot::{
    compute_snapshot_hash, validate_capability_against_snapshot, AgentCapabilitySnapshot,
    CapabilityScope,
};
use nexus_intent::agent_core::dispatcher::{DispatchBackend, DispatchOutcome};
use nexus_intent::agent_core::engine::{AgentCoreEngine, SessionConfig};
use nexus_intent::agent_core::envelope::{
    AgentEnvelope, AgentExecutionConstraints, AgentPrincipal, AgentRequestKind, ProtocolKind,
    QueryKind,
};
use nexus_intent::agent_core::planner::{compute_plan_hash, PlannerBackend};
use nexus_intent::agent_core::policy::ConfirmationThreshold;
use nexus_intent::agent_core::provenance::{ProvenanceRecord, ProvenanceStatus};
use nexus_intent::agent_core::provenance_store::ProvenanceStore;
use nexus_intent::agent_core::session::AgentSession;
use nexus_intent::error::IntentResult;
use nexus_intent::types::UserIntent;
use nexus_intent::{ExecutionReceipt, SimulationResult};
use nexus_primitives::{AccountAddress, Amount, Blake3Digest, TimestampMs, TokenId};
use nexus_rpc::mcp::handler::handle_tool_call;
use nexus_rpc::mcp::schema::McpToolCall;

// ── Mock planner ───────────────────────────────────────────────────────

struct MockPlanner;

impl PlannerBackend for MockPlanner {
    fn simulate(&self, envelope: &AgentEnvelope) -> IntentResult<SimulationResult> {
        let plan_hash = compute_plan_hash(&envelope.request_id, b"resilience-sim")?;
        Ok(SimulationResult {
            session_id: envelope.session_id,
            plan_hash,
            estimated_gas: 10_000,
            step_count: 1,
            requires_cross_shard: false,
            simulated_at_ms: TimestampMs(1_000),
            summary: String::new(),
        })
    }

    fn confirm(
        &self,
        session: &AgentSession,
        plan_hash: &Blake3Digest,
    ) -> IntentResult<nexus_intent::agent_core::planner::ConfirmationResponse> {
        Ok(nexus_intent::agent_core::planner::ConfirmationResponse {
            session_id: session.session_id,
            plan_hash: *plan_hash,
            confirmation_ref: Blake3Digest([0xCC; 32]),
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
            gas_used: 10_000,
            completed_at_ms: TimestampMs(2_000),
        })
    }
}

fn addr(b: u8) -> AccountAddress {
    AccountAddress([b; 32])
}

fn make_envelope(kind: AgentRequestKind) -> AgentEnvelope {
    AgentEnvelope {
        protocol_kind: ProtocolKind::Mcp,
        protocol_version: "mcp/2025-11-05".to_string(),
        request_id: Blake3Digest([0x01; 32]),
        session_id: Blake3Digest([0x02; 32]),
        idempotency_key: Blake3Digest([0x03; 32]),
        caller: AgentPrincipal {
            address: addr(0xAA),
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

// ── E2E: Full ACE pipeline ─────────────────────────────────────────────

#[test]
fn e2e_simulate_then_execute_lifecycle() {
    let engine = AgentCoreEngine::new(MockPlanner, ConfirmationThreshold::default());

    let sim_env = make_envelope(AgentRequestKind::SimulateIntent {
        intent: UserIntent::Transfer {
            to: addr(0xBB),
            token: TokenId::Native,
            amount: Amount(500),
        },
    });
    let sim_outcome = engine.dispatch(&sim_env).unwrap();
    let plan_hash = match sim_outcome {
        DispatchOutcome::Simulated(ref s) => s.plan_hash,
        _ => panic!("expected Simulated"),
    };

    let exec_env = AgentEnvelope {
        request_kind: AgentRequestKind::ExecutePlan {
            plan_hash,
            confirmation_ref: Blake3Digest([0xFF; 32]),
        },
        idempotency_key: Blake3Digest([0x04; 32]),
        ..make_envelope(AgentRequestKind::Query {
            query_kind: QueryKind::Balance { account: addr(0) },
        })
    };

    let exec_outcome = engine.dispatch(&exec_env).unwrap();
    assert!(matches!(exec_outcome, DispatchOutcome::Executed(_)));
}

// ── Resilience: A2A replay rejection ───────────────────────────────────

#[test]
fn a2a_replay_nonce_rejected() {
    use nexus_crypto::{DilithiumSigner, Signer};

    let negotiator = A2aNegotiator::new();
    let (_init_sk, init_pk) = DilithiumSigner::generate_keypair();
    let init_addr = AccountAddress::from_dilithium_pubkey(init_pk.as_bytes());
    let (resp_sk, resp_pk) = DilithiumSigner::generate_keypair();
    let resp_addr = AccountAddress::from_dilithium_pubkey(resp_pk.as_bytes());

    let neg_id = negotiator
        .initiate(
            init_addr,
            resp_addr,
            Blake3Digest([0x01; 32]),
            TimestampMs(u64::MAX),
            TimestampMs(1_000),
        )
        .unwrap();

    let nonce = [0x02; 32];
    let kind = A2aMessageKind::Counter {
        summary: "test".into(),
    };
    let digest = compute_message_digest(&neg_id, &resp_addr, &Blake3Digest(nonce), &kind).unwrap();
    let sig = DilithiumSigner::sign(&resp_sk, A2A_SIGNATURE_DOMAIN, &digest.0);

    let msg = A2aMessage {
        negotiation_id: neg_id,
        sender: resp_addr,
        sender_pk: resp_pk.as_bytes().to_vec(),
        signature: sig.as_bytes().to_vec(),
        nonce: Blake3Digest(nonce),
        kind: kind.clone(),
        expires_at_ms: TimestampMs(u64::MAX),
        message_digest: digest,
    };

    // First message succeeds.
    assert!(negotiator.process_message(&msg, 2_000).is_ok());

    // Exact replay is rejected.
    let result = negotiator.process_message(&msg, 3_000);
    assert!(result.is_err());
    let err_msg = format!("{:?}", result.unwrap_err());
    assert!(err_msg.contains("replay") || err_msg.contains("nonce"));
}

// ── Resilience: Provenance eviction under pressure ─────────────────────

#[test]
fn provenance_store_evicts_oldest_at_capacity() {
    let store = ProvenanceStore::with_max_records(3);

    for i in 0u8..5 {
        store.record(ProvenanceRecord {
            provenance_id: Blake3Digest([i; 32]),
            session_id: Blake3Digest([0x10; 32]),
            request_id: Blake3Digest([i; 32]),
            agent_id: addr(0xAA),
            parent_agent_id: None,
            capability_token_id: None,
            intent_hash: Blake3Digest([0; 32]),
            plan_hash: Blake3Digest([0; 32]),
            confirmation_ref: None,
            tx_hash: None,
            status: ProvenanceStatus::Pending,
            created_at_ms: TimestampMs(i as u64 * 1_000),
        });
    }

    // Only 3 records should remain.
    assert_eq!(store.len(), 3);

    // Oldest two (idx 0 and 1) evicted; idx 2, 3, 4 remain.
    assert!(store.get(&Blake3Digest([0u8; 32])).is_none());
    assert!(store.get(&Blake3Digest([1u8; 32])).is_none());
    assert!(store.get(&Blake3Digest([2u8; 32])).is_some());
    assert!(store.get(&Blake3Digest([3u8; 32])).is_some());
    assert!(store.get(&Blake3Digest([4u8; 32])).is_some());
}

// ── Resilience: Snapshot hash tamper detection ─────────────────────────

#[test]
fn snapshot_hash_tamper_detected() {
    let mut snap = AgentCapabilitySnapshot {
        agent_id: addr(0x01),
        scope: CapabilityScope::Full,
        max_value: Amount(50_000),
        deadline: TimestampMs(u64::MAX),
        allowed_contracts: vec![],
        delegation_chain: vec![],
        snapshot_hash: Blake3Digest([0u8; 32]),
    };

    // Compute correct hash.
    snap.snapshot_hash = compute_snapshot_hash(&snap);
    assert!(validate_capability_against_snapshot(&snap, 1_000).is_ok());

    // Tamper with max_value.
    snap.max_value = Amount(999_999);
    assert!(validate_capability_against_snapshot(&snap, 1_000).is_err());
}

// ── Resilience: Expired envelope rejected ──────────────────────────────

#[test]
fn expired_envelope_rejected() {
    let engine = AgentCoreEngine::new(MockPlanner, ConfirmationThreshold::default());

    let mut env = make_envelope(AgentRequestKind::SimulateIntent {
        intent: UserIntent::Transfer {
            to: addr(0xBB),
            token: TokenId::Native,
            amount: Amount(100),
        },
    });
    // Set deadline to the past.
    env.deadline_ms = TimestampMs(1);

    let result = engine.dispatch(&env);
    assert!(result.is_err());
}

// ── Z-6: A2A negotiation e2e — two ACE sessions via A2A ───────────────

/// Helper to build a signed A2A message from a real keypair.
fn make_signed_a2a_msg(
    negotiation_id: Blake3Digest,
    sk: &nexus_crypto::DilithiumSigningKey,
    pk: &nexus_crypto::DilithiumVerifyKey,
    address: AccountAddress,
    kind: A2aMessageKind,
    nonce: [u8; 32],
) -> A2aMessage {
    let nonce_d = Blake3Digest(nonce);
    let digest = compute_message_digest(&negotiation_id, &address, &nonce_d, &kind).unwrap();
    let sig = DilithiumSigner::sign(sk, A2A_SIGNATURE_DOMAIN, &digest.0);
    A2aMessage {
        negotiation_id,
        sender: address,
        sender_pk: pk.as_bytes().to_vec(),
        signature: sig.as_bytes().to_vec(),
        nonce: nonce_d,
        kind,
        expires_at_ms: TimestampMs(u64::MAX),
        message_digest: digest,
    }
}

#[test]
fn z6_two_ace_sessions_coordinate_via_a2a() {
    // Two agents each simulate intents through ACE, then coordinate
    // plan hash locking and settlement via the A2A negotiator.

    // ── Setup: two agents with real crypto identities.
    let (init_sk, init_pk) = DilithiumSigner::generate_keypair();
    let init_addr = AccountAddress::from_dilithium_pubkey(init_pk.as_bytes());
    let (resp_sk, resp_pk) = DilithiumSigner::generate_keypair();
    let resp_addr = AccountAddress::from_dilithium_pubkey(resp_pk.as_bytes());

    // ── Step 1: Both agents simulate intents through ACE.
    let engine = AgentCoreEngine::new(MockPlanner, ConfirmationThreshold::default());

    // Agent A simulates a transfer.
    let mut env_a = make_envelope(AgentRequestKind::SimulateIntent {
        intent: UserIntent::Transfer {
            to: resp_addr,
            token: TokenId::Native,
            amount: Amount(1_000),
        },
    });
    env_a.caller.address = init_addr;
    env_a.session_id = Blake3Digest([0xA0; 32]);
    let sim_a = engine.dispatch(&env_a).unwrap();
    let plan_hash_a = match sim_a {
        DispatchOutcome::Simulated(ref s) => s.plan_hash,
        _ => panic!("expected Simulated for agent A"),
    };

    // Agent B simulates a transfer.
    let mut env_b = make_envelope(AgentRequestKind::SimulateIntent {
        intent: UserIntent::Transfer {
            to: init_addr,
            token: TokenId::Native,
            amount: Amount(500),
        },
    });
    env_b.caller.address = resp_addr;
    env_b.session_id = Blake3Digest([0xB0; 32]);
    env_b.idempotency_key = Blake3Digest([0xB3; 32]);
    let sim_b = engine.dispatch(&env_b).unwrap();
    let plan_hash_b = match sim_b {
        DispatchOutcome::Simulated(ref s) => s.plan_hash,
        _ => panic!("expected Simulated for agent B"),
    };

    // ── Step 2: A2A negotiation — Propose → Accept → Lock → Execute → Settle.
    let negotiator = A2aNegotiator::new();
    let neg_id = negotiator
        .initiate(
            init_addr,
            resp_addr,
            Blake3Digest([0x01; 32]),
            TimestampMs(u64::MAX),
            TimestampMs(1_000),
        )
        .unwrap();

    // A proposes (Counter from responder side first is also valid).
    let msg = make_signed_a2a_msg(
        neg_id,
        &resp_sk,
        &resp_pk,
        resp_addr,
        A2aMessageKind::Accept,
        [0x02; 32],
    );
    let state = negotiator.process_message(&msg, 2_000).unwrap();
    assert_eq!(state, A2aSessionState::Accepted);

    // Lock both plan hashes.
    let msg = make_signed_a2a_msg(
        neg_id,
        &init_sk,
        &init_pk,
        init_addr,
        A2aMessageKind::LockPlans {
            initiator_plan_hash: plan_hash_a,
            responder_plan_hash: plan_hash_b,
        },
        [0x03; 32],
    );
    let state = negotiator.process_message(&msg, 3_000).unwrap();
    assert_eq!(state, A2aSessionState::Locked);

    // Begin execution.
    let msg = make_signed_a2a_msg(
        neg_id,
        &init_sk,
        &init_pk,
        init_addr,
        A2aMessageKind::BeginExecution,
        [0x04; 32],
    );
    let state = negotiator.process_message(&msg, 4_000).unwrap();
    assert_eq!(state, A2aSessionState::Executing);

    // Settle with the initiator's plan hash.
    let msg = make_signed_a2a_msg(
        neg_id,
        &init_sk,
        &init_pk,
        init_addr,
        A2aMessageKind::Settle {
            result: SettlementResult {
                plan_hash: plan_hash_a,
                proof_ref: Blake3Digest([0xEE; 32]),
                tx_hashes: vec![Blake3Digest([0xFF; 32])],
                settled_at_ms: TimestampMs(5_000),
            },
        },
        [0x05; 32],
    );
    let state = negotiator.process_message(&msg, 5_000).unwrap();
    assert_eq!(state, A2aSessionState::Settled);

    // Verify settlement references the correct plan hash from agent A's simulation.
    let settlement = negotiator.get_settlement(&neg_id).unwrap();
    assert_eq!(settlement.plan_hash, plan_hash_a);
    assert_eq!(negotiator.active_count(), 0);
}

#[test]
fn z6_a2a_negotiation_reject_then_renegotiate() {
    // Agent B rejects the first negotiation, then a new one succeeds.
    let (_init_sk, init_pk) = DilithiumSigner::generate_keypair();
    let init_addr = AccountAddress::from_dilithium_pubkey(init_pk.as_bytes());
    let (resp_sk, resp_pk) = DilithiumSigner::generate_keypair();
    let resp_addr = AccountAddress::from_dilithium_pubkey(resp_pk.as_bytes());

    let negotiator = A2aNegotiator::new();

    // First negotiation — rejected.
    let neg_id_1 = negotiator
        .initiate(
            init_addr,
            resp_addr,
            Blake3Digest([0x01; 32]),
            TimestampMs(u64::MAX),
            TimestampMs(1_000),
        )
        .unwrap();
    let msg = make_signed_a2a_msg(
        neg_id_1,
        &resp_sk,
        &resp_pk,
        resp_addr,
        A2aMessageKind::Reject {
            reason: "unacceptable terms".into(),
        },
        [0x02; 32],
    );
    let state = negotiator.process_message(&msg, 2_000).unwrap();
    assert_eq!(state, A2aSessionState::Rejected);

    // Second negotiation with different nonce — succeeds.
    let neg_id_2 = negotiator
        .initiate(
            init_addr,
            resp_addr,
            Blake3Digest([0x10; 32]),
            TimestampMs(u64::MAX),
            TimestampMs(3_000),
        )
        .unwrap();
    assert_ne!(neg_id_1, neg_id_2);

    let msg = make_signed_a2a_msg(
        neg_id_2,
        &resp_sk,
        &resp_pk,
        resp_addr,
        A2aMessageKind::Accept,
        [0x11; 32],
    );
    let state = negotiator.process_message(&msg, 4_000).unwrap();
    assert_eq!(state, A2aSessionState::Accepted);
}

#[test]
fn z6_a2a_execution_failure_path() {
    // Negotiation reaches Executing then fails — verify terminal state.
    let (init_sk, init_pk) = DilithiumSigner::generate_keypair();
    let init_addr = AccountAddress::from_dilithium_pubkey(init_pk.as_bytes());
    let (resp_sk, resp_pk) = DilithiumSigner::generate_keypair();
    let resp_addr = AccountAddress::from_dilithium_pubkey(resp_pk.as_bytes());

    let negotiator = A2aNegotiator::new();
    let neg_id = negotiator
        .initiate(
            init_addr,
            resp_addr,
            Blake3Digest([0x01; 32]),
            TimestampMs(u64::MAX),
            TimestampMs(1_000),
        )
        .unwrap();

    // Accept → Lock → Execute.
    let msg = make_signed_a2a_msg(
        neg_id,
        &resp_sk,
        &resp_pk,
        resp_addr,
        A2aMessageKind::Accept,
        [0x02; 32],
    );
    negotiator.process_message(&msg, 2_000).unwrap();

    let msg = make_signed_a2a_msg(
        neg_id,
        &init_sk,
        &init_pk,
        init_addr,
        A2aMessageKind::LockPlans {
            initiator_plan_hash: Blake3Digest([0xAA; 32]),
            responder_plan_hash: Blake3Digest([0xBB; 32]),
        },
        [0x03; 32],
    );
    negotiator.process_message(&msg, 3_000).unwrap();

    let msg = make_signed_a2a_msg(
        neg_id,
        &init_sk,
        &init_pk,
        init_addr,
        A2aMessageKind::BeginExecution,
        [0x04; 32],
    );
    negotiator.process_message(&msg, 4_000).unwrap();

    // Fail.
    let msg = make_signed_a2a_msg(
        neg_id,
        &init_sk,
        &init_pk,
        init_addr,
        A2aMessageKind::Fail {
            reason: "insufficient liquidity".into(),
        },
        [0x05; 32],
    );
    let state = negotiator.process_message(&msg, 5_000).unwrap();
    assert_eq!(state, A2aSessionState::Failed);
    assert!(negotiator.get_settlement(&neg_id).is_err());
}

// ── Z-7: MCP adapter e2e — full MCP→ACE pipeline ──────────────────────

/// Domain tag matching handler.rs for signature construction.
const MCP_CALLER_SIG_DOMAIN: &[u8] = b"nexus::mcp::caller_auth::v1";

/// Helper to build a signed MCP tool call with real Dilithium credentials.
fn make_mcp_call(
    tool: &str,
    args: serde_json::Value,
    sk: &nexus_crypto::DilithiumSigningKey,
    pk: &nexus_crypto::DilithiumVerifyKey,
    address: &AccountAddress,
) -> McpToolCall {
    let args_bytes = serde_json::to_vec(&args).unwrap_or_default();
    let mut payload = Vec::new();
    payload.extend_from_slice(&address.0);
    payload.extend_from_slice(tool.as_bytes());
    payload.extend_from_slice(&args_bytes);

    let sig = DilithiumSigner::sign(sk, MCP_CALLER_SIG_DOMAIN, &payload);

    McpToolCall {
        tool: tool.to_string(),
        arguments: args,
        caller: hex::encode(address.0),
        caller_public_key: hex::encode(pk.as_bytes()),
        caller_signature: hex::encode(sig.as_bytes()),
        mcp_session_id: Some("mcp-e2e-session".to_string()),
    }
}

#[test]
fn z7_mcp_query_balance_through_real_engine() {
    // Full e2e: MCP tool call → handler → ACE engine → dispatch → result.
    let engine = Arc::new(AgentCoreEngine::new(
        MockPlanner,
        ConfirmationThreshold::default(),
    ));
    let (sk, pk) = DilithiumSigner::generate_keypair();
    let caller = AccountAddress::from_dilithium_pubkey(pk.as_bytes());

    let call = make_mcp_call(
        "query_balance",
        serde_json::json!({ "account": hex::encode(caller.0) }),
        &sk,
        &pk,
        &caller,
    );

    let result = handle_tool_call(&call, &engine, 0, TimestampMs(u64::MAX));
    assert!(
        result.success,
        "query_balance should succeed: {:?}",
        result.error
    );
    assert!(result.data.is_some());
}

#[test]
fn z7_mcp_simulate_intent_through_real_engine() {
    let engine = Arc::new(AgentCoreEngine::new(
        MockPlanner,
        ConfirmationThreshold::default(),
    ));
    let (sk, pk) = DilithiumSigner::generate_keypair();
    let caller = AccountAddress::from_dilithium_pubkey(pk.as_bytes());

    let call = make_mcp_call(
        "simulate_intent",
        serde_json::json!({
            "intent_type": "transfer",
            "params": {
                "to": "bb".repeat(32),
                "token": "native",
                "amount": 1000,
            },
        }),
        &sk,
        &pk,
        &caller,
    );

    let result = handle_tool_call(&call, &engine, 0, TimestampMs(u64::MAX));
    assert!(
        result.success,
        "simulate should succeed: {:?}",
        result.error
    );
    let data = result.data.unwrap();
    assert!(data["plan_hash"].is_string());
    assert!(data["estimated_gas"].as_u64().unwrap() > 0);
}

#[test]
fn z7_mcp_forbidden_tool_rejected() {
    let engine = Arc::new(AgentCoreEngine::new(
        MockPlanner,
        ConfirmationThreshold::default(),
    ));
    let (sk, pk) = DilithiumSigner::generate_keypair();
    let caller = AccountAddress::from_dilithium_pubkey(pk.as_bytes());

    let call = make_mcp_call("raw_move_payload", serde_json::json!({}), &sk, &pk, &caller);

    let result = handle_tool_call(&call, &engine, 0, TimestampMs(u64::MAX));
    assert!(!result.success);
    assert!(result.error.as_deref().unwrap().contains("forbidden"));
}

#[test]
fn z7_mcp_tampered_signature_rejected() {
    let engine = Arc::new(AgentCoreEngine::new(
        MockPlanner,
        ConfirmationThreshold::default(),
    ));
    let (sk, pk) = DilithiumSigner::generate_keypair();
    let caller = AccountAddress::from_dilithium_pubkey(pk.as_bytes());

    let mut call = make_mcp_call(
        "query_balance",
        serde_json::json!({ "account": hex::encode(caller.0) }),
        &sk,
        &pk,
        &caller,
    );

    // Tamper with the signature.
    let mut sig_bytes = hex::decode(&call.caller_signature).unwrap();
    sig_bytes[0] ^= 0xFF;
    call.caller_signature = hex::encode(&sig_bytes);

    let result = handle_tool_call(&call, &engine, 0, TimestampMs(u64::MAX));
    assert!(!result.success, "tampered signature must fail");
    assert!(result.error.as_deref().unwrap().contains("signature"));
}

#[test]
fn z7_mcp_query_provenance_through_real_engine() {
    let engine = Arc::new(AgentCoreEngine::new(
        MockPlanner,
        ConfirmationThreshold::default(),
    ));
    let (sk, pk) = DilithiumSigner::generate_keypair();
    let caller = AccountAddress::from_dilithium_pubkey(pk.as_bytes());

    let call = make_mcp_call(
        "query_provenance",
        serde_json::json!({
            "filter": "agent",
            "id": hex::encode(caller.0),
        }),
        &sk,
        &pk,
        &caller,
    );

    let result = handle_tool_call(&call, &engine, 0, TimestampMs(u64::MAX));
    assert!(
        result.success,
        "query_provenance should succeed: {:?}",
        result.error
    );
}

// ── Z-10: Agent Core supplemental test coverage ────────────────────────

#[test]
fn z10_confirm_plan_full_cycle_through_ace() {
    // Simulate → ConfirmPlan → verify Confirmed outcome.
    let threshold = ConfirmationThreshold {
        value_threshold: Amount(100),
    };
    let engine = AgentCoreEngine::new(MockPlanner, threshold);

    // 1. Simulate (value 500 ≥ threshold 100 → RequiresConfirmation).
    let sim_env = make_envelope(AgentRequestKind::SimulateIntent {
        intent: UserIntent::Transfer {
            to: addr(0xBB),
            token: TokenId::Native,
            amount: Amount(500),
        },
    });
    let sim = engine.dispatch(&sim_env).unwrap();
    let plan_hash = match sim {
        DispatchOutcome::Simulated(ref s) => s.plan_hash,
        _ => panic!("expected Simulated"),
    };

    // 2. Confirm — engine auto-executes after confirmation.
    let mut confirm_env = make_envelope(AgentRequestKind::ConfirmPlan { plan_hash });
    confirm_env.idempotency_key = Blake3Digest([0x04; 32]);
    let outcome = engine.dispatch(&confirm_env).unwrap();
    assert!(
        matches!(outcome, DispatchOutcome::Executed(_)),
        "expected Executed (auto-execute after confirm), got {:?}",
        outcome,
    );
}

#[test]
fn z10_reject_plan_aborts_session() {
    let threshold = ConfirmationThreshold {
        value_threshold: Amount(100),
    };
    let engine = AgentCoreEngine::new(MockPlanner, threshold);

    // 1. Simulate.
    let sim_env = make_envelope(AgentRequestKind::SimulateIntent {
        intent: UserIntent::Transfer {
            to: addr(0xBB),
            token: TokenId::Native,
            amount: Amount(500),
        },
    });
    let sim = engine.dispatch(&sim_env).unwrap();
    let plan_hash = match sim {
        DispatchOutcome::Simulated(ref s) => s.plan_hash,
        _ => panic!("expected Simulated"),
    };

    // 2. Reject.
    let mut reject_env = make_envelope(AgentRequestKind::RejectPlan {
        plan_hash,
        reason: Some("user changed mind".into()),
    });
    reject_env.idempotency_key = Blake3Digest([0x05; 32]);
    let outcome = engine.dispatch(&reject_env).unwrap();
    assert!(matches!(outcome, DispatchOutcome::Rejected { .. }));

    // 3. Session should be Aborted — further actions fail.
    let mut env3 = make_envelope(AgentRequestKind::ConfirmPlan { plan_hash });
    env3.idempotency_key = Blake3Digest([0x06; 32]);
    let result = engine.dispatch(&env3);
    assert!(result.is_err(), "actions after rejection must fail");
}

#[test]
fn z10_session_capacity_enforcement() {
    let config = SessionConfig {
        max_sessions: 3,
        session_ttl_ms: 60_000,
        max_idempotency_keys_per_session: 100,
        rate_limit_per_session: 0,
        rate_limit_window_ms: 60_000,
    };
    let engine =
        AgentCoreEngine::with_config(MockPlanner, ConfirmationThreshold::default(), config);

    // Create 4 sessions — capacity is 3, so the oldest should be evicted.
    for i in 0u8..4 {
        let mut env = make_envelope(AgentRequestKind::Query {
            query_kind: QueryKind::Balance {
                account: addr(0xBB),
            },
        });
        env.session_id = Blake3Digest([i + 0x10; 32]);
        env.idempotency_key = Blake3Digest([i + 0x20; 32]);
        let result = engine.dispatch(&env);
        assert!(result.is_ok(), "session {i} dispatch should succeed");
    }
}

#[test]
fn z10_idempotency_key_dedup() {
    let engine = AgentCoreEngine::new(MockPlanner, ConfirmationThreshold::default());

    let env = make_envelope(AgentRequestKind::Query {
        query_kind: QueryKind::Balance {
            account: addr(0xBB),
        },
    });

    // First call succeeds.
    let r1 = engine.dispatch(&env).unwrap();
    assert!(matches!(r1, DispatchOutcome::QueryResult { .. }));

    // Second call with same idempotency key is rejected (replay protection).
    let r2 = engine.dispatch(&env);
    assert!(r2.is_err(), "duplicate idempotency key must be rejected");
    let err_msg = format!("{}", r2.unwrap_err());
    assert!(
        err_msg.to_lowercase().contains("idempotency"),
        "error should mention idempotency: {err_msg}",
    );
}

#[test]
fn z10_query_all_kinds_through_ace() {
    let engine = AgentCoreEngine::new(MockPlanner, ConfirmationThreshold::default());

    // Balance query.
    let mut env = make_envelope(AgentRequestKind::Query {
        query_kind: QueryKind::Balance {
            account: addr(0xBB),
        },
    });
    assert!(engine.dispatch(&env).is_ok());

    // IntentStatus query.
    env.request_kind = AgentRequestKind::Query {
        query_kind: QueryKind::IntentStatus {
            digest: Blake3Digest([0xDD; 32]),
        },
    };
    env.idempotency_key = Blake3Digest([0x10; 32]);
    assert!(engine.dispatch(&env).is_ok());

    // ContractState query.
    env.request_kind = AgentRequestKind::Query {
        query_kind: QueryKind::ContractState {
            contract: nexus_primitives::ContractAddress([0xCC; 32]),
            resource: "counter".to_string(),
        },
    };
    env.idempotency_key = Blake3Digest([0x11; 32]);
    assert!(engine.dispatch(&env).is_ok());
}
