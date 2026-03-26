# Nexus Compiler Summary

## Scope

This summary covers the compiler pipeline in `nexus-intent`.

## Module Map

| Module | Current responsibility |
| --- | --- |
| `compiler/mod.rs` | compile entry points and stage ordering |
| `validator.rs` | input validation |
| `parser.rs` | intent normalization |
| `planner.rs` | plan construction |
| `optimizer.rs` | plan simplification and heuristics |

## Important Facts

- Compiler is responsible for transforming user intent shape into internal plan shape.
- Resolver is a separate boundary for account, contract, and shard lookup.

## Minimal Read Paths

- Entry flow: `src/compiler/mod.rs`
- Input problem: `src/compiler/validator.rs` → `parser.rs`
- Plan problem: `src/compiler/planner.rs` → `optimizer.rs`
