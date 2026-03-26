# Nexus Network Code Context

## Purpose

Use this file for libp2p transport, peer discovery, gossip, rate limiting, and network service orchestration.

## First Read Set

1. `Code_Summary.md`
2. `src/lib.rs`
3. one target subsystem under `transport/`, `discovery/`, `gossip/`, or `service.rs`

## Module Routing

- `transport/`: transport manager, QUIC path, connection pool
- `discovery/`: discovery service and disjoint peer logic
- `gossip/`: gossip service, deduplication, peer scoring
- `service.rs`: top-level service builder and handles
- `rate_limit.rs`: per-peer message limiting
- `codec.rs` and `types.rs`: wire-facing types and encoding

## Cross-Crate Notes

- `nexus-node` owns runtime integration and bridge logic.
- `NetworkConfig` is re-exported through `nexus-config`.
- Workspace `Cargo.toml` enables QUIC, Yamux, Kad, gossipsub, identify, request-response, and TCP features for libp2p.
