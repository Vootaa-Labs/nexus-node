// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! MCP schema ↔ ACE canonical schema translator.
//!
//! Converts MCP JSON invocation payloads into [`AgentEnvelope`] requests
//! and ACE responses back into MCP-compatible JSON results.
//!
//! The translator never interprets business semantics — it only
//! reshapes data between the two representations.

use nexus_intent::agent_core::envelope::{
    AgentEnvelope, AgentExecutionConstraints, AgentPrincipal, AgentRequestKind, ProtocolKind,
    ProvenanceFilter, QueryKind,
};
use nexus_intent::types::UserIntent;
use nexus_primitives::{
    AccountAddress, Amount, Blake3Digest, ContractAddress, TimestampMs, TokenId,
};
use serde::{Deserialize, Serialize};

use crate::error::RpcError;
use crate::mcp::registry::McpToolKind;

// ── MCP protocol version ────────────────────────────────────────────────

/// MCP protocol version string embedded in every translated envelope.
pub const MCP_PROTOCOL_VERSION: &str = "mcp/2025-11-05";

// ── Inbound MCP payloads ────────────────────────────────────────────────

/// Top-level MCP tool invocation received from an LLM client.
#[derive(Debug, Clone, Deserialize)]
pub struct McpToolCall {
    /// MCP tool name (must match a registered [`McpToolKind`]).
    pub tool: String,
    /// JSON arguments (schema varies per tool).
    pub arguments: serde_json::Value,
    /// Caller agent address (hex-encoded 32-byte address).
    pub caller: String,
    /// Hex-encoded ML-DSA-65 public key of the caller (SEC-M12).
    ///
    /// Required for authenticated calls.  The handler verifies the
    /// public-key → address binding and signature before dispatch.
    pub caller_public_key: String,
    /// Hex-encoded ML-DSA-65 signature over `BLAKE3(caller ‖ tool ‖ arguments)`
    /// proving ownership of the claimed caller address (SEC-M12).
    pub caller_signature: String,
    /// Optional MCP session identifier (opaque string from client).
    pub mcp_session_id: Option<String>,
}

/// Arguments for `query_balance`.
#[derive(Debug, Clone, Deserialize)]
pub struct QueryBalanceArgs {
    /// Hex-encoded account address.
    pub account: String,
}

/// Arguments for `query_intent`.
#[derive(Debug, Clone, Deserialize)]
pub struct QueryIntentArgs {
    /// Hex-encoded digest.
    pub digest: String,
}

/// Arguments for `query_contract`.
#[derive(Debug, Clone, Deserialize)]
pub struct QueryContractArgs {
    /// Hex-encoded contract address.
    pub contract: String,
    /// Resource tag or function name.
    pub resource: String,
}

/// Arguments for `simulate_intent`.
#[derive(Debug, Clone, Deserialize)]
pub struct SimulateIntentArgs {
    /// Intent type (e.g. "transfer", "swap").
    pub intent_type: String,
    /// Intent-specific parameters.
    pub params: serde_json::Value,
}

/// Arguments for `execute_plan`.
#[derive(Debug, Clone, Deserialize)]
pub struct ExecutePlanArgs {
    /// Hex-encoded plan hash.
    pub plan_hash: String,
    /// Hex-encoded confirmation reference.
    pub confirmation_ref: String,
}

/// Arguments for `query_provenance`.
#[derive(Debug, Clone, Deserialize)]
pub struct QueryProvenanceArgs {
    /// Filter kind: "agent", "session", "capability", "transaction".
    pub filter: String,
    /// Hex-encoded identifier for the filter.
    pub id: String,
}

// ── Outbound MCP results ────────────────────────────────────────────────

/// MCP tool result sent back to the LLM client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolResult {
    /// Whether the tool call succeeded.
    pub success: bool,
    /// Result payload (tool-specific).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    /// Error message (if `success == false`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl McpToolResult {
    /// Create a success result.
    pub fn ok(data: serde_json::Value) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
        }
    }

    /// Create an error result.
    pub fn err(message: impl Into<String>) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(message.into()),
        }
    }
}

// ── Translation helpers ─────────────────────────────────────────────────

/// Parse a 32-byte hex-encoded address (delegates to shared implementation).
fn parse_address(hex_str: &str) -> Result<AccountAddress, RpcError> {
    crate::rest::parse_address(hex_str)
}

