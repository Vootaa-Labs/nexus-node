# Nexus REST Summary

## Scope

This summary covers the REST area inside `nexus-rpc`.

## Module Map

| Module | Current responsibility |
| --- | --- |
| `rest/mod.rs` | router assembly, `AppState`, backend traits |
| `health.rs`, `readiness.rs` | health and readiness endpoints |
| `account.rs`, `contract.rs` | state and contract queries |
| `transaction.rs` | transaction submit and lookup |
| `intent.rs` | intent submit or simulation-style endpoints |
| `network.rs`, `consensus.rs` | network and chain status |
| `faucet.rs` | faucet endpoints |
| `prometheus.rs` | metrics endpoint |

## Important Facts

- REST is an adapter layer over backend traits implemented in `nexus-node`.
- WebSocket and MCP are sibling adapters, not extensions of REST.

## Minimal Read Paths

- Router question: `src/rest/mod.rs`
- Endpoint question: `src/rest/mod.rs` → target endpoint file
- Backend handoff: target endpoint file → `crates/nexus-node/src/backends.rs`
