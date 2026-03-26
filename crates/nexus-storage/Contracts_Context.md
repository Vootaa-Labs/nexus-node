# Nexus Storage Contracts Context

## Purpose

Use this file when the task is about storage traits, encoded keys, write batches, column families, or storage-facing contracts that other crates rely on.

## First Read Set

1. `Contracts_Summary.md`
2. `src/lib.rs`
3. `src/traits.rs`
4. `src/types.rs`

## Module Routing

- `traits.rs`: `StateStorage`, `WriteBatchOps`, `StateCommitment`, `BackupHashTree`
- `types.rs`: typed keys, column families, write operations
- `config.rs`: storage configuration and presets
- `error.rs`: storage error model

## Cross-Crate Notes

- `nexus-node`, `nexus-execution`, and `nexus-consensus` all depend on these contracts.
- If the task changes key encoding or write semantics, inspect backend files and one consumer crate before editing.
