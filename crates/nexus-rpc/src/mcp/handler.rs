//! MCP tool-call handler — orchestrates the full MCP→ACE dispatch cycle.
//!
//! Receives a raw [`McpToolCall`], validates it against the registry,
//! translates it into an [`AgentEnvelope`], dispatches through the
//! [`DispatchBackend`], and returns an [`McpToolResult`].

use std::sync::Arc;

use nexus_crypto::{DilithiumSignature, DilithiumSigner, DilithiumVerifyKey, Signer};
use nexus_intent::agent_core::dispatcher::{DispatchBackend, DispatchOutcome};
use nexus_primitives::{AccountAddress, Blake3Digest, TimestampMs};

use crate::mcp::error_map::{map_intent_error, McpErrorCode};
use crate::mcp::registry::{is_forbidden_tool, lookup_tool};
use crate::mcp::schema::{
    build_envelope_from_mcp, translate_request_kind, McpToolCall, McpToolResult,
};
use crate::mcp::session_bridge::{derive_idempotency_key, derive_request_id, derive_session_id};

/// Domain tag for MCP caller signature verification (SEC-M12).
const MCP_CALLER_SIG_DOMAIN: &[u8] = b"nexus::mcp::caller_auth::v1";

/// Handle an incoming MCP tool call end-to-end.
///
/// Flow: caller auth → forbidden check → registry lookup → translate
/// → session bridge → envelope → dispatch → outcome → MCP result.
pub fn handle_tool_call<D: DispatchBackend + ?Sized>(
    call: &McpToolCall,
    dispatcher: &Arc<D>,
    call_index: u64,
    deadline_ms: TimestampMs,
) -> McpToolResult {
    // 0. Caller identity verification (SEC-M12).
    if let Err(msg) = verify_caller_identity(call) {
        return McpToolResult::err(msg);
    }

    // 1. Forbidden tool check.
    if let Some(kind) = is_forbidden_tool(&call.tool) {
        return McpToolResult::err(format!("tool '{}' is forbidden: {:?}", call.tool, kind));
    }

    // 2. Registry lookup.
    let tool_kind = match lookup_tool(&call.tool) {
        Some(k) => k,
        None => {
            return McpToolResult::err(format!(
                "unknown tool '{}' — code {}",
                call.tool,
                McpErrorCode::ToolNotFound.code()
            ));
        }
    };

    // 3. Translate arguments → AgentRequestKind.
    let request_kind = match translate_request_kind(tool_kind, &call.arguments) {
        Ok(rk) => rk,
        Err(e) => return McpToolResult::err(format!("{e}")),
    };

    // 4. Session keying.
    let mcp_session = call.mcp_session_id.as_deref().unwrap_or("ephemeral");
    let session_id = derive_session_id(mcp_session);
    let request_id = derive_request_id(&session_id, call_index);
    let args_hash = compute_args_hash(&call.arguments);
    let idempotency_key = derive_idempotency_key(&session_id, &call.tool, &args_hash);

    // 5. Build envelope.
    let envelope = match build_envelope_from_mcp(
        call,
        request_kind,
        session_id,
        request_id,
        idempotency_key,
        deadline_ms,
    ) {
        Ok(env) => env,
        Err(e) => return McpToolResult::err(format!("{e}")),
    };

    // 6. Dispatch.
    match dispatcher.dispatch(&envelope) {
        Ok(outcome) => outcome_to_result(outcome),
        Err(e) => {
            let code = map_intent_error(&e);
            // Expose only the error category, not internal variant details.
            let public_msg = match code {
                McpErrorCode::InvalidParams => "invalid request parameters",
                McpErrorCode::ToolNotFound => "tool not found",
                McpErrorCode::CapabilityDenied => "capability denied",
                McpErrorCode::Expired => "request expired",
                McpErrorCode::PlanBindingError => "plan binding error",
                McpErrorCode::ValueLimitExceeded => "value limit exceeded",
                McpErrorCode::Unavailable => "service unavailable",
                McpErrorCode::InternalError => "internal error",
            };
            McpToolResult::err(format!("{public_msg} (mcp_code={})", code.code()))
        }
    }
}

/// Convert a [`DispatchOutcome`] into an [`McpToolResult`].
fn outcome_to_result(outcome: DispatchOutcome) -> McpToolResult {
    match outcome {
        DispatchOutcome::Simulated(sim) => McpToolResult::ok(serde_json::json!({
            "session_id": hex::encode(sim.session_id.0),
            "plan_hash": hex::encode(sim.plan_hash.0),
            "estimated_gas": sim.estimated_gas,
            "step_count": sim.step_count,
            "requires_cross_shard": sim.requires_cross_shard,
        })),
        DispatchOutcome::Executed(receipt) => McpToolResult::ok(serde_json::json!({
            "session_id": hex::encode(receipt.session_id.0),
            "plan_hash": hex::encode(receipt.plan_hash.0),
            "tx_hashes": receipt.tx_hashes.iter().map(|h| hex::encode(h.0)).collect::<Vec<_>>(),
            "gas_used": receipt.gas_used,
        })),
        DispatchOutcome::QueryResult { payload } => McpToolResult::ok(serde_json::json!({
            "payload": hex::encode(payload),
        })),
        DispatchOutcome::Confirmed(conf) => McpToolResult::ok(serde_json::json!({
            "session_id": hex::encode(conf.session_id.0),
            "plan_hash": hex::encode(conf.plan_hash.0),
            "confirmation_ref": hex::encode(conf.confirmation_ref.0),
        })),
        DispatchOutcome::Rejected { reason } => McpToolResult::err(reason),
    }
}

/// Compute a BLAKE3 hash of the JSON arguments for idempotency keying.
fn compute_args_hash(args: &serde_json::Value) -> Blake3Digest {
    let bytes = serde_json::to_vec(args).unwrap_or_default();
    let hash = blake3::hash(&bytes);
    Blake3Digest(*hash.as_bytes())
}

