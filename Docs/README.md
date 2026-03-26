# Nexus Documentation Portal

`Docs/` is the bilingual documentation root for the `v0.1.13` baseline.
Guide, ops, and report materials now live inside the language trees so that the
top level stays clean and predictable.

## Language Entry

- English portal: `Docs/en/README.md`
- 中文入口: `Docs/zh/README.md`

## Core Reports

- English version change report: `Docs/en/Report/Version_Change_Report_v0.1.13.md`
- English technical report: `Docs/en/Report/Technical_Report_v0.1.13.md`
- English coverage report: `Docs/en/Report/Coverage_Report_v0.1.13.md`
- English benchmark report: `Docs/en/Report/Benchmark_Report_v0.1.13.md`
- English devnet benchmark report: `Docs/en/Report/Benchmark/Devnet_Benchmark_Report_v0.1.13.md`

- 中文版本变更报告: `Docs/zh/Report/Version_Change_Report_v0.1.13.md`
- 中文技术报告: `Docs/zh/Report/Technical_Report_v0.1.13.md`
- 中文覆盖率报告: `Docs/zh/Report/Coverage_Report_v0.1.13.md`
- 中文基准测试报告: `Docs/zh/Report/Benchmark_Report_v0.1.13.md`
- 中文 devnet 基准测试报告: `Docs/zh/Report/Benchmark/Devnet_Benchmark_Report_v0.1.13.md`

## Main Reading Paths

### English

- `Docs/en/Guide/Local_Developer_Rehearsal_Guide.md`
- `Docs/en/Guide/Formal_Verification_Guide.md`
- `Docs/en/Guide/Nexus_Move_Dependency_Baseline.md`
- `Docs/en/Guide/External_Tool_Repositories.md`
- `Docs/en/Ops/Testnet_Operations_Guide.md`
- `Docs/en/Ops/Testnet_Release_Runbook.md`
- `Docs/en/Ops/Testnet_SLO.md`
- `Docs/en/Report/Agent_Core_MCP_Report.md`
- `Docs/en/Report/Proof_Trust_Model.md`
- `Docs/en/Report/Version_History_Summary.md`

### 中文

- `Docs/zh/Guide/Local_Developer_Rehearsal_Guide.md`
- `Docs/zh/Guide/Formal_Verification_Guide.md`
- `Docs/zh/Guide/Nexus_Move_Dependency_Baseline.md`
- `Docs/zh/Guide/External_Tool_Repositories.md`
- `Docs/zh/Ops/Testnet_Operations_Guide.md`
- `Docs/zh/Ops/Testnet_Release_Runbook.md`
- `Docs/zh/Ops/Testnet_SLO.md`
- `Docs/zh/Report/Agent_Core_MCP_Report.md`
- `Docs/zh/Report/Proof_Trust_Model.md`
- `Docs/zh/Report/Version_History_Summary.md`

## Current Baseline Notes

- Public API surface observed in code: REST, WebSocket, and MCP.
- The developer contract workflow is entered through `nexus-wallet move ...`.
- The shortest local path remains build -> setup-devnet -> compose up -> smoke tests.

```bash
rustup toolchain install 1.85.0
rustup override set 1.85.0

cargo build -p nexus-wallet
docker build -t nexus-node .
./scripts/setup-devnet.sh -o devnet -f
docker compose up -d
./scripts/smoke-test.sh
./scripts/contract-smoke-test.sh
```