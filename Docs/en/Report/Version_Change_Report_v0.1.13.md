# Nexus Version Change Report v0.1.13

## 1. Document Purpose

This report explains the functional progression of Nexus up to `v0.1.13` without relying on legacy Git history in the new public repository.

It is not a task log or development diary. It is a reconstruction of delivered capabilities based on three evidence sources:

- the current `v0.1.13` codebase and test surface
- the non-documentation Git commit stream
- the roadmap and audit materials for `v0.1.1` through `v0.1.11`

There was no formal version tracking before `v0.1.0`, so this report starts from the first identifiable release sequence.

## 2. How To Use This Report

This report is intended to answer two questions:

1. how the current baseline was formed
2. which capabilities should now be treated as real rather than aspirational

It should therefore be read as a lineage and interpretation document, not as a release checklist or implementation diary.

## 3. Executive Conclusion

From `v0.1.1` to `v0.1.13`, Nexus did not progress by loosely adding features. The codebase matured in a recognizable sequence:

1. establish the workspace, crypto, network, storage, consensus, execution, intent, RPC, and node assembly layers
2. connect devnet, tooling, tests, CI, Move flows, and the default developer path
3. harden the system from merely runnable to recoverable, verifiable, governable, and scalable
4. complete the main capability loops for staking, epoch rotation, multi-shard runtime, HTLC, Agent Core, and verification runners

By `v0.1.13`, the project has moved beyond the phase of filling missing infrastructure. The dominant work now is runtime hardening, interface convergence, and clearer public communication of what already exists.

## 4. Version Lineage

| Version | Primary progression | Delivered result |
| --- | --- | --- |
| `v0.1.0` | task-driven foundation period | workspace skeleton and subsystem boundaries were formed |
| `v0.1.1` | devnet, developer path, Move tooling, end-to-end flow | `nexus-wallet move ...` became the unified CLI; local devnet, smoke tests, and deploy/call/query flows became coherent |
| `v0.1.2` | testnet hardening and security boundaries | rate limiting, validation, recovery, and security gates tightened the system for stricter testnet use |
| `v0.1.3` | execution and consensus correctness | Block-STM behavior, transaction validation, consensus safety, and state-sync validation were tightened |
| `v0.1.4` | runtime truthfulness and reconfiguration groundwork | `/ready`, safer genesis boot, epoch and committee infrastructure, proof RPC, and query governance were wired in |
| `v0.1.5` | testnet-grade evidence and calibration | chaos, soak, proof, gas calibration, and config drift checks created stronger operational evidence |
| `v0.1.6` | persistence closure | DAG, BatchStore, cold restart recovery, and RocksDB-backed paths became recoverable rather than in-memory only |
| `v0.1.7` | economic and governance foundation | token precision was corrected and stake-weighted quorum replaced count-based voting |
| `v0.1.8` | production state commitment | inclusion and exclusion proofs, persistent commitment trees, and a canonical state root were completed |
| `v0.1.9` | staking and committee rotation | staking contract lifecycle, snapshots, election, rotation, and recovery paths became part of runtime behavior |
| `v0.1.10` | multi-shard and cross-shard execution | multi-shard runtime, shard-aware mempool/gossip/state sync, and HTLC lock/claim/refund were wired end to end |
| `v0.1.11` | Agent Core and verification evidence | ACE skeleton, MCP, session and provenance persistence, and differential/property-test runners entered the main path |
| `v0.1.12` | Runtime Hardening | Move VM boundary hardening, gas/payload interface formalization, config externalization, PQC (ML-DSA-65) integration |
| `v0.1.13` | consolidation against reality and public repo reset | audit evidence shows the main capability surface is present; emphasis shifts to hardening and public-facing restructuring |

## 5. Cross-Version Themes

### 5.1 From prototype paths to real runtime paths

Earlier releases mainly answered whether a subsystem existed at all. Later releases converted those paths into real runtime behavior:

- the Move VM path moved from adapter groundwork to the default real execution path
- readiness moved from interface presence to subsystem-backed truthfulness
- proof handling moved from surface presence to verifiable, persistent, recoverable state commitments

### 5.2 From memory-local state to persistent protocol state

One of the clearest themes is that the persistence boundary kept moving upward:

- session and provenance persistence came first
- then DAG, BatchStore, and cold restart recovery
- then state commitment persistence with dedicated RocksDB column families

This changed the node from being merely correct while alive to being continuous across restarts.

### 5.3 From static setup to governance-driven runtime

After `v0.1.7`, the system clearly moved away from manually fixed committees and toward runtime governance based on stake and snapshots:

- quorum became stake-weighted
- staking contracts gained register, bond, unbond, withdraw, and slash lifecycle support
- committee rotation and epoch lifecycle became part of the actual node path

### 5.4 From shard-aware design to multi-shard runtime

The codebase had shard-aware data structures earlier, but the runtime became truly multi-shard in `v0.1.10`. That shift included:

- shard-aware mempool behavior
- shard-aware gossip and state sync
- per-shard chain-head and RPC observation surfaces
- HTLC-based cross-shard lock, claim, and refund flow

### 5.5 From feature claims to evidence chains

Later versions increasingly require evidence, not just implementation:

- integration and scenario test coverage expanded significantly
- CI gates cover lint, security, coverage, KAT, Move smoke, and benchmark regression checks
- differential corpora, property tests, and proof assets became part of the verification surface

## 6. Actual State at v0.1.13

The current code and audit evidence show that the following are part of the real baseline, not future intent:

- REST, WebSocket, and MCP external surfaces
- RocksDB-backed node, session, and provenance persistence
- stake-weighted consensus, epoch lifecycle, and staking rotation
- multi-shard execution and HTLC
- BLAKE3 state commitment and proof surfaces
- Agent Core skeleton, A2A negotiation, confirm flow, and provenance recording
- Move contract build, deploy, call, and query developer workflow
- sustained evidence through tests, scripts, CI, and verification runners

## 7. What Is Still a Follow-On Area

This report also needs to be explicit about what has not fully converged yet:

1. The real Move VM path is present, but gas metering quality still needs stronger hardening.
2. Agent Core execution receipts still need a more direct closure with real signing and chain submission.
3. Some metadata and comments still contain GraphQL and gRPC language from future slots, while the real public surface remains REST, WebSocket, and MCP.
4. The codebase now has multi-shard and governance infrastructure, but production-grade capacity tuning, permissions, and operator controls still need continued work.

## 8. Public Positioning Guidance

If the new public repository starts without legacy Git history, the external framing should be:

- this is a `v0.1.13` baseline that already went through multiple rounds of code-audited convergence
- the documentation focuses on what the current code actually provides
- legacy roadmap and audit materials were used to reconstruct lineage, but they are not the public centerpiece

## 9. Closing Statement

The main line of Nexus up to `v0.1.13` can be summarized simply:

it evolved from a clearly structured Rust blockchain workspace into a system-level baseline with governance, sharding, Move execution, Agent Core, evidence-oriented verification, and operator-facing runtime paths.