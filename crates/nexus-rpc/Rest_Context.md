# Nexus REST Context

## Purpose

Use this file for HTTP endpoint behavior, REST router assembly, backend trait wiring, and request-to-backend translation inside `nexus-rpc`.

## First Read Set

1. `Rest_Summary.md`
2. `src/rest/mod.rs`
3. one target endpoint file

## Routing

- `mod.rs`: `AppState`, backend traits, router assembly
- `health.rs` and `readiness.rs`: liveness and readiness endpoints
- `transaction.rs`: transaction submission and lookup
- `intent.rs`: intent-facing HTTP path
- `account.rs`, `contract.rs`: state and contract reads
- `network.rs`, `consensus.rs`: network and chain status endpoints
- `faucet.rs`, `prometheus.rs`: faucet and metrics exposure

## Boundary Note

- If the issue is event streaming, switch to `ws.rs` after reading the REST boundary.
- If the issue is tool exposure for AI agents, switch to `Mcp_Context.md`.
