# Nexus Testnet Operations Guide

> Version: `v0.1.13`
> Audience: operators, release owners, testnet maintainers

## 1. Scope

This guide covers the operator path for the current baseline:

- local Docker-based rehearsal
- testnet deployment preparation
- startup and post-release validation
- alignment with the current REST, WebSocket, and MCP surface

## 2. Baseline Assumptions

### 2.1 Toolchain and host assumptions

The current repository baseline assumes:

- Rust `1.85.0`
- Docker + Compose v2
- `scripts/setup-devnet.sh` for local genesis and config generation
- `scripts/smoke-test.sh` and `scripts/contract-smoke-test.sh` for the shortest reproducible checks

### 2.2 Network and API assumptions

The real external surface in current code is:

- REST
- WebSocket
- MCP

No standalone GraphQL or gRPC implementation should be documented as current.

## 3. Local Bring-Up Summary

### 3.1 Short reproducible path

```bash
docker build -t nexus-node .
./scripts/setup-devnet.sh -o devnet -f
docker compose up -d
./scripts/smoke-test.sh
./scripts/contract-smoke-test.sh
```

The setup script currently generates 7 validators by default and writes per-node
config plus genesis material into `devnet-n7s/`.

### 3.2 Minimum validation line

After bring-up, validate in this order:

1. `/ready`
2. `/health`
3. `/metrics`
4. `/v2/consensus/status`
5. `/v2/network/status`
6. `/v2/validators`
7. `/v2/shards`

If the network is public-facing, also validate faucet, proof, and quota-sensitive routes.

### 3.3 Multi-shard note

The current repository can be configured for multi-shard behavior, but operators
should treat shard count as a genesis-scoped setting. Changes to shard topology
must be coordinated with release and restart procedures.

## 4. Operational Surfaces In The Current Codebase

The repo currently exposes operationally relevant routes for:

- node health and readiness
- network peers and network status
- consensus status
- accounts, contracts, and transactions
- shard topology
- state commitment and state proof
- faucet
- MCP-related discovery and adapter paths

## 5. Common Failure Buckets

### 5.1 Node never becomes ready

Check:

- generated `node.toml`
- mounted genesis path
- validator keys and identity keys
- RocksDB data directory permissions
- container logs

### 5.2 Cluster boots but consensus does not progress

Check:

- validator connectivity
- committee or genesis mismatch
- clock skew and deployment drift
- `/v2/consensus/status` across multiple nodes

### 5.3 API responds but high-cost routes degrade

Check:

- proof and query latency
- faucet abuse or quota exhaustion
- reverse proxy limits and TLS termination
- resource pressure on the operator host

## 6. Monitoring And Validation Priorities

The operational baseline should continuously observe:

- readiness
- commit growth
- validator-set consistency
- shard and proof route availability
- quota-sensitive route behavior on public testnet

## 7. Release And Change Coordination

Before a release or config change, review together:

- `Docs/en/Ops/Testnet_Release_Runbook.md`
- `Docs/en/Ops/Testnet_Access_Policy.md`
- `Docs/en/Ops/Testnet_SLO.md`
- `Docs/en/Ops/Schema_Migration_Guide.md`
- `Docs/en/Ops/Staking_Rotation_Runbook.md`
- `Docs/en/Ops/Epoch_Operations_Runbook.md`

## 8. Current Reality At v0.1.13

The correct operator-facing description of the current baseline is:

- the node is reproducibly bootstrapped from scripts already in the repo
- the public API surface is REST, WebSocket, and MCP
- RocksDB-backed persistence is part of the node path
- shard, proof, staking, and rotation concepts are not merely planning terms; they appear in code, scripts, or operator-facing endpoints
