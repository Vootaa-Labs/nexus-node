# Nexus Crypto Signatures Summary

## Scope

This summary covers signature schemes and their supporting traits in `nexus-crypto`.

## Module Map

| Module | Current responsibility |
| --- | --- |
| `falcon.rs` | Falcon signing and verify key types |
| `mldsa.rs` | ML-DSA signing and verify key types |
| `traits.rs` | signing and verification traits |
| `domains.rs` | signature domain separation labels |
| `error.rs` | crypto errors |

## Important Facts

- Crate root forbids unsafe code.
- Signature code is part of validator bootstrap and transaction signing flows.
- Internal assumptions should be validated against consumers in consensus or tooling before changing encodings.

## Minimal Read Paths

- Falcon path: `src/lib.rs` → `falcon.rs`
- ML-DSA path: `src/lib.rs` → `mldsa.rs`
- Shared contracts: `src/lib.rs` → `traits.rs` → `domains.rs`
