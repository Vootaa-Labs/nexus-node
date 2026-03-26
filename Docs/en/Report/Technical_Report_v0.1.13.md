# Nexus Technical Report v0.1.13

## 1. Document Purpose

This is not a vision document. It is a technical report reconstructed from the actual code, tests, scripts, tools, and workflows in the current repository.

Its purpose is to let technical readers understand, without reading the source directly:

- what the product currently does
- how the system is structured
- how the main runtime and data flows work
- what testing and formal-verification surfaces exist
- which capabilities are already implemented and which ones remain follow-on work

## 2. How To Read This Report

This report is organized from outside to inside:

1. capability surface
2. workspace structure
3. runtime mechanics
4. verification and evidence
5. interpretation boundary

The intent is to help a technical reader move from public-facing claims to code-backed operational facts without needing to read every crate directly.

## 3. Current Product Capabilities

As of `v0.1.13`, Nexus already presents the following major capability surface.

### 3.1 Node and protocol capabilities

- validator networking
- Narwhal DAG plus Shoal ordering
- stake-weighted quorum
- epoch lifecycle and committee rotation
- RocksDB persistence with cold restart recovery

### 3.2 Execution and state capabilities

- Block-STM parallel execution
- Move contract build, deploy, call, and query workflow
- multi-shard runtime
- cross-shard HTLC lock, claim, and refund flow
- BLAKE3 state commitment and proof surface

### 3.3 Higher-layer semantics and external interfaces

- intent compile, resolve, queue, and execute support paths
- Agent Core session, capability, A2A, confirm, and provenance skeleton
- REST, WebSocket, and MCP external interfaces
- developer CLI, keygen, genesis, benchmark, simulator, and monitor tools

## 4. System Structure

## 4.1 Workspace layering

The workspace currently contains 17 Cargo packages. They can be understood in four layers.

### Semantics and cryptography layer

- `nexus-primitives`
- `nexus-crypto`

### Infrastructure layer

- `nexus-network`
- `nexus-storage`
- `nexus-config`

### Protocol and service layer

- `nexus-consensus`
- `nexus-execution`
- `nexus-intent`
- `nexus-rpc`

### Assembly, tooling, and test layer

- `nexus-node`
- `nexus-keygen`
- `nexus-genesis`
- `nexus-wallet`
- `nexus-bench`
- `nexus-simulator`
- `nexus-monitor`
- `tests/nexus-test-utils`

## 4.2 Assembly boundary

`nexus-node` is the main assembly boundary. It wires configuration, storage, networking, consensus, execution, intent, RPC, and background tasks together.

Observable facts from the current `main.rs` include:

- `RocksStore` is used by default
- session and provenance also use RocksDB-backed stores
- startup recovers session, provenance, genesis, and committee-related state
- long-lived tasks handle anchoring, intent watching, and session cleanup

## 5. Module Responsibilities

### 5.1 `nexus-consensus`

This crate owns certificate construction and verification, DAG storage, Shoal ordering, committee rules, and epoch progression.

The current module surface includes not only `engine.rs`, `certificate.rs`, `dag.rs`, `shoal.rs`, and `validator.rs`, but also `epoch_manager.rs` for epoch-side transition logic.

### 5.2 `nexus-execution`

This crate owns transaction execution, the Block-STM concurrency model, the Move adapter layer, and execution services.

The current code shows that:

- the default build enables `move-vm`
- the feature gate still exists for controlled comparison builds
- serial reference and parallel execution paths coexist, which helps correctness validation

### 5.3 `nexus-intent`

This is not just a parser crate. It consists of compiler, resolver, and agent-core subsystems.

The current Agent Core surface includes:

- envelope and principal modeling
- session lifecycle
- capability snapshot and policy handling
- planner, dispatcher, and intent planner bridge
- A2A negotiation
- provenance recording
- both in-memory and RocksDB-backed session and provenance stores

### 5.4 `nexus-rpc`

The real external API surface today is:

- REST
- WebSocket
- MCP

There is no standalone GraphQL or gRPC implementation in the current code. Some historical metadata and environment variables still reference those future slots, but they should not be treated as current feature claims.

The REST surface is broader than basic health, account, and transaction routes. It also includes:

- chain head routes
- shard topology routes
- HTLC query routes
- state proof routes
- session and provenance inspection routes

## 6. Runtime Mechanics

## 6.1 Node startup path

The current startup sequence can be summarized as:

1. load and validate `NodeConfig`
2. initialize tracing and the tokio runtime
3. open RocksDB storage
4. recover session and provenance state
5. load genesis and validate chain identity
6. assemble committee, consensus, execution, intent, and RPC subsystems
7. start networking, background tasks, and readiness tracking

