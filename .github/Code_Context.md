# Nexus Code Context

## Purpose

This is the default routing document for code work in the Nexus workspace.
Read this file first, then `.github/Code_Summary.md`, then `README.md` when you need the current developer entrypoints or local bring-up path, then only the smallest crate-level context or summary file that matches the task.

## Workspace Snapshot

- Version: v0.1.14
- Workspace members: 15 Cargo packages.
- Core libraries: primitives, crypto, network, storage, config, consensus, execution, intent, rpc.
- Assembly crate: `nexus-node` as both library and `nexus-node` binary.
- Tool crates: `nexus-keygen`, `nexus-genesis`, `nexus-wallet`, `nexus-bench`.
- Test support: `tests/nexus-test-utils`.
- Non-code operational roots: `Makefile`, `Dockerfile`, `.github/workflows/`, `scripts/`.
- Docker compose files: generated at runtime as `docker-compose-<context>.yml` (not git-tracked).

## Read Budget

1. Keep the first pass within 6 to 8 files.
2. Start at root docs before opening implementation files.
3. Prefer one crate at a time unless the call chain clearly crosses a trait boundary.
4. Read one nearby test file before claiming behavior or safety properties.
5. Open proofs only for formal verification tasks.
6. Ignore generated outputs and build artifacts by default.

## Default No-Load Paths

- `target/`
- `tools/nexus-bench/target/`
- `proofs/**` unless the task is about formal methods or proof artifacts
- generated devnet directories and temporary files
- large benchmark output folders unless the task is performance analysis

## Recommended Startup Set

1. `.github/Code_Context.md`
2. `.github/Code_Summary.md`
3. `README.md` when the task touches developer flows, scripts, or CLI usage
4. `Cargo.toml`
5. one relevant crate context or summary file
6. one relevant `src/lib.rs` or `src/main.rs`
7. one relevant implementation file
8. one nearby test, bench, script, or workflow file if the task involves behavior claims

## Routing By Task

| Task | Read first | Expand next |
| --- | --- | --- |
| Workspace architecture | `.github/Code_Summary.md` | `Cargo.toml`, `Makefile`, `.github/workflows/ci.yml` |
| Node bootstrap or runtime wiring | `crates/nexus-node/Runtime_Context.md` | `Runtime_Summary.md`, `src/main.rs`, `backends.rs`, `genesis_boot.rs` |
| Mempool, proposer, consensus bridge, state sync | `crates/nexus-node/Pipeline_Context.md` | `Pipeline_Summary.md`, then the specific bridge or stage file |
| Consensus engine | `crates/nexus-consensus/Code_Context.md` | `Code_Summary.md`, then `engine.rs`, `certificate.rs`, `dag.rs`, or `shoal.rs` |
| Move execution surface | `crates/nexus-execution/Move_Context.md` | `Move_Summary.md`, then `move_adapter/` entry modules |
| Parallel execution internals | `crates/nexus-execution/Block_Stm_Context.md` | `Block_Stm_Summary.md`, then `executor.rs`, `mvhashmap.rs`, `adaptive.rs` |
| Intent compilation | `crates/nexus-intent/Compiler_Context.md` | `Compiler_Summary.md`, then `parser.rs`, `validator.rs`, `planner.rs`, `optimizer.rs` |
| Intent resolution | `crates/nexus-intent/Resolver_Context.md` | `Resolver_Summary.md`, then `contract_registry.rs`, `shard_lookup.rs`, `balance_agg.rs` |
| Agent core | `crates/nexus-intent/Agent_Core_Context.md` | `Agent_Core_Summary.md`, then `engine.rs`, `session.rs`, `envelope.rs`, `a2a.rs`, `provenance.rs` |
| Networking | `crates/nexus-network/Code_Context.md` | `Code_Summary.md`, then `transport/`, `discovery/`, `gossip/`, `service.rs` |
| REST and WebSocket APIs | `crates/nexus-rpc/Rest_Context.md` | `Rest_Summary.md`, then `rest/` endpoint files, `ws.rs`, `server.rs` |
| MCP adapter | `crates/nexus-rpc/Mcp_Context.md` | `Mcp_Summary.md`, then `mcp/registry.rs`, `handler.rs`, `schema.rs`, `session_bridge.rs` |
| Crypto signatures | `crates/nexus-crypto/Signatures_Context.md` | `Signatures_Summary.md`, then `falcon.rs`, `mldsa.rs`, `traits.rs`, `domains.rs` |
| Crypto KEM | `crates/nexus-crypto/Kem_Context.md` | `Kem_Summary.md`, then `mlkem.rs`, `csprng.rs`, `domains.rs` |
| Hashing and domain separation | `crates/nexus-crypto/Hash_Context.md` | `Hash_Summary.md`, then `hasher.rs`, `domains.rs` |
| Storage traits and encoded keys | `crates/nexus-storage/Contracts_Context.md` | `Contracts_Summary.md`, then `traits.rs`, `types.rs`, `config.rs` |
| Storage backends | `crates/nexus-storage/Backends_Context.md` | `Backends_Summary.md`, then `memory.rs`, `rocks/mod.rs`, `rocks/schema.rs` |
| Config loading and validation | `crates/nexus-config/Config_Context.md` | `Config_Summary.md`, then `node.rs`, `genesis.rs`, relevant subsystem config |
| Primitives and protocol types | `crates/nexus-primitives/Primitives_Context.md` | `Primitives_Summary.md`, then `ids.rs`, `digest.rs`, `address.rs` |
| Shared tests and fixtures | `tests/nexus-test-utils/Test_Context.md` | `Test_Summary.md`, then target suite or fixture module |
| Local developer bring-up | `README.md` | `scripts/setup-devnet.sh`, `scripts/smoke-test.sh`, `scripts/contract-smoke-test.sh` |
| Tooling or ops | `.github/Code_Summary.md` | relevant file in `tools/`, `scripts/`, `Dockerfile`, `docker-compose-<context>.yml`, or workflow |

## Known Cross-Crate Boundaries

- `nexus-node` is the main assembly point for network, consensus, execution, intent, rpc, config, and storage.
- `nexus-execution` depends on `nexus-consensus` types and `nexus-storage` state access.
- `nexus-intent` depends on `nexus-execution` transaction shapes and emits work into consensus and execution.
- `nexus-rpc` depends on backend traits implemented in `nexus-node`, not on direct business logic modules.
- `nexus-config` re-exports network and storage config types instead of redefining them.

## Current Caveats To Remember

- The Move VM path still exists behind `move-vm`, but the default build now enables that feature.
- Gas accounting now returns non-zero deterministic values across public execution paths; economic calibration quality still needs continued verification.
- Root docs must reflect REST, WebSocket, and MCP. A standalone GraphQL module was not observed in current code.
- `.github/Code_Summary.md` is the source of truth for workspace-wide routing; use crate summaries for details.
- `README.md` is the source of truth for the current local quick-start path and developer CLI entrypoints.