/// Parse a 32-byte hex-encoded digest.
fn parse_digest(hex_str: &str) -> Result<Blake3Digest, RpcError> {
    let bytes = hex::decode(hex_str)
        .map_err(|e| RpcError::BadRequest(format!("invalid hex digest: {e}")))?;
    if bytes.len() != 32 {
        return Err(RpcError::BadRequest(format!(
            "digest must be 32 bytes, got {}",
            bytes.len()
        )));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(Blake3Digest(arr))
}

/// Parse MCP intent_type + params into a [`UserIntent`].
fn parse_user_intent(
    intent_type: &str,
    params: &serde_json::Value,
) -> Result<UserIntent, RpcError> {
    match intent_type {
        "transfer" => {
            let to = params
                .get("to")
                .and_then(|v| v.as_str())
                .ok_or_else(|| RpcError::BadRequest("transfer requires 'to' field".into()))?;
            let to = parse_address(to)?;
            let token = parse_token_id(params.get("token"))?;
            let amount = params
                .get("amount")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| RpcError::BadRequest("transfer requires 'amount' field".into()))?;
            Ok(UserIntent::Transfer {
                to,
                token,
                amount: Amount(amount),
            })
        }
        "swap" => {
            let from_token = parse_token_id(params.get("from_token"))?;
            let to_token = parse_token_id(params.get("to_token"))?;
            let amount = params
                .get("amount")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| RpcError::BadRequest("swap requires 'amount' field".into()))?;
            let raw_slippage = params
                .get("max_slippage_bps")
                .and_then(|v| v.as_u64())
                .unwrap_or(100);
            if raw_slippage > 10_000 {
                return Err(RpcError::BadRequest(format!(
                    "max_slippage_bps must be <= 10000, got {raw_slippage}"
                )));
            }
            let max_slippage_bps = raw_slippage as u16;
            Ok(UserIntent::Swap {
                from_token,
                to_token,
                amount: Amount(amount),
                max_slippage_bps,
            })
        }
        other => Err(RpcError::BadRequest(format!(
            "unsupported intent_type: {other} (supported: transfer, swap)"
        ))),
    }
}

/// Parse an optional JSON token field into a [`TokenId`].
///
/// If absent or `"native"`, returns [`TokenId::Native`].
/// Otherwise interprets the value as a hex-encoded 32-byte contract address.
fn parse_token_id(value: Option<&serde_json::Value>) -> Result<TokenId, RpcError> {
    match value.and_then(|v| v.as_str()) {
        None | Some("native") => Ok(TokenId::Native),
        Some(hex_str) => {
            let bytes = hex::decode(hex_str)
                .map_err(|e| RpcError::BadRequest(format!("invalid hex token: {e}")))?;
            if bytes.len() != 32 {
                return Err(RpcError::BadRequest(format!(
                    "token contract address must be 32 bytes, got {}",
                    bytes.len()
                )));
            }
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&bytes);
            Ok(TokenId::Contract(ContractAddress(arr)))
        }
    }
}