/// Verify the MCP caller's identity: public-key → address binding and
/// signature over the call payload (SEC-M12).
fn verify_caller_identity(call: &McpToolCall) -> Result<(), String> {
    // 1. Decode public key.
    let pk_bytes = hex::decode(&call.caller_public_key)
        .map_err(|e| format!("invalid caller_public_key hex: {e}"))?;
    let vk = DilithiumVerifyKey::from_bytes(&pk_bytes)
        .map_err(|e| format!("invalid caller_public_key: {e}"))?;

    // 2. Derive address from public key and verify it matches the claimed caller.
    let derived = AccountAddress::from_dilithium_pubkey(vk.as_bytes());
    let claimed = hex::decode(&call.caller).map_err(|e| format!("invalid caller hex: {e}"))?;
    if claimed.len() != 32 {
        return Err(format!(
            "caller address must be 32 bytes, got {}",
            claimed.len()
        ));
    }
    if derived.0[..] != claimed[..] {
        return Err("caller address does not match public key".into());
    }

    // 3. Decode signature.
    let sig_bytes = hex::decode(&call.caller_signature)
        .map_err(|e| format!("invalid caller_signature hex: {e}"))?;
    let sig = DilithiumSignature::from_bytes(&sig_bytes)
        .map_err(|e| format!("invalid caller_signature: {e}"))?;

    // 4. Build signed payload: BLAKE3(caller ‖ tool ‖ arguments_json).
    let args_bytes = serde_json::to_vec(&call.arguments).unwrap_or_default();
    let mut payload = Vec::new();
    payload.extend_from_slice(&claimed);
    payload.extend_from_slice(call.tool.as_bytes());
    payload.extend_from_slice(&args_bytes);

    // 5. Verify.
    DilithiumSigner::verify(&vk, MCP_CALLER_SIG_DOMAIN, &payload, &sig)
        .map_err(|_| "caller signature verification failed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_intent::agent_core::dispatcher::DispatchOutcome;
    use nexus_intent::agent_core::envelope::AgentEnvelope;
    use nexus_intent::agent_core::planner::{ExecutionReceipt, SimulationResult};
    use nexus_intent::error::{IntentError, IntentResult};
    use nexus_primitives::AccountAddress;

    /// Mock dispatcher that always returns a simulated outcome.
    struct MockDispatcher;

    impl DispatchBackend for MockDispatcher {
        fn dispatch(&self, _envelope: &AgentEnvelope) -> IntentResult<DispatchOutcome> {
            Ok(DispatchOutcome::Simulated(SimulationResult {
                session_id: Blake3Digest([0x02; 32]),
                plan_hash: Blake3Digest([0xAA; 32]),
                estimated_gas: 10_000,
                step_count: 1,
                requires_cross_shard: false,
                simulated_at_ms: TimestampMs(1_000),
                summary: String::new(),
            }))
        }
    }

    /// Mock dispatcher that always returns an error.
    struct ErrorDispatcher;

    impl DispatchBackend for ErrorDispatcher {
        fn dispatch(&self, _envelope: &AgentEnvelope) -> IntentResult<DispatchOutcome> {
            Err(IntentError::AccountNotFound {
                account: AccountAddress([0xFF; 32]),
            })
        }
    }

    fn make_call(tool: &str, args: serde_json::Value) -> McpToolCall {
        // Generate a real keypair so caller identity checks pass.
        let (sk, vk) = DilithiumSigner::generate_keypair();
        let caller_addr = AccountAddress::from_dilithium_pubkey(vk.as_bytes());

        // Build the signed payload: caller_addr ‖ tool ‖ args_json
        let args_bytes = serde_json::to_vec(&args).unwrap_or_default();
        let mut payload = Vec::new();
        payload.extend_from_slice(&caller_addr.0);
        payload.extend_from_slice(tool.as_bytes());
        payload.extend_from_slice(&args_bytes);

        let sig = DilithiumSigner::sign(&sk, MCP_CALLER_SIG_DOMAIN, &payload);

        McpToolCall {
            tool: tool.to_string(),
            arguments: args,
            caller: hex::encode(caller_addr.0),
            caller_public_key: hex::encode(vk.as_bytes()),
            caller_signature: hex::encode(sig.as_bytes()),
            mcp_session_id: Some("test-session-1".to_string()),
        }
    }

    #[test]
    fn handle_query_balance_success() {
        let dispatcher = Arc::new(MockDispatcher);
        let call = make_call(
            "query_balance",
            serde_json::json!({ "account": "bb".repeat(32) }),
        );
        let result = handle_tool_call(&call, &dispatcher, 0, TimestampMs(u64::MAX));
        assert!(result.success);
    }

    #[test]
    fn handle_unknown_tool() {
        let dispatcher = Arc::new(MockDispatcher);
        let call = make_call("nonexistent_tool", serde_json::json!({}));
        let result = handle_tool_call(&call, &dispatcher, 0, TimestampMs(u64::MAX));
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("unknown tool"));
    }

    #[test]
    fn handle_forbidden_tool() {
        let dispatcher = Arc::new(MockDispatcher);
        let call = make_call("raw_move_payload", serde_json::json!({}));
        let result = handle_tool_call(&call, &dispatcher, 0, TimestampMs(u64::MAX));
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("forbidden"));
    }

    #[test]
    fn handle_dispatch_error_includes_mcp_code() {
        let dispatcher = Arc::new(ErrorDispatcher);
        let call = make_call(
            "query_balance",
            serde_json::json!({ "account": "cc".repeat(32) }),
        );
        let result = handle_tool_call(&call, &dispatcher, 0, TimestampMs(u64::MAX));
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("mcp_code="));
    }

    #[test]
    fn handle_simulate_intent_transfer() {
        let dispatcher = Arc::new(MockDispatcher);
        let call = make_call(
            "simulate_intent",
            serde_json::json!({
                "intent_type": "transfer",
                "params": {
                    "to": "bb".repeat(32),
                    "token": "native",
                    "amount": 5000,
                },
            }),
        );
        let result = handle_tool_call(&call, &dispatcher, 0, TimestampMs(u64::MAX));
        assert!(result.success);
        let data = result.data.unwrap();
        assert_eq!(data["estimated_gas"], 10_000);
    }

    #[test]
    fn outcome_simulated_to_result() {
        let sim = DispatchOutcome::Simulated(SimulationResult {
            session_id: Blake3Digest([0x01; 32]),
            plan_hash: Blake3Digest([0x02; 32]),
            estimated_gas: 5_000,
            step_count: 2,
            requires_cross_shard: true,
            simulated_at_ms: TimestampMs(100),
            summary: String::new(),
        });
        let r = outcome_to_result(sim);
        assert!(r.success);
        let data = r.data.unwrap();
        assert_eq!(data["step_count"], 2);
        assert_eq!(data["requires_cross_shard"], true);
    }

    #[test]
    fn outcome_executed_to_result() {
        let receipt = DispatchOutcome::Executed(ExecutionReceipt {
            session_id: Blake3Digest([0x01; 32]),
            plan_hash: Blake3Digest([0x02; 32]),
            tx_hashes: vec![Blake3Digest([0xEE; 32])],
            gas_used: 42_000,
            completed_at_ms: TimestampMs(200),
        });
        let r = outcome_to_result(receipt);
        assert!(r.success);
        let data = r.data.unwrap();
        assert_eq!(data["gas_used"], 42_000);
    }

    #[test]
    fn outcome_rejected_to_result() {
        let r = outcome_to_result(DispatchOutcome::Rejected {
            reason: "bad plan".into(),
        });
        assert!(!r.success);
        assert_eq!(r.error.as_deref(), Some("bad plan"));
    }

    #[test]
    fn idempotent_calls_produce_same_key() {
        let call = make_call(
            "query_balance",
            serde_json::json!({ "account": "bb".repeat(32) }),
        );
        let session_id = derive_session_id("test-session-1");
        let args_hash = compute_args_hash(&call.arguments);
        let key1 = derive_idempotency_key(&session_id, &call.tool, &args_hash);
        let key2 = derive_idempotency_key(&session_id, &call.tool, &args_hash);
        assert_eq!(key1, key2);
    }

    #[test]
    fn mcp_caller_identity_should_be_authenticated() {
        // A call with a valid signature on a known tool should succeed.
        let dispatcher = Arc::new(MockDispatcher);
        let call = make_call(
            "query_balance",
            serde_json::json!({ "account": "bb".repeat(32) }),
        );
        let result = handle_tool_call(&call, &dispatcher, 0, TimestampMs(u64::MAX));
        assert!(result.success, "valid signature should succeed");

        // Tamper with the signature — the call must be rejected before dispatch.
        let mut bad_call = call.clone();
        // Flip a byte in the signature hex.
        let mut sig_bytes = hex::decode(&bad_call.caller_signature).unwrap();
        sig_bytes[0] ^= 0xFF;
        bad_call.caller_signature = hex::encode(&sig_bytes);
        let result = handle_tool_call(&bad_call, &dispatcher, 0, TimestampMs(u64::MAX));
        assert!(!result.success, "tampered signature must fail");
        assert!(
            result.error.as_deref().unwrap().contains("signature"),
            "error should mention signature"
        );

        // Use a different caller address with the same signature —
        // the address↔public-key binding must fail.
        let mut wrong_addr = call.clone();
        wrong_addr.caller = "ff".repeat(32);
        let result = handle_tool_call(&wrong_addr, &dispatcher, 0, TimestampMs(u64::MAX));
        assert!(!result.success, "wrong caller address must fail");
        assert!(
            result.error.as_deref().unwrap().contains("does not match"),
            "error should mention address mismatch"
        );
    }
}
