# Nexus Agent Core Summary

## Scope

This summary covers the agent core subsystem inside `nexus-intent`.

## Module Map

| Module | Current responsibility |
| --- | --- |
| `agent_core/mod.rs` | architecture glue and exports |
| `envelope.rs` | canonical envelope and principal model |
| `session.rs` | session lifecycle |
| `capability_snapshot.rs`, `policy.rs` | capability scope and policy decisions |
| `planner.rs`, `dispatcher.rs`, `intent_planner_bridge.rs` | planning and execution routing |
| `a2a.rs`, `a2a_negotiator.rs` | agent-to-agent negotiation |
| `provenance.rs`, `provenance_store.rs` | provenance data and persistence |

## Important Facts

- Agent core is separate from compiler and resolver concerns.
- MCP integration should be read as an adapter into this subsystem, not as a replacement for it.

## Minimal Read Paths

- Session and envelope: `src/agent_core/envelope.rs` → `session.rs`
- Planning path: `src/agent_core/planner.rs` → `dispatcher.rs`
- A2A path: `src/agent_core/a2a.rs` → `a2a_negotiator.rs`
