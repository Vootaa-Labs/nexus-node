# Nexus Node Pipeline Context

## Purpose

Use this file for the live validator pipeline in `nexus-node`: mempool, batching, bridge modules, execution handoff, and state sync.

## First Read Set

1. `Pipeline_Summary.md`
2. one target stage file
3. one adjacent stage file if the handoff matters

## Routing

- `mempool.rs`: admission, dedup, capacity, TTL
- `gossip_bridge.rs`: transaction ingress from gossip into the node
- `consensus_bridge.rs`: consensus message ingress and bridge behavior
- `batch_proposer.rs` and `batch_store.rs`: batching and intermediate batch retention
- `execution_bridge.rs`: committed batch handoff into execution and events
- `state_sync.rs`: catch-up path and missing data requests

## Boundary Note

- Use `Runtime_Context.md` if the issue is startup or handle construction rather than steady-state flow.
