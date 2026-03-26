# Nexus Network Code Summary

## Scope

`nexus-network` is the validator P2P transport layer built on libp2p.

## Module Map

| Module | Current responsibility |
| --- | --- |
| `lib.rs` | crate exports and handles |
| `config.rs` | network config |
| `transport/` | swarm transport, QUIC path, connection pooling |
| `discovery/` | DHT and peer discovery helpers |
| `gossip/` | pubsub flow, deduplication, scoring |
| `service.rs` | composed network service |
| `rate_limit.rs` | peer message rate limiting |
| `codec.rs` | message codec |
| `types.rs` | peer IDs, topics, message metadata |
| `metrics.rs` | network metrics hooks |
| `traits.rs` | transport and discovery traits |

## Important Facts

- Crate root forbids unsafe code.
- Network service is built here, but node-level message semantics live in `nexus-node` bridge modules.
- Tests are colocated in module files rather than in a separate crate-level test folder.

## Minimal Read Paths

- Build and lifecycle: `src/lib.rs` → `service.rs` → `transport/mod.rs`
- Discovery: `src/lib.rs` → `discovery/mod.rs`
- Gossip behavior: `src/lib.rs` → `gossip/mod.rs` → `gossip/scoring.rs`
