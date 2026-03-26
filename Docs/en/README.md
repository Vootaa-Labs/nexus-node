# Nexus Documentation Portal

This is the English entry for the `v0.1.13` documentation baseline.
It is rebuilt from the current code, tests, scripts, and workflows instead of
depending on legacy repository history.

## Recommended Reading Order

### 1. First-time orientation

- Start with the repository root `README.md`.
- Continue with `Docs/en/Report/Version_Change_Report_v0.1.13.md`.
- Then read `Docs/en/Report/Technical_Report_v0.1.13.md`.
- Use `Docs/en/Report/Coverage_Report_v0.1.13.md` for the current first-party test coverage baseline.
- Use `Docs/en/Report/Benchmark_Report_v0.1.13.md` for the current first-party Criterion benchmark baseline.
- Use `Docs/en/Report/Benchmark/Devnet_Benchmark_Report_v0.1.13.md` for the current multi-node devnet TPS and latency sweep.

### 2. Role-based paths

#### Product stakeholders and partners

- `Docs/en/Report/Version_Change_Report_v0.1.13.md`
- `Docs/en/Report/Technical_Report_v0.1.13.md`

#### Rust developers and architects

- `Docs/en/Report/Technical_Report_v0.1.13.md`
- `Docs/en/Guide/Local_Developer_Rehearsal_Guide.md`
- `Docs/en/Guide/Formal_Verification_Guide.md`
- `Docs/en/Report/Benchmark_Report_v0.1.13.md`
- `Docs/en/Report/Benchmark/Devnet_Benchmark_Report_v0.1.13.md`

#### Operators, release owners, and testnet maintainers

- `Docs/en/Ops/Testnet_Operations_Guide.md`
- `Docs/en/Ops/Testnet_Release_Runbook.md`
- `Docs/en/Ops/Testnet_SLO.md`
- `Docs/en/Ops/Staking_Rotation_Runbook.md`
- `Docs/en/Ops/Schema_Migration_Guide.md`
- `Docs/en/Ops/Testnet_Access_Policy.md`

#### Auditors, testers, and verification engineers

- `Docs/en/Report/Technical_Report_v0.1.13.md`
- `Docs/en/Guide/Formal_Verification_Guide.md`
- `Docs/en/Report/Proof_Trust_Model.md`
- `Docs/en/Report/Coverage_Report_v0.1.13.md`
- `Docs/en/Report/Benchmark_Report_v0.1.13.md`
- `Docs/en/Report/Benchmark/Devnet_Benchmark_Report_v0.1.13.md`

## Content Layout

- `Docs/en/Guide/`: developer and verification guides
- `Docs/en/Ops/`: deployment, release, SLO, capacity, schema, and rotation runbooks
- `Docs/en/Report/`: focused reports aligned to the current codebase
- `Docs/en/Report/Benchmark/`: devnet multi-node TPS and latency sweep variants

## Current Baseline Highlights

- The active external surfaces are REST, WebSocket, and MCP.
- The developer contract CLI entry is `nexus-wallet move ...`.
- The shortest reproducible local path remains build -> setup-devnet -> compose up -> smoke tests.