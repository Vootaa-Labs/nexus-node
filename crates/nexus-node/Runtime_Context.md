# Nexus Node Runtime Context

## Purpose

Use this file for node startup, subsystem assembly, backend adapter wiring, genesis bootstrap, and validator discovery in `nexus-node`.

## First Read Set

1. `Runtime_Summary.md`
2. `src/main.rs`
3. one of `backends.rs`, `genesis_boot.rs`, or `validator_discovery.rs`

## Routing

- `main.rs`: startup order and service construction
- `backends.rs`: node-owned implementations for RPC-facing backend traits
- `genesis_boot.rs`: genesis loading and initialization
- `validator_discovery.rs`: validator peer discovery
- `chain_identity.rs` and `validator_keys.rs`: startup integrity and key material support

## Current Caveats

- Current `main.rs` initializes `RocksStore`; node-local session and provenance stores are also Rocks-backed.
- Deployment and health-check questions should be checked against `Dockerfile`, `docker-compose.yml`, and `scripts/` after the node-local boundary is clear.
