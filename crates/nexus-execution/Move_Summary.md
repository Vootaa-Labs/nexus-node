# Nexus Move Summary

## Scope

This summary covers the Move adapter area inside `nexus-execution`.

## Module Map

| Module | Current responsibility |
| --- | --- |
| `move_adapter/mod.rs` | local Move boundary and VM selection |
| `move_runtime.rs` | feature-gated Move VM path |
| `nexus_vm.rs`, `builtin_vm.rs` | non-runtime execution paths |
| `publisher.rs`, `package.rs`, `verifier.rs` | publish and bytecode validation path |
| `query.rs`, `resources.rs`, `state_view.rs`, `type_bridge.rs` | read path and state bridging |
| `session.rs`, `events.rs`, `entry_function.rs` | execution-session helpers |
| `gas_meter.rs`, `vm_config.rs`, `abi.rs` | gas, configuration, and ABI support |

## Important Facts

- The Move VM path is enabled in the default build via the `move-vm` feature set.
- Public Move execution paths now use deterministic non-zero gas accounting instead of zero-value placeholders.
- `tools/nexus-wallet` is the main CLI consumer of this area via `nexus-wallet move ...`.
- Root `README.md` documents the shortest path from local devnet bring-up to `scripts/contract-smoke-test.sh` and direct `nexus-wallet move ...` usage.

## Minimal Read Paths

- VM selection: `src/move_adapter/mod.rs`
- Publish path: `src/move_adapter/publisher.rs` → `package.rs` → `verifier.rs`
- Query path: `src/move_adapter/query.rs` → `state_view.rs`
