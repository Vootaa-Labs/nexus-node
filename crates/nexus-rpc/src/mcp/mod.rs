//! MCP Adapter — Model Context Protocol bridge for Nexus.
//!
//! **Position**: Adapter-MCP-v1 (thin translation layer per TLD-07 §5).
//!
//! # Responsibilities
//!
//! - Expose a curated list of MCP tools (see [`registry`]).
//! - Translate MCP JSON-Schema ↔ ACE canonical types ([`schema`]).
//! - Bridge MCP sessions to [`AgentSession`](nexus_intent::agent_core::session::AgentSession) ([`session_bridge`]).
//! - Map internal errors to MCP tool-error codes ([`error_map`]).
//!
//! # Non-responsibilities
//!
//! The MCP adapter **MUST NOT**:
//!
//! - Define an independent permission model.
//! - Define plan / session / provenance semantics.
//! - Execute on-chain actions directly (must go through ACE).
//! - Hold authoritative audit state.
//!
//! All executable requests flow through the Agent Core Engine (ACE)
//! before reaching the intent compiler or execution backends.

pub mod error_map;
pub mod handler;
pub mod registry;
pub mod schema;
pub mod session_bridge;