## 6.2 Transaction and intent flow

For the traditional transaction path, the main flow is:

1. a client submits through REST
2. the node routes into mempool and gossip propagation
3. batch proposer and consensus produce executable batches
4. the execution bridge executes those batches and updates chain head, receipts, and storage

For the intent path, the flow adds semantic handling:

1. compiler and resolver apply higher-level constraints and planning
2. planner and dispatcher drive a simulate, plan, confirm, and execute path
3. session and provenance continuously record context and results

## 6.3 State and proof flow

The current state-proof chain in code includes:

- storage writes
- BLAKE3 commitment updates
- inclusion and exclusion proof generation and verification
- proof RPC exposure for external observers

This means state commitment is not only an internal data structure. It is part of the observable external capability surface.

## 7. Data and Control Flow

The system can be read as five cooperating lines.

### 7.1 Configuration control flow

`nexus-config` aggregates node, network, storage, genesis, and RPC configuration, which `nexus-node` consumes at startup.

### 7.2 Consensus control flow

Batches move from proposers into the DAG, then through certificate verification, Shoal ordering, and quorum rules into executable order.

### 7.3 Execution data flow

Transactions or cross-shard operations enter the executor, use storage-backed state views, and write updated state, receipts, and chain-head snapshots.

### 7.4 Agent Core semantic flow

Requests are wrapped as envelopes, then constrained by session, capability, and policy, and finally routed through planning, dispatch, confirmation, and provenance recording.

### 7.5 External interface flow

REST, WebSocket, and MCP are adapter layers. They call backend traits that are implemented and injected by `nexus-node`; they do not directly embed the business logic.

## 8. Testing and Verification

## 8.1 Test structure

`tests/nexus-test-utils` is not only a fixture crate. It contains many scenario modules, including:

- pipeline
- multinode
- node e2e
- RPC integration
- resilience
- recovery
- readiness
- persistence
- staking rotation
- multi-shard
- HTLC
- release regression

This shows that testing has already expanded into cross-crate, cross-module, and cross-lifecycle system behavior.

## 8.2 CI and evidence gates

The current workflow surface includes:

- lint
- security audit
- test execution
- coverage
- crypto KAT
- workspace check
- benchmark comparison
- conditional fuzz workflow

These gates turn implementation presence into continuously checked evidence.

## 8.3 Formal and differential verification assets

The repository contains:

- TLA+
- Haskell
- Agda
- Move Prover
- property tests
- differential corpus assets

Among these, the differential runner and property tests appear to be integrated more directly into the active engineering path. The other proof assets are present and valuable, but their automation depth is still uneven across subsystems.

## 9. Current Technical Characteristics

### 9.1 Explicit assembly boundary

`nexus-node` remains a thin assembly layer instead of becoming a monolithic logic container.

### 9.2 Good separation for parallel subsystem evolution

Consensus, execution, intent, and RPC are layered clearly enough to evolve without collapsing into one crate.

### 9.3 Reality-first documentation boundary

At `v0.1.13`, the correct documentation posture is to describe only what the current code, tests, scripts, and workflows can support. That is especially important for API surface, persistence behavior, and Agent Core maturity claims.

## 10. Closing Summary

The `v0.1.13` baseline already looks like a system-level engineering codebase rather than a loosely assembled feature prototype. Its main value lies in the combination of:

- clear crate boundaries
- real persistence and restart semantics
- observable external interfaces
- verification and operational evidence

The remaining work is therefore less about proving existence and more about hardening, calibration, and convergence.

### 8.3 AI and agent-aware interface model

MCP and Agent Core indicate that the project is designed for more than conventional blockchain transactions. It also targets higher-level semantic and tool-driven interactions.

### 8.4 Evidence-oriented engineering

Context routing docs, tests, CI, proof assets, and operator runbooks together form an engineering evidence chain.

## 9. Current Boundaries and Follow-On Work

Inferred directly from the current code, the main continuing areas are:

1. stronger gas metering quality and resource bounding in the Move VM path
2. fuller closure from Agent Core execution to real signing and chain submission
3. further configuration of operator parameters and governance gates
4. deeper automated verification coverage across more subsystems

## 10. Conclusion

The `v0.1.13` Nexus baseline already has the key characteristics of a system-level engineering codebase:

- clear module boundaries
- runnable primary paths
- persistence and recovery
- testing and evidence surfaces
- landed agent-facing and multi-shard capabilities

That is why the correct public framing for this repository is not a set of future promises, but an accurate explanation of what this code already implements, how it is organized, how it runs, and how it is verified.