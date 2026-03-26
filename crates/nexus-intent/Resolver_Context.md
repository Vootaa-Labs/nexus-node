# Nexus Resolver Context

## Purpose

Use this file for account resolution, contract lookup, shard placement, and balance aggregation inside `nexus-intent`.

## First Read Set

1. `Resolver_Summary.md`
2. `src/resolver/mod.rs`
3. one target helper file

## Routing

- `mod.rs`: resolver entry points and orchestration
- `balance_agg.rs`: balance merge logic
- `contract_registry.rs`: contract lookup and metadata
- `shard_lookup.rs`: shard mapping and routing

## Boundary Note

- If the issue is still about transforming user input rather than resolving world state, switch back to `Compiler_Context.md`.
