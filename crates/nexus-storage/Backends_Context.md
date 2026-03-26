# Nexus Storage Backends Context

## Purpose

Use this file for `MemoryStore`, RocksDB behavior, snapshots, schema layout, and backend-specific performance or correctness questions.

## First Read Set

1. `Backends_Summary.md`
2. `src/lib.rs`
3. one of `memory.rs` or `rocks/mod.rs`
4. `rocks/schema.rs` if the task touches RocksDB structure

## Module Routing

- `memory.rs`: in-memory implementation used heavily in tests and default node assembly path today
- `rocks/mod.rs`: production-oriented RocksDB backend
- `rocks/schema.rs`: column-family and key layout for RocksDB
- `rocks/batch.rs`: batch helpers for RocksDB path

## Caveats

- Do not assume the runtime currently uses RocksDB by default; inspect `nexus-node/src/main.rs` for actual assembly behavior.
- If changing backend behavior, also inspect `tests/nexus-test-utils` and any node/runtime code that constructs stores.
