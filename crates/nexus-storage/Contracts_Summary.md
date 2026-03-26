# Nexus Storage Contracts Summary

## Scope

This summary covers the stable interfaces and data-shaping layer of `nexus-storage`, not backend-specific implementation details.

## Module Map

| Module | Current responsibility |
| --- | --- |
| `lib.rs` | crate exports |
| `traits.rs` | storage and commitment contracts |
| `types.rs` | typed keys, column family enum, write ops |
| `config.rs` | storage config |
| `error.rs` | storage errors |

## Important Facts

- Crate root forbids unsafe code.
- The contract layer is shared by both `MemoryStore` and `RocksStore`.
- Changes here usually require checking execution, node, and tests.

## Minimal Read Paths

- Trait question: `src/lib.rs` → `traits.rs`
- Encoding question: `src/lib.rs` → `types.rs`
- Config question: `src/lib.rs` → `config.rs`
