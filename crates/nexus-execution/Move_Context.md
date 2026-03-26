# Nexus Move Context

## Purpose

Use this file for the Move adapter layer inside `nexus-execution`: VM selection, package publishing, query paths, and state/type bridges.

## First Read Set

1. `Move_Summary.md`
2. `src/move_adapter/mod.rs`
3. one implementation file such as `nexus_vm.rs`, `builtin_vm.rs`, or `move_runtime.rs`
4. one helper file such as `state_view.rs`, `type_bridge.rs`, or `gas_meter.rs`

## Routing

- `mod.rs`: VM boundary, feature-gated selection, shared adapter exports
- `move_runtime.rs`: real Move VM path behind `move-vm`
- `nexus_vm.rs` and `builtin_vm.rs`: local fallback implementations
- `publisher.rs`, `package.rs`, `verifier.rs`: package and publish path
- `query.rs`, `resources.rs`, `state_view.rs`: state reads and resource queries
- `gas_meter.rs`, `vm_config.rs`, `abi.rs`: policy and ABI support

## Current Caveats

- `move-vm` is now part of default features; feature gates still exist for targeted compilation and comparison.
- Public execution paths should no longer emit `gas_used: 0` placeholders; remaining work is calibration quality, not zero-value stubs.
- The main developer CLI consumer is now `nexus-wallet move ...`; root `README.md` is the fastest route to current devnet and contract-flow commands.
- If the issue is parallel scheduling rather than VM semantics, switch to `Block_Stm_Context.md`.
