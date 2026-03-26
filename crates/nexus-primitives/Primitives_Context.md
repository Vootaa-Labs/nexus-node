# Nexus Primitives Context

## Purpose

Use this file when the task involves protocol identifiers, digest types, addresses, amounts, or other low-level value semantics shared across the workspace.

## Read This Before Code

1. `Primitives_Summary.md`
2. `src/lib.rs`
3. exactly one of `ids.rs`, `digest.rs`, `address.rs`, or `traits.rs`

## Module Routing

- `ids.rs`: epoch, round, shard, validator, commit sequence, timestamp newtypes
- `digest.rs`: `Blake3Digest` and semantic digest aliases such as `TxDigest`, `BatchDigest`, `CertDigest`, `BlockDigest`, `StateRoot`
- `address.rs`: `AccountAddress`, `ContractAddress`, `TokenId`, `Amount`
- `traits.rs`: protocol ID trait constraints
- `error.rs`: decode and crate-level primitive errors

## When To Stop Reading

- If the issue is only about type shape or serialization, stay in this crate.
- Expand to another crate only after identifying the consumer of the primitive.

## Cross-Crate Notes

- Every other production crate depends directly or indirectly on this crate.
- Serialization assumptions here affect consensus, execution, storage, rpc, and tooling.
