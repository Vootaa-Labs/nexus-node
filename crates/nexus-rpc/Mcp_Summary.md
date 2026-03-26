# Nexus MCP Summary

## Scope

This summary covers the MCP adapter area inside `nexus-rpc`.

## Module Map

| Module | Current responsibility |
| --- | --- |
| `mcp/mod.rs` | adapter scope and exports |
| `handler.rs` | MCP request handling |
| `registry.rs` | curated tool registry |
| `schema.rs` | schema and payload translation |
| `session_bridge.rs` | session bridging |
| `error_map.rs` | error translation |

## Important Facts

- MCP is a thin adapter and does not own intent, policy, or provenance truth.
- Session and capability semantics ultimately belong to agent core in `nexus-intent`.

## Minimal Read Paths

- Tool exposure: `src/mcp/registry.rs`
- Payload issue: `src/mcp/schema.rs` → `session_bridge.rs`
- Request handling: `src/mcp/handler.rs` → `error_map.rs`
