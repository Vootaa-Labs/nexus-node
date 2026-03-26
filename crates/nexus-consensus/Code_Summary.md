# Nexus Consensus Code Summary

## Scope

`nexus-consensus` implements the Narwhal plus Shoal++ path for Nexus: certificate verification, DAG storage, ordering, committee logic, and engine orchestration.

## Module Map

| Module | Current responsibility |
| --- | --- |
| `lib.rs` | crate exports and re-exports |
| `types.rs` | batches, certificates, votes, anchors, validator metadata |
| `traits.rs` | proposer, DAG, orderer, validator registry traits |
| `certificate.rs` | certificate construction and verification |
| `dag.rs` | in-memory DAG implementation |
| `shoal.rs` | ordering and commit logic |
| `validator.rs` | committee and quorum rules |
| `engine.rs` | stage orchestration |
| `error.rs` | consensus error model |

## Important Facts

- Crate root forbids unsafe code.
- The engine is the best starting point for runtime flow, but quorum and type invariants live in stage-specific files.
- Consensus-specific property tests are present in the crate `tests/` directory.

## Minimal Read Paths

- Full flow: `src/lib.rs` → `engine.rs`
- Certificate issue: `src/lib.rs` → `certificate.rs` → `types.rs`
- Ordering issue: `src/lib.rs` → `shoal.rs` → `validator.rs`