/// Translate MCP tool call arguments into an [`AgentRequestKind`].
///
/// This function performs schema translation only — no business logic.
pub fn translate_request_kind(
    tool: McpToolKind,
    arguments: &serde_json::Value,
) -> Result<AgentRequestKind, RpcError> {
    match tool {
        McpToolKind::QueryBalance => {
            let args: QueryBalanceArgs = serde_json::from_value(arguments.clone())
                .map_err(|e| RpcError::BadRequest(format!("invalid query_balance args: {e}")))?;
            let account = parse_address(&args.account)?;
            Ok(AgentRequestKind::Query {
                query_kind: QueryKind::Balance { account },
            })
        }
        McpToolKind::QueryIntent => {
            let args: QueryIntentArgs = serde_json::from_value(arguments.clone())
                .map_err(|e| RpcError::BadRequest(format!("invalid query_intent args: {e}")))?;
            let digest = parse_digest(&args.digest)?;
            Ok(AgentRequestKind::Query {
                query_kind: QueryKind::IntentStatus { digest },
            })
        }
        McpToolKind::QueryContract => {
            let args: QueryContractArgs = serde_json::from_value(arguments.clone())
                .map_err(|e| RpcError::BadRequest(format!("invalid query_contract args: {e}")))?;
            let bytes = hex::decode(&args.contract)
                .map_err(|e| RpcError::BadRequest(format!("invalid hex contract: {e}")))?;
            if bytes.len() != 32 {
                return Err(RpcError::BadRequest(format!(
                    "contract address must be 32 bytes, got {}",
                    bytes.len()
                )));
            }
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&bytes);
            Ok(AgentRequestKind::Query {
                query_kind: QueryKind::ContractState {
                    contract: ContractAddress(arr),
                    resource: args.resource,
                },
            })
        }
        McpToolKind::SimulateIntent => {
            let args: SimulateIntentArgs = serde_json::from_value(arguments.clone())
                .map_err(|e| RpcError::BadRequest(format!("invalid simulate_intent args: {e}")))?;
            let intent = parse_user_intent(&args.intent_type, &args.params)?;
            Ok(AgentRequestKind::SimulateIntent { intent })
        }
        McpToolKind::ExecutePlan => {
            let args: ExecutePlanArgs = serde_json::from_value(arguments.clone())
                .map_err(|e| RpcError::BadRequest(format!("invalid execute_plan args: {e}")))?;
            let plan_hash = parse_digest(&args.plan_hash)?;
            let confirmation_ref = parse_digest(&args.confirmation_ref)?;
            Ok(AgentRequestKind::ExecutePlan {
                plan_hash,
                confirmation_ref,
            })
        }
        McpToolKind::QueryProvenance => {
            let args: QueryProvenanceArgs = serde_json::from_value(arguments.clone())
                .map_err(|e| RpcError::BadRequest(format!("invalid query_provenance args: {e}")))?;
            let filter = match args.filter.as_str() {
                "agent" => {
                    let agent_id = parse_address(&args.id)?;
                    ProvenanceFilter::ByAgent { agent_id }
                }
                "session" => {
                    let session_id = parse_digest(&args.id)?;
                    ProvenanceFilter::BySession { session_id }
                }
                "capability" => {
                    // Interpret the ID as a hex-encoded contract address for
                    // Contract tokens, or the literal string "native" for the
                    // platform native token.
                    let token_id = if args.id == "native" {
                        TokenId::Native
                    } else {
                        let bytes = hex::decode(&args.id).map_err(|e| {
                            RpcError::BadRequest(format!("invalid hex token id: {e}"))
                        })?;
                        if bytes.len() != 32 {
                            return Err(RpcError::BadRequest(format!(
                                "token contract address must be 32 bytes, got {}",
                                bytes.len()
                            )));
                        }
                        let mut arr = [0u8; 32];
                        arr.copy_from_slice(&bytes);
                        TokenId::Contract(ContractAddress(arr))
                    };
                    ProvenanceFilter::ByCapability { token_id }
                }
                "transaction" => {
                    let tx_hash = parse_digest(&args.id)?;
                    ProvenanceFilter::ByTransaction { tx_hash }
                }
                other => {
                    return Err(RpcError::BadRequest(format!(
                        "unknown provenance filter: {other}"
                    )));
                }
            };
            Ok(AgentRequestKind::QueryProvenance { filter })
        }
    }
}

