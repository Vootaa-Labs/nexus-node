# Nexus Agent Core Context

## Purpose

Use this file for the agent-facing core inside `nexus-intent`: envelopes, sessions, capability scope, planning, A2A, and provenance.

## First Read Set

1. `Agent_Core_Summary.md`
2. `src/agent_core/mod.rs`
3. one target subsystem file

## Routing

- `envelope.rs`: canonical request and principal model
- `session.rs`: lifecycle and replay-related state
- `capability_snapshot.rs` and `policy.rs`: scope and policy controls
- `planner.rs` and `dispatcher.rs`: simulate-plan-confirm-execute path
- `a2a.rs` and `a2a_negotiator.rs`: agent-to-agent negotiation
- `provenance.rs` and `provenance_store.rs`: provenance records and storage

## Boundary Note

- If the task is external protocol adaptation, pair this with `crates/nexus-rpc/Mcp_Context.md` instead of opening unrelated crates first.
