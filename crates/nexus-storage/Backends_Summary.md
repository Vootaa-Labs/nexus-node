# Nexus Storage Backends Summary

## Scope

This summary covers concrete storage backend implementations in `nexus-storage`.

## Module Map

| Module | Current responsibility |
| --- | --- |
| `memory.rs` | in-memory backend for tests and lightweight runtime usage |
| `rocks/mod.rs` | RocksDB-backed store |
| `rocks/schema.rs` | backend schema and key structure |
| `rocks/batch.rs` | Rocks-specific batch support |

## Important Facts

- `MemoryStore` and `RocksStore` share the same contract layer.
- RocksDB code is isolated under `src/rocks/`.
- Snapshot and batch behavior should be validated against trait-level expectations.

## Minimal Read Paths

- In-memory behavior: `src/lib.rs` → `memory.rs`
- RocksDB behavior: `src/lib.rs` → `rocks/mod.rs` → `rocks/schema.rs`
