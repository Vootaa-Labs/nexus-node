# Nexus 文档门户

这是 `v0.1.13` 基线的中文文档入口。
文档内容以当前代码、测试、脚本和工作流为依据重建，不依赖旧仓库历史。

## 建议阅读顺序

### 1. 首次了解项目

- 先看仓库根目录 `README.md`
- 再看 `Docs/zh/Report/Version_Change_Report_v0.1.13.md`
- 然后看 `Docs/zh/Report/Technical_Report_v0.1.13.md`
- 如需查看当前 first-party 测试覆盖率基线，请阅读 `Docs/zh/Report/Coverage_Report_v0.1.13.md`
- 如需查看当前 first-party Criterion 基准测试基线，请阅读 `Docs/zh/Report/Benchmark_Report_v0.1.13.md`
- 如需查看当前多节点 devnet TPS 与延迟 sweep，请阅读 `Docs/zh/Report/Benchmark/Devnet_Benchmark_Report_v0.1.13.md`

### 2. 按角色阅读

#### 产品、项目与合作方

- `Docs/zh/Report/Version_Change_Report_v0.1.13.md`
- `Docs/zh/Report/Technical_Report_v0.1.13.md`

#### Rust 开发者与架构师

- `Docs/zh/Report/Technical_Report_v0.1.13.md`
- `Docs/zh/Guide/Local_Developer_Rehearsal_Guide.md`
- `Docs/zh/Guide/Formal_Verification_Guide.md`
- `Docs/zh/Report/Benchmark_Report_v0.1.13.md`
- `Docs/zh/Report/Benchmark/Devnet_Benchmark_Report_v0.1.13.md`

#### 运维、测试网与发布人员

- `Docs/zh/Ops/Testnet_Operations_Guide.md`
- `Docs/zh/Ops/Testnet_Release_Runbook.md`
- `Docs/zh/Ops/Testnet_SLO.md`
- `Docs/zh/Ops/Staking_Rotation_Runbook.md`
- `Docs/zh/Ops/Schema_Migration_Guide.md`
- `Docs/zh/Ops/Testnet_Access_Policy.md`

#### 审计、测试与验证人员

- `Docs/zh/Report/Technical_Report_v0.1.13.md`
- `Docs/zh/Guide/Formal_Verification_Guide.md`
- `Docs/zh/Report/Proof_Trust_Model.md`
- `Docs/zh/Report/Coverage_Report_v0.1.13.md`
- `Docs/zh/Report/Benchmark_Report_v0.1.13.md`
- `Docs/zh/Report/Benchmark/Devnet_Benchmark_Report_v0.1.13.md`

## 目录说明

- `Docs/zh/Guide/`: 开发与形式化验证指南
- `Docs/zh/Ops/`: 部署、发布、容量、SLO、schema 与 staking 运维手册
- `Docs/zh/Report/`: 基于当前代码现状整理的专题报告
- `Docs/zh/Report/Benchmark/`: devnet 多节点 TPS 与延迟 sweep 变体

## 当前基线说明

- 当前公开接口面为 REST、WebSocket、MCP。
- 合约相关开发入口统一为 `nexus-wallet move ...`。
- 最短本地演练路径仍是 build -> setup-devnet -> compose up -> smoke tests。