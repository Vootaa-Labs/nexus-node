# Version History Summary: v0.1.0 → v0.1.12

This document provides a condensed functional summary of each prior version.
For detailed analysis of the current version, see the v0.1.13 reports.

## Version Progression

| Version | Codename / Focus | Key Deliverables |
|---------|-----------------|------------------|
| v0.1.0 | Genesis | Initial workspace structure, core crate layout |
| v0.1.1 | Devnet Foundation | P2P network (libp2p+QUIC), Narwhal DAG consensus, chain head, RocksDB storage, intent execution pipeline |
| v0.1.2 | Testnet Hardening | Security boundaries, input validation, RPC rate limiting, auth layer |
| v0.1.3 | Execution Correctness | Block-STM parallel execution, consensus validation, state-sync protocol |
| v0.1.4 | Runtime Truthfulness | `/ready` health endpoint, genesis boot sequence, epoch infrastructure, proof RPC |
| v0.1.5 | Evidence Grade | Chaos/soak testing, proof calibration, operational runbooks, formal verification bootstrap |
| v0.1.6 | Persistence Hardening | DAG recovery, BatchStore persistence, cold restart resilience |
| v0.1.7 | Economic Foundation | Token precision (18→9 decimal bits), stake-weighted quorum, MVCC shared data structures |
| v0.1.8 | State Commitment | BLAKE3 Merkle state commitment, exclusion proofs, state root productionization |
| v0.1.9 | On-Chain Staking | Staking contract, committee rotation driven by canonical state |
| v0.1.10 | Multi-Shard Execution | Shard-aware execution, cross-shard transaction coordination |
| v0.1.11 | Documentation & Agent Core | Documentation debt cleanup, Agent Core MCP production, formal verification deepening |
| v0.1.12 | Runtime Hardening | Move VM boundary hardening, gas/payload interface formalization, config externalization, PQC (ML-DSA-65) integration |

## Maturation Phases

The v0.1.0–v0.1.12 progression followed four broad phases:

1. **Infrastructure** (v0.1.0–v0.1.1): Foundational crate layout, networking, consensus, storage
2. **Devnet Connectivity** (v0.1.2–v0.1.5): Security hardening, runtime correctness, evidence collection
3. **Economic & State Hardening** (v0.1.6–v0.1.9): Persistence, token economics, state proofs, staking
4. **Capability Closure** (v0.1.10–v0.1.12): Multi-shard execution, agent core, PQC crypto, Move VM hardening

## Document Boundary

- **This file** covers historical versions v0.1.0 through v0.1.12. It is a summary record, not a living specification.
- **Current version docs** (`*_v0.1.13.md` reports, guides, and ops runbooks) describe the active state of the system.
- Historical roadmaps and audit reports for each version are archived in the development repository under `Docs_Dev/`.
