# Nexus Block STM Context

## Purpose

Use this file for optimistic parallel execution, conflict retries, adaptive worker behavior, and MVCC overlay semantics in `nexus-execution`.

## First Read Set

1. `Block_Stm_Summary.md`
2. `src/block_stm/mod.rs`
3. one of `executor.rs`, `mvhashmap.rs`, or `adaptive.rs`

## Routing

- `mod.rs`: batch-level control flow and retry loop
- `executor.rs`: single-transaction execution path
- `mvhashmap.rs`: version visibility and validation logic
- `adaptive.rs`: worker-count tuning

## Caveats

- Only switch to Move adapter files if the issue is clearly VM semantics rather than scheduling or isolation.
- Determinism, validation, and retry rules are correctness-sensitive.
