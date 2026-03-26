# Nexus Compiler Context

## Purpose

Use this file for intent validation, parsing, planning, and optimization inside `nexus-intent`.

## First Read Set

1. `Compiler_Summary.md`
2. `src/compiler/mod.rs`
3. one target stage file

## Routing

- `validator.rs`: preconditions and reject-fast checks
- `parser.rs`: normalization and canonical input shaping
- `planner.rs`: execution-ready plan construction
- `optimizer.rs`: plan cleanup and heuristics

## Boundary Note

- If the issue becomes about world-state lookup or shard placement, switch to `Resolver_Context.md`.
