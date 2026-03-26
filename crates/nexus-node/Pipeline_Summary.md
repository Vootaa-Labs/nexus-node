# Nexus Node Pipeline Summary

## Scope

This summary covers the steady-state validator pipeline in `nexus-node`.

## Module Map

| Module | Current responsibility |
| --- | --- |
| `mempool.rs` | transaction queueing and admission |
| `gossip_bridge.rs` | gossip ingress |
| `consensus_bridge.rs` | consensus ingress and bridge logic |
| `batch_proposer.rs` | mempool drain and batch proposal |
| `batch_store.rs` | batch retention and lookup |
| `execution_bridge.rs` | committed batch execution handoff |
| `state_sync.rs` | state sync and catch-up |

## Important Facts

- These modules orchestrate handoffs; they do not redefine consensus, execution, or network semantics.
- Node pipeline debugging should start at the failing stage rather than at `main.rs`.

## Minimal Read Paths

- Ingress problem: `src/gossip_bridge.rs` → `mempool.rs`
- Batch problem: `src/batch_proposer.rs` → `batch_store.rs`
- Commit-to-execution problem: `src/execution_bridge.rs` → `batch_store.rs`
