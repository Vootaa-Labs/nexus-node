# Nexus Config Summary

## Scope

`nexus-config` centralizes typed startup configuration and validation for the validator node and its subsystems.

## Module Map

| Module | Current responsibility |
| --- | --- |
| `lib.rs` | re-exports all config types |
| `node.rs` | aggregate node config and load path |
| `genesis.rs` | chain ID, validators, allocations, shard count, total supply helpers |
| `consensus.rs` | consensus defaults and validation |
| `execution.rs` | execution defaults and shard settings |
| `intent.rs` | intent-layer runtime knobs |
| `rpc.rs` | REST and gRPC listen addresses, faucet, rate limits, API keys |
| `telemetry.rs` | log level and telemetry settings |
| `dirs.rs` | directory checks |
| `error.rs` | config errors |

## Important Facts

- Crate root forbids unsafe code.
- Tests in module files cover serialization and parse behavior.
- Current node startup reads a config path from the first CLI argument if present.

## Minimal Read Paths

- Node startup config: `src/lib.rs` → `node.rs`
- Genesis format: `src/lib.rs` → `genesis.rs`
- API knobs: `src/lib.rs` → `rpc.rs`
