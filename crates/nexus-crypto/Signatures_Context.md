# Nexus Crypto Signatures Context

## Purpose

Use this file for signing, verification, key material encoding, and signature-domain questions.

## First Read Set

1. `Signatures_Summary.md`
2. `src/lib.rs`
3. one of `falcon.rs` or `mldsa.rs`
4. `traits.rs` or `domains.rs` if the task crosses algorithms

## Cross-Crate Notes

- Falcon is used for validator and consensus-facing signatures.
- ML-DSA is used for transaction-facing signatures.
- Tool crates surface these algorithms directly, especially `nexus-keygen`.
