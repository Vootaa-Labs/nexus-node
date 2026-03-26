# Nexus Crypto KEM Context

## Purpose

Use this file for ML-KEM or entropy-source questions.

## First Read Set

1. `Kem_Summary.md`
2. `src/lib.rs`
3. `src/mlkem.rs`
4. `src/csprng.rs` when randomness or key generation is relevant

## Cross-Crate Notes

- KEM support is exposed through tool crates and can influence secure channel setup assumptions.
- Check `domains.rs` if key derivation or transcript separation is part of the task.
