# nexus-move v0.1.1 Dependency Baseline

## Overview

`nexus-node` v0.1.13 consumes the Move smart-contract subsystem as an external git dependency from [`nexus-move`](https://github.com/vootaa-labs/nexus-move) at tag `v0.1.1`.

## Dependency Declaration

```toml
# nexus-node root Cargo.toml
nexus-move-types    = { git = "https://github.com/vootaa-labs/nexus-move", tag = "v0.1.1" }
nexus-move-bytecode = { git = "https://github.com/vootaa-labs/nexus-move", tag = "v0.1.1" }
nexus-move-runtime  = { git = "https://github.com/vootaa-labs/nexus-move", tag = "v0.1.1" }
nexus-move-package  = { git = "https://github.com/vootaa-labs/nexus-move", tag = "v0.1.1" }
```

## Facade Crates Consumed

| Crate | Role in nexus-node |
|---|---|
| `nexus-move-types` | Shared types: `VmOutput`, `FunctionCall`, `UpgradePolicy`, etc. |
| `nexus-move-bytecode` | Bytecode policy and publish preflight verification |
| `nexus-move-runtime` | Execution facade, VM backends, gas metering, upstream type re-exports |
| `nexus-move-package` | Package build pipeline (used by `nexus-wallet move build`) |

## Key Feature Flags

| Flag | Enabled by nexus-node | Effect |
|---|---|---|
| `vm-backend` | Yes (nexus-execution, nexus-rpc) | Real Move VM execution + `upstream` re-export module |
| `verified-compile` | Optional | Bytecode verification during package builds |
| `native-compile` | Optional | Compilation via vendored `move-compiler-v2` |

## Upstream Type Access

All upstream Move types (from `move-core-types`, `move-binary-format`, `move-vm-runtime`, `move-vm-types`) are accessed **exclusively** through `nexus_move_runtime::upstream::*`. Direct vendor crate imports are prohibited.

```rust
// Correct
use nexus_move_runtime::upstream::move_core_types::account_address::AccountAddress;

// Prohibited — never depend on vendor crates directly
// use move_core_types::account_address::AccountAddress;
```

## Version Pinning

- `nexus-move` is pinned by git tag (`v0.1.1`), not branch or revision.
- `Cargo.lock` records the exact commit hash for reproducibility.
- Upgrading requires changing the tag in `Cargo.toml` and running `cargo update`.

## Documentation

Full `nexus-move` documentation is maintained in its own repository:
- Architecture, facade mapping, development, and release docs under `docs/`
- See the [nexus-move README](https://github.com/vootaa-labs/nexus-move) for details.
