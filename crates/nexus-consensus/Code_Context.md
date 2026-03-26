# Nexus Consensus Code Context

## Purpose

Use this file for Narwhal certificate flow, DAG insertion, Shoal ordering, committee behavior, and consensus engine orchestration.

## First Read Set

1. `Code_Summary.md`
2. `src/lib.rs`
3. `src/engine.rs`
4. one target stage file

## Module Routing

- `engine.rs`: end-to-end verify, insert, order, commit flow
- `certificate.rs`: certificate building, digest helpers, signature verification
- `dag.rs`: in-memory DAG and parent/causality checks
- `shoal.rs`: BFT ordering and commit logic
- `validator.rs`: committee composition and quorum math
- `traits.rs` and `types.rs`: public contracts and compatibility-sensitive data types

## Cross-Crate Notes

- `nexus-node` owns the network bridge into consensus.
- Consensus types flow into execution and node integration code.
- Property-oriented tests live in `tests/fv_property_tests.rs` and `tests/fv_proptest.rs`.
