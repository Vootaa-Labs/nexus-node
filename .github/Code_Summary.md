# Nexus Code Summary

## Workspace Profile

- Rust workspace with 15 Cargo packages.
- Core platform focus: validator networking, DAG-based consensus, BFT ordering, execution, intent routing, AI-facing APIs, and node assembly.
- External tool surface: key generation, genesis generation, developer wallet/Move tooling, and benchmark harness.
- Formal methods and property testing assets are present under `proofs/`.

## Top-Level Roots

| Path | Current role |
| --- | --- |
| `README.md` | shortest developer quick-start for local devnet, smoke tests, and `nexus-wallet move ...` |
| `Cargo.toml` | workspace members, shared dependencies, feature gates, toolchain assumptions |
| `crates/` | production libraries plus the node assembly crate |
| `tools/` | CLI tools and Criterion benchmark harness |
| `tests/nexus-test-utils/` | shared fixtures plus integration-style suites |
| `contracts/` | sample Move packages used by tooling and smoke tests |
| `scripts/` | environment checks, devnet setup, smoke tests |
| `.github/workflows/` | CI, benchmark, fuzz, and release automation |
| `Dockerfile`, `docker-compose-<context>.yml` | container build and devnet layout (compose files generated at runtime, not git-tracked) |
| `proofs/` | Agda, TLA+, Haskell, Move Prover, differential and property-test assets |

## Production Crate Map

| Crate | Actual responsibility |
| --- | --- |
| `nexus-primitives` | protocol IDs, digests, addresses, amounts, and core traits |
| `nexus-crypto` | Falcon signatures, ML-DSA signatures, ML-KEM, hashing, domain separation, CSPRNG |
| `nexus-network` | libp2p transport, peer discovery, gossip, rate limiting, network service orchestration |
| `nexus-storage` | `StateStorage` traits, key encoding, `MemoryStore`, RocksDB backend |
| `nexus-config` | node and subsystem config loading, defaults, env overrides, genesis and directory validation |
| `nexus-consensus` | Narwhal certificate pipeline, in-memory DAG, Shoal ordering, committee management |
| `nexus-execution` | Block-STM executor, execution service, transaction types, Move adapter layer |
| `nexus-intent` | intent compilation, shard resolution, metrics, service queue, agent core engine |
| `nexus-rpc` | REST endpoints, WebSocket event channel, MCP adapter, middleware, DTOs, RPC service builder |
| `nexus-node` | startup path, backend adapters, mempool, batch proposer, consensus bridge, execution bridge, state sync, validator discovery |

## Tooling Map

| Tool | Actual responsibility |
| --- | --- |
| `nexus-keygen` | validator key bundles and libp2p identity generation |
| `nexus-genesis` | generate, validate, and create test genesis JSON |
| `nexus-wallet` | primary developer wallet CLI for address, balance, transfer, status, faucet, and Move commands |
| `nexus-bench` | Criterion bench harness with subsystem-specific bench files |

## Tests, Proofs, and Ops Assets

- Integration-heavy test support lives in `tests/nexus-test-utils/src/`.
- Consensus and intent each include property-test files in their own `tests/` directories.
- `scripts/setup-devnet.sh` bootstraps key material, genesis, and per-node config.
- `scripts/smoke-test.sh` checks readiness, health, restart recovery, and late-join behavior.
- `scripts/contract-smoke-test.sh` is the shortest end-to-end contract path after devnet bring-up and uses `nexus-wallet move ...`.
- `.github/workflows/ci.yml` defines lint, security, test, coverage, crypto-KAT, and workspace-check jobs.
- `.github/workflows/bench.yml` compares benchmark baselines on PRs.
- `.github/workflows/fuzz.yml` is present but only runs if a `fuzz/` workspace exists.

## Important Current Facts

1. `nexus-execution` enables the `move-vm` path in the default build, while still keeping the feature gate available for targeted comparison builds.
2. Public execution and intent receipt paths no longer rely on `gas_used: 0` placeholders; remaining gas work is calibration and coverage quality.
3. Remediation tracking now lives in `Docs/Report/BACKLOG.md` and `Docs/Report/OPTIMIZATION_DEBT.md`; treat them as refreshed, code-verified planning docs rather than historical snapshots.
4. Current code surface for external APIs is REST, WebSocket, and MCP; a standalone GraphQL module was not observed.
5. Workspace toolchain, GitHub workflows, and Docker builder are now aligned to Rust 1.85.0; keep future CI or release changes pinned to the same version.
6. The main developer CLI is `nexus-wallet`; contract build/deploy/query flows are entered through `nexus-wallet move ...`.

## Minimal Reading Recipes

| Question | Read this sequence |
| --- | --- |
| How does a node start? | `.github/Code_Context.md` → `crates/nexus-node/Runtime_Context.md` → `crates/nexus-node/src/main.rs` |
| How does tx ordering work? | `crates/nexus-consensus/Code_Context.md` → `.github/Code_Summary.md` → `crates/nexus-consensus/src/engine.rs` |
| How does execution work? | `crates/nexus-execution/Move_Context.md` or `Block_Stm_Context.md` → crate root → one target module |
| How do I bring up a local developer flow? | `README.md` → `scripts/setup-devnet.sh` → `scripts/smoke-test.sh` → `scripts/contract-smoke-test.sh` |
| How does intent become concrete work? | `crates/nexus-intent/Compiler_Context.md` or `Resolver_Context.md` → crate root → one implementation file |
| How do APIs reach the node? | `crates/nexus-rpc/Rest_Context.md` → `crates/nexus-rpc/src/server.rs` → `crates/nexus-node/src/backends.rs` |
| How is devnet assembled? | `README.md` → `scripts/setup-devnet.sh` → `docker-compose-n7s.yml` (generated) |
| Where are integration tests? | `tests/nexus-test-utils/Test_Context.md` → `Test_Summary.md` → target suite |

## What To Avoid Loading By Default

- `target/` and generated benchmark output
- proof directories unrelated to the active task
- every crate root at once
- multiple workflow files unless the task is build, CI, release, or ops related
- tool crates when the task is isolated to library internals