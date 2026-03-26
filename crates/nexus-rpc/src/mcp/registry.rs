// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! MCP tool registry — the compatibility matrix.
//!
//! Defines which MCP tools are exposed, which ACE request kind each
//! tool maps to, and which tools are explicitly forbidden.
//!
//! # Compatibility Matrix (TLD-07 §5.2)
//!
//! | MCP Tool            | ACE RequestKind        | Mutating | Confirmation |
//! |---------------------|------------------------|----------|--------------|
//! | `query_balance`     | `Query::Balance`       | No       | Never        |
//! | `query_intent`      | `Query::IntentStatus`  | No       | Never        |
//! | `query_contract`    | `Query::ContractState`  | No       | Never        |
//! | `simulate_intent`   | `SimulateIntent`       | No       | Never        |
//! | `execute_plan`      | `ExecutePlan`          | **Yes**  | Conditional  |
//! | `query_provenance`  | `QueryProvenance`      | No       | Never        |
//!
//! ## Forbidden tools (MUST NOT be exposed)
//!
//! - `raw_move_payload`: direct Move call-data passthrough
//! - `direct_broadcast`: execution bypassing plan binding
//! - `admin_override`: capability-bypass backdoor

use serde::{Deserialize, Serialize};

// ── McpToolKind ─────────────────────────────────────────────────────────

/// Enumerated MCP tools that may be registered with the MCP server.
///
/// Each variant maps 1:1 to a specific [`AgentRequestKind`]
/// variant through the schema translator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum McpToolKind {
    /// Query account balance.
    QueryBalance,
    /// Query intent / transaction status.
    QueryIntent,
    /// Query contract state.
    QueryContract,
    /// Simulate an intent (dry-run).
    SimulateIntent,
    /// Execute a confirmed plan.
    ExecutePlan,
    /// Query provenance / audit trail.
    QueryProvenance,
}

impl McpToolKind {
    /// MCP tool name as exposed to LLM clients.
    pub fn tool_name(self) -> &'static str {
        match self {
            Self::QueryBalance => "query_balance",
            Self::QueryIntent => "query_intent",
            Self::QueryContract => "query_contract",
            Self::SimulateIntent => "simulate_intent",
            Self::ExecutePlan => "execute_plan",
            Self::QueryProvenance => "query_provenance",
        }
    }

    /// Human-readable description for the MCP tool listing.
    pub fn description(self) -> &'static str {
        match self {
            Self::QueryBalance => "Query the balance of an account for a given token.",
            Self::QueryIntent => "Query the status of an intent or transaction by digest.",
            Self::QueryContract => "Query contract state by address and resource tag.",
            Self::SimulateIntent => {
                "Simulate an intent without executing it. Returns a plan_hash for later execution."
            }
            Self::ExecutePlan => {
                "Execute a previously simulated and confirmed plan, bound by plan_hash."
            }
            Self::QueryProvenance => {
                "Query the provenance audit trail by agent, session, capability, or transaction."
            }
        }
    }

    /// Whether this tool causes state mutation.
    pub fn is_mutating(self) -> bool {
        matches!(self, Self::ExecutePlan)
    }

    /// Whether this tool may trigger a human confirmation gate.
    pub fn may_require_confirmation(self) -> bool {
        matches!(self, Self::ExecutePlan)
    }

    /// Enumerate all registered tools.
    pub fn all() -> &'static [McpToolKind] {
        &[
            Self::QueryBalance,
            Self::QueryIntent,
            Self::QueryContract,
            Self::SimulateIntent,
            Self::ExecutePlan,
            Self::QueryProvenance,
        ]
    }
}

// ── ForbiddenToolKind ───────────────────────────────────────────────────

/// Tools that are explicitly forbidden from MCP exposure.
///
/// Attempts to register these should produce a compile-time or
/// initialisation error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForbiddenToolKind {
    /// Direct Move call-data passthrough — bypasses intent compilation.
    RawMovePayload,
    /// Direct broadcast without plan binding — bypasses confirmation.
    DirectBroadcast,
    /// Admin override — bypasses capability tokens entirely.
    AdminOverride,
}

impl ForbiddenToolKind {
    /// Reason this tool category is forbidden.
    pub fn denial_reason(self) -> &'static str {
        match self {
            Self::RawMovePayload => {
                "raw Move payload passthrough bypasses intent compilation and plan binding"
            }
            Self::DirectBroadcast => {
                "direct broadcast bypasses simulation → confirmation → execution pipeline"
            }
            Self::AdminOverride => "admin override bypasses capability token validation",
        }
    }
}

