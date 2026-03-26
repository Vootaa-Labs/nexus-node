# Nexus Block STM Summary

## Scope

This summary covers the Block-STM parallel execution area inside `nexus-execution`.

## Module Map

| Module | Current responsibility |
| --- | --- |
| `block_stm/mod.rs` | batch-level pipeline and retry control |
| `block_stm/executor.rs` | per-transaction execution recording |
| `block_stm/mvhashmap.rs` | MVCC overlay and read validation |
| `block_stm/adaptive.rs` | adaptive worker policy |

## Important Facts

- Block-STM is separate from the Move adapter boundary.
- Correctness questions usually localize to `mvhashmap.rs` or `mod.rs`.
- Execution metrics are exposed elsewhere in the crate, not in the docs here.

## Minimal Read Paths

- Full pipeline: `src/block_stm/mod.rs`
- Isolation question: `src/block_stm/mvhashmap.rs`
- Throughput tuning: `src/block_stm/adaptive.rs`
