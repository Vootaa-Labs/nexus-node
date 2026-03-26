# Nexus Resolver Summary

## Scope

This summary covers the resolver pipeline in `nexus-intent`.

## Module Map

| Module | Current responsibility |
| --- | --- |
| `resolver/mod.rs` | resolver entry points and orchestration |
| `balance_agg.rs` | balance aggregation |
| `contract_registry.rs` | contract metadata and lookup |
| `shard_lookup.rs` | shard mapping and routing |

## Important Facts

- Resolver is the lookup layer, not the plan builder.
- Account, contract, and shard semantics are intentionally separated into helper modules.

## Minimal Read Paths

- Main flow: `src/resolver/mod.rs`
- Contract issue: `src/resolver/contract_registry.rs`
- Shard issue: `src/resolver/shard_lookup.rs`
