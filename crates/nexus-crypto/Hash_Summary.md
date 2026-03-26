# Nexus Crypto Hash Summary

## Scope

This summary covers the BLAKE3 hashing path and domain separation support in `nexus-crypto`.

## Module Map

| Module | Current responsibility |
| --- | --- |
| `hasher.rs` | BLAKE3-backed hashing helpers |
| `domains.rs` | domain separation constants and labels |
| `error.rs` | crypto error surface used by hash helpers |

## Minimal Read Paths

- Hash API: `src/lib.rs` → `hasher.rs`
- Domain separation: `src/lib.rs` → `domains.rs`