/// Build a minimal [`AgentEnvelope`] from an MCP tool call.
///
/// The envelope is populated with protocol-level fields; session
/// management is handled by [`super::session_bridge`].
pub fn build_envelope_from_mcp(
    call: &McpToolCall,
    request_kind: AgentRequestKind,
    session_id: Blake3Digest,
    request_id: Blake3Digest,
    idempotency_key: Blake3Digest,
    deadline_ms: TimestampMs,
) -> Result<AgentEnvelope, RpcError> {
    let caller_addr = parse_address(&call.caller)?;
    Ok(AgentEnvelope {
        protocol_kind: ProtocolKind::Mcp,
        protocol_version: MCP_PROTOCOL_VERSION.to_string(),
        request_id,
        session_id,
        idempotency_key,
        caller: AgentPrincipal {
            address: caller_addr,
            display_name: None,
        },
        delegated_capability: None,
        request_kind,
        constraints: AgentExecutionConstraints {
            max_gas: 1_000_000,
            max_total_value: Amount(1_000_000),
            allowed_contracts: vec![],
        },
        deadline_ms,
        parent_session_id: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_address() {
        let hex = "aa".repeat(32);
        let addr = parse_address(&hex).unwrap();
        assert_eq!(addr.0, [0xAA; 32]);
    }

    #[test]
    fn parse_invalid_address_length() {
        let hex = "aa".repeat(16);
        assert!(parse_address(&hex).is_err());
    }

    #[test]
    fn parse_invalid_hex() {
        assert!(parse_address("not_hex_at_all").is_err());
    }

    #[test]
    fn translate_query_balance() {
        let args = serde_json::json!({ "account": "bb".repeat(32) });
        let kind = translate_request_kind(McpToolKind::QueryBalance, &args).unwrap();
        match kind {
            AgentRequestKind::Query {
                query_kind: QueryKind::Balance { account },
            } => {
                assert_eq!(account.0, [0xBB; 32]);
            }
            other => panic!("unexpected kind: {other:?}"),
        }
    }

    #[test]
    fn translate_execute_plan() {
        let args = serde_json::json!({
            "plan_hash": "01".repeat(32),
            "confirmation_ref": "02".repeat(32),
        });
        let kind = translate_request_kind(McpToolKind::ExecutePlan, &args).unwrap();
        match kind {
            AgentRequestKind::ExecutePlan {
                plan_hash,
                confirmation_ref,
            } => {
                assert_eq!(plan_hash.0, [0x01; 32]);
                assert_eq!(confirmation_ref.0, [0x02; 32]);
            }
            other => panic!("unexpected kind: {other:?}"),
        }
    }

    #[test]
    fn translate_query_provenance_by_agent() {
        let args = serde_json::json!({
            "filter": "agent",
            "id": "cc".repeat(32),
        });
        let kind = translate_request_kind(McpToolKind::QueryProvenance, &args).unwrap();
        match kind {
            AgentRequestKind::QueryProvenance {
                filter: ProvenanceFilter::ByAgent { agent_id },
            } => {
                assert_eq!(agent_id.0, [0xCC; 32]);
            }
            other => panic!("unexpected kind: {other:?}"),
        }
    }

    #[test]
    fn translate_unknown_provenance_filter() {
        let args = serde_json::json!({
            "filter": "unknown",
            "id": "aa".repeat(32),
        });
        assert!(translate_request_kind(McpToolKind::QueryProvenance, &args).is_err());
    }

    #[test]
    fn translate_simulate_intent_transfer() {
        let args = serde_json::json!({
            "intent_type": "transfer",
            "params": {
                "to": "bb".repeat(32),
                "token": "native",
                "amount": 1000,
            },
        });
        let kind = translate_request_kind(McpToolKind::SimulateIntent, &args).unwrap();
        match kind {
            AgentRequestKind::SimulateIntent { intent } => {
                assert!(matches!(
                    intent,
                    nexus_intent::types::UserIntent::Transfer { .. }
                ));
            }
            other => panic!("unexpected kind: {other:?}"),
        }
    }

    #[test]
    fn translate_simulate_intent_unsupported_type() {
        let args = serde_json::json!({
            "intent_type": "unknown",
            "params": {},
        });
        assert!(translate_request_kind(McpToolKind::SimulateIntent, &args).is_err());
    }

    #[test]
    fn mcp_tool_result_ok() {
        let r = McpToolResult::ok(serde_json::json!({"balance": 100}));
        assert!(r.success);
        assert!(r.data.is_some());
        assert!(r.error.is_none());
    }

    #[test]
    fn mcp_tool_result_err() {
        let r = McpToolResult::err("something broke");
        assert!(!r.success);
        assert!(r.data.is_none());
        assert_eq!(r.error.as_deref(), Some("something broke"));
    }

    #[test]
    fn mcp_tool_result_serde_round_trip() {
        let r = McpToolResult::ok(serde_json::json!({"key": "value"}));
        let json = serde_json::to_string(&r).unwrap();
        let decoded: McpToolResult = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.success, r.success);
    }

    #[test]
    fn translate_query_provenance_by_capability_native() {
        let args = serde_json::json!({
            "filter": "capability",
            "id": "native",
        });
        let kind = translate_request_kind(McpToolKind::QueryProvenance, &args).unwrap();
        match kind {
            AgentRequestKind::QueryProvenance {
                filter: ProvenanceFilter::ByCapability { token_id },
            } => {
                assert_eq!(token_id, TokenId::Native);
            }
            other => panic!("unexpected kind: {other:?}"),
        }
    }

    #[test]
    fn translate_query_provenance_by_capability_contract() {
        let args = serde_json::json!({
            "filter": "capability",
            "id": "dd".repeat(32),
        });
        let kind = translate_request_kind(McpToolKind::QueryProvenance, &args).unwrap();
        match kind {
            AgentRequestKind::QueryProvenance {
                filter: ProvenanceFilter::ByCapability { token_id },
            } => {
                assert_eq!(token_id, TokenId::Contract(ContractAddress([0xDD; 32])));
            }
            other => panic!("unexpected kind: {other:?}"),
        }
    }

    #[test]
    fn translate_query_provenance_by_capability_invalid() {
        let args = serde_json::json!({
            "filter": "capability",
            "id": "not_valid_hex",
        });
        assert!(translate_request_kind(McpToolKind::QueryProvenance, &args).is_err());
    }

    #[test]
    fn simulate_swap_rejects_slippage_over_10000() {
        let args = serde_json::json!({
            "intent_type": "swap",
            "params": {
                "from_token": "native",
                "to_token": "native",
                "amount": 1000,
                "max_slippage_bps": 10001
            }
        });
        let err = translate_request_kind(McpToolKind::SimulateIntent, &args);
        assert!(err.is_err());
    }
}
