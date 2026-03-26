# Nexus Primitives Summary

## Scope

`nexus-primitives` is the workspace base layer. It defines stable newtypes and digest/address value objects that other crates build on.

## Module Map

| Module | Current responsibility |
| --- | --- |
| `lib.rs` | crate re-exports and module declaration |
| `ids.rs` | integer newtypes for validator, epoch, round, shard, commit sequence, timestamp |
| `digest.rs` | BLAKE3-backed digest wrapper plus semantic aliases |
| `address.rs` | account and contract addressing plus token and amount types |
| `traits.rs` | `ProtocolId` trait |
| `error.rs` | primitive error types |

## Key Facts

- Crate root forbids unsafe code.
- Types are designed for serde and BCS use across the workspace.
- Tests in source files cover serialization and round-trip behavior.

## Minimal Read Paths

- ID semantics: `src/lib.rs` → `ids.rs`
- Digest or hash alias questions: `src/lib.rs` → `digest.rs`
- Address or token questions: `src/lib.rs` → `address.rs`
