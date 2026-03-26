# Nexus Node Runtime Summary

## Scope

This summary covers runtime assembly responsibilities in `nexus-node`.

## Module Map

| Module | Current responsibility |
| --- | --- |
| `main.rs` | config load, tracing, subsystem construction, spawn order |
| `lib.rs` | node-local module exports |
| `backends.rs` | RPC-facing backend adapters |
| `genesis_boot.rs` | genesis load and initial state seeding |
| `validator_discovery.rs` | validator peer discovery |
| `chain_identity.rs`, `validator_keys.rs` | chain identity and validator key support |

## Important Facts

- `nexus-node` is both a library and the `nexus-node` binary crate.
- Runtime assembly is intentionally thin, but `main.rs` still reflects current operational choices such as RocksDB-backed state, session, and provenance storage.

## Minimal Read Paths

- Startup path: `src/main.rs`
- RPC adapter path: `src/backends.rs`
- Bootstrap path: `src/genesis_boot.rs` → `validator_discovery.rs`
