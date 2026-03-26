# Nexus Config Context

## Purpose

Use this file for node startup configuration, genesis loading, environment overrides, and validation behavior.

## First Read Set

1. `Config_Summary.md`
2. `src/lib.rs`
3. `src/node.rs`
4. one relevant subsystem config file

## Module Routing

- `node.rs`: aggregate `NodeConfig`, file loading, default shaping
- `genesis.rs`: `GenesisConfig`, validator entries, allocation validation
- `consensus.rs`, `execution.rs`, `intent.rs`, `rpc.rs`, `telemetry.rs`: subsystem-specific config structures
- `dirs.rs`: data/config directory validation
- `error.rs`: config error surface

## Cross-Crate Notes

- `nexus-node` reads this crate at startup.
- `NetworkConfig` and `StorageConfig` are re-exported from other crates instead of duplicated here.
- If a config question touches deployment, also inspect `scripts/setup-devnet.sh`, `docker-compose.yml`, or workflow files.
