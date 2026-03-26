# Nexus MCP Context

## Purpose

Use this file for the MCP adapter in `nexus-rpc`: tool exposure, schema translation, session bridging, and MCP-facing error behavior.

## First Read Set

1. `Mcp_Summary.md`
2. `src/mcp/mod.rs`
3. one target adapter file

## Routing

- `registry.rs`: exposed tool list and registration
- `schema.rs`: MCP payload and schema translation
- `session_bridge.rs`: mapping between MCP sessions and internal session state
- `handler.rs`: request handling entry point
- `error_map.rs`: outward-facing error mapping

## Boundary Note

- If the issue is session truth, permissions, or provenance semantics, pair this with `crates/nexus-intent/Agent_Core_Context.md`.
