# Nexus Crypto Hash Context

## Purpose

Use this file for hashing, digest derivation, and domain separation questions.

## First Read Set

1. `Hash_Summary.md`
2. `src/lib.rs`
3. `src/hasher.rs`
4. `src/domains.rs`

## Cross-Crate Notes

- Hashing here feeds `nexus-primitives` digest aliases and protocol-specific digests across consensus, execution, and node code.
- If the task touches a protocol digest, also inspect the consuming crate after reading this slice.