// ── ToolEntry (registry record) ─────────────────────────────────────────

/// Registry entry for a single MCP tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolEntry {
    /// Tool kind (determines schema and dispatch).
    pub kind: McpToolKind,
    /// Whether the tool is currently enabled.
    pub enabled: bool,
}

/// Validate that a tool name is in the allowed set.
///
/// Returns `Some(kind)` if the name corresponds to a registered tool,
/// `None` if the name is unknown or forbidden.
pub fn lookup_tool(name: &str) -> Option<McpToolKind> {
    McpToolKind::all()
        .iter()
        .find(|t| t.tool_name() == name)
        .copied()
}

/// Check if a tool name matches a forbidden pattern.
pub fn is_forbidden_tool(name: &str) -> Option<ForbiddenToolKind> {
    match name {
        "raw_move_payload" | "raw_payload" => Some(ForbiddenToolKind::RawMovePayload),
        "direct_broadcast" | "broadcast_raw" => Some(ForbiddenToolKind::DirectBroadcast),
        "admin_override" | "sudo" => Some(ForbiddenToolKind::AdminOverride),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_tools_have_unique_names() {
        let tools = McpToolKind::all();
        let mut names: Vec<_> = tools.iter().map(|t| t.tool_name()).collect();
        let original_len = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), original_len, "duplicate tool names detected");
    }

    #[test]
    fn only_execute_is_mutating() {
        for tool in McpToolKind::all() {
            if *tool == McpToolKind::ExecutePlan {
                assert!(tool.is_mutating());
            } else {
                assert!(!tool.is_mutating(), "{:?} should not be mutating", tool);
            }
        }
    }

    #[test]
    fn lookup_known_tool() {
        assert_eq!(
            lookup_tool("query_balance"),
            Some(McpToolKind::QueryBalance)
        );
        assert_eq!(lookup_tool("execute_plan"), Some(McpToolKind::ExecutePlan));
        assert_eq!(
            lookup_tool("query_provenance"),
            Some(McpToolKind::QueryProvenance)
        );
    }

    #[test]
    fn lookup_unknown_tool() {
        assert!(lookup_tool("not_a_tool").is_none());
        assert!(lookup_tool("").is_none());
    }

    #[test]
    fn forbidden_tool_detected() {
        assert_eq!(
            is_forbidden_tool("raw_move_payload"),
            Some(ForbiddenToolKind::RawMovePayload)
        );
        assert_eq!(
            is_forbidden_tool("direct_broadcast"),
            Some(ForbiddenToolKind::DirectBroadcast)
        );
        assert_eq!(
            is_forbidden_tool("admin_override"),
            Some(ForbiddenToolKind::AdminOverride)
        );
    }

    #[test]
    fn non_forbidden_tool_passes() {
        assert!(is_forbidden_tool("query_balance").is_none());
        assert!(is_forbidden_tool("simulate_intent").is_none());
    }

    #[test]
    fn tool_descriptions_non_empty() {
        for tool in McpToolKind::all() {
            assert!(
                !tool.description().is_empty(),
                "{:?} has empty description",
                tool
            );
        }
    }

    #[test]
    fn tool_entry_serde_round_trip() {
        let entry = ToolEntry {
            kind: McpToolKind::SimulateIntent,
            enabled: true,
        };
        let json = serde_json::to_string(&entry).unwrap();
        let decoded: ToolEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.kind, entry.kind);
        assert_eq!(decoded.enabled, entry.enabled);
    }

    // ── Z-8: Security audit — forbidden tools never in registry listing ──

    #[test]
    fn sec_forbidden_tools_not_discoverable_in_registry() {
        let forbidden_names = [
            "raw_move_payload",
            "raw_payload",
            "direct_broadcast",
            "broadcast_raw",
            "admin_override",
            "sudo",
        ];
        let all_tools: Vec<&str> = McpToolKind::all().iter().map(|t| t.tool_name()).collect();
        for name in &forbidden_names {
            assert!(
                !all_tools.contains(name),
                "forbidden tool '{}' must not appear in McpToolKind::all()",
                name
            );
            assert!(
                lookup_tool(name).is_none(),
                "forbidden tool '{}' must not be found via lookup_tool()",
                name
            );
        }
    }
}
