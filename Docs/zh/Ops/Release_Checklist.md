# Nexus 版本发布核对清单

> **版本:** v0.1.13  
> **依据:** 当前 `v0.1.13` 发布与运维基线  
> **使用方式:** 每次版本发布前，逐项确认并签字

---

## 使用说明

1. 复制本文件为 `Release_Checklist_vX.Y.Z.md`（替换为实际版本号）。
2. 在每个检查项的 `[ ]` 中填入 `x` 表示已确认。
3. 所有项确认后方可执行发布。
4. 将完成的清单归档至 `Docs_Dev/` 目录。

---

## 阶段 1：代码准备

- [ ] **1.1** 所有目标 PR 已合并到 `main` 分支
- [ ] **1.2** `CHANGELOG.md` 或 `Docs_Dev/` 中的版本审计报告已更新
- [ ] **1.3** `Cargo.toml` 工作区版本号已更新（若需要）
- [ ] **1.4** `rust-toolchain.toml` 中的 Rust 版本无变更或已同步

---

## 阶段 2：CI 全量门禁

| 门禁 | 状态 | 签字 |
|------|------|------|
| **2.1** Lint (fmt + clippy + machete) | [ ] 通过 | |
| **2.2** Security Audit (cargo-audit + cargo-deny) | [ ] 通过 | |
| **2.2a** 已依据 `Docs/Report/BACKLOG.md` 复核临时 cargo-audit 豁免 | [ ] 通过 | |
| **2.3** Tests (cargo nextest + doctest) | [ ] 通过 | |
| **2.4** Correctness-Negative (Phase A–F) | [ ] 通过 | |
| **2.5** Startup-Readiness Gate | [ ] 通过 | |
| **2.6** Epoch-Reconfig Gate | [ ] 通过 | |
| **2.7** Proof-Surface Gate | [ ] 通过 | |
| **2.8** Recovery Tests | [ ] 通过 | |
| **2.9** Coverage (≥ 阈值) | [ ] 通过 | |
| **2.10** Crypto KAT (known-answer vectors) | [ ] 通过 | |
| **2.11** Workspace Check (feature-flag matrix) | [ ] 通过 | |
| **2.12** Move VM + Devnet Smoke | [ ] 通过 | |
| **2.13** Capacity Curves Gate (D-2) | [ ] 通过 | |
| **2.14** Bilingual Config-Doc Drift Check (E-1) | [ ] 通过 | |
| **2.15** Gas Calibration Gate (D-1) | [ ] 通过 | |
| **2.16** Release Go/No-Go (E-2) | [ ] 通过 | |
| **2.17** Exclusion Proof Tests (L-phase) | [ ] 通过 | |
| **2.18** Commitment Recovery Tests (M-phase) | [ ] 通过 | |
| **2.19** Canonical Root Tests (N-phase) | [ ] 通过 | |
| **2.20** Backup Tree & Determinism (N-phase) | [ ] 通过 | |
| **2.21** Staking Rotation Tests (R-phase) | [ ] 通过 | |
| **2.22** Cross-Node Election Determinism (S-phase) | [ ] 通过 | |
| **2.23** Staking Failure/Fallback Tests (S-phase) | [ ] 通过 | |
| **2.24** Staking Release Regression (S-phase) | [ ] 通过 | |
| **2.25** Multi-Shard Core Tests (W-phase) | [ ] 通过 | |
| **2.26** HTLC Lifecycle Tests (W-phase) | [ ] 通过 | |
| **2.27** Cross-Shard Determinism Tests (X-3) | [ ] 通过 | |
| **2.28** Shard Failure Rollback Tests (X-4) | [ ] 通过 | |
| **2.29** Release Regression Tests (X-5) | [ ] 通过 | |
| **2.30** Agent Core Unit Tests (ACE engine/session/planner/policy) | [ ] 通过 | |
| **2.31** Agent Core E2E (MCP → ACE → intent → execution) | [ ] 通过 | |
| **2.32** Differential Corpus (18 份语料全部通过) | [ ] 通过 | |
| **2.33** Property Tests Multi-Shard (多分片确定性 + HTLC 原子性) | [ ] 通过 | |
| **2.34** Doc Drift Check (全部 Ops 文档版本头 ≥ v0.1.13) | [ ] 通过 | |
| **2.35** BACKLOG Re-Audit (49 项全部有明确状态标记) | [ ] 通过 | |

---

## 阶段 3：文档一致性

- [ ] **3.1** `Docs/en/Ops/Testnet_Access_Policy.md` 与 `Docs/zh/Ops/Testnet_Access_Policy.md` 均与 `RpcConfig` 默认值一致
- [ ] **3.2** `Docs/zh/Ops/Capacity_Calibration_Reference.md` 校准数据为最新
- [ ] **3.3** `Docs/zh/Ops/Testnet_Release_Runbook.md` 步骤与实际流程一致
- [ ] **3.4** `Docs/zh/Ops/Epoch_Operations_Runbook.md` 已审阅
- [ ] **3.5** `Docs/zh/Report/Proof_Trust_Model.md` 覆盖当前 proof 能力（含排除证明邻叶 witness）
- [ ] **3.6** `Docs/zh/Ops/Testnet_SLO.md` SLO 指标和阈值已确认
- [ ] **3.7** `Docs_Dev/issues.md` 中所有阻塞项已关闭或豁免
- [ ] **3.8** `Docs/zh/Ops/Schema_Migration_Guide.md` 已确认 schema v3 迁移步骤
- [ ] **3.9** `Docs/zh/Ops/Staking_Rotation_Runbook.md` 已审阅（v0.1.9 新增）
- [ ] **3.10** `Docs/zh/Ops/Testnet_Operations_Guide.md` §3.4a 多分片 devnet 配置已审阅（v0.1.10 新增）

---

## 阶段 4：Staging 部署验证

- [ ] **4.1** Docker 镜像构建成功
  ```bash
  make devnet-build
  ```
- [ ] **4.2** Staging 环境部署完成
- [ ] **4.3** `scripts/validate-startup.sh -n 7 -t 90` 通过
- [ ] **4.4** `scripts/smoke-test.sh` 22/22 场景通过
- [ ] **4.5** `scripts/contract-smoke-test.sh` 6/6 阶段通过
- [ ] **4.6** Soak test 完成（若执行）

---

## 阶段 5：Go / No-Go 审查

- [ ] **5.1** `scripts/release-go-nogo.sh --json` 所有 30 项通过
- [ ] **5.2** 至少一名工程师审查 go/no-go 输出
- [ ] **5.3** 配额策略与预期容量一致
- [ ] **5.4** 决定：**GO** / **NO-GO**

---

## 阶段 6：发布执行

- [ ] **6.1** 创建 Git Tag
  ```bash
  git tag vX.Y.Z
  git push origin vX.Y.Z
  ```
- [ ] **6.2** GHCR 镜像标记
  ```bash
  docker tag nexus-node:latest ghcr.io/<owner>/nexus-node:vX.Y.Z
  docker tag nexus-node:latest ghcr.io/<owner>/nexus-node:public-testnet-latest
  docker push ghcr.io/<owner>/nexus-node:vX.Y.Z
  docker push ghcr.io/<owner>/nexus-node:public-testnet-latest
  ```
- [ ] **6.3** GitHub Release 创建（包含二进制 artifacts）
- [ ] **6.4** 公开 testnet 部署完成
- [ ] **6.5** 部署后验证通过（`/ready`、`/health`、`/v2/network/health`）

---

## 阶段 7：发布后监控

- [ ] **7.1** 发布后 1 小时内监控以下指标无异常：
  - RPC 请求成功率
  - 共识提交率（`total_commits` 持续增长）
  - 错误率 < SLO 阈值
  - 内存和 CPU 使用稳定
- [ ] **7.2** 发布通知已发送给相关方
- [ ] **7.3** 归档：将本清单副本存入 `Docs_Dev/Release_Checklist_vX.Y.Z.md`

---

## 阶段 8：异常回滚（仅在 No-Go 或严重问题时）

- [ ] **8.1** 执行回滚
  ```bash
  docker tag ghcr.io/<owner>/nexus-node:<previous-tag> ghcr.io/<owner>/nexus-node:public-testnet-latest
  docker push ghcr.io/<owner>/nexus-node:public-testnet-latest
  ```
- [ ] **8.2** 重部署上一版本
- [ ] **8.3** 验证回滚后网络恢复
- [ ] **8.4** 记录回滚原因和后续行动

---

## 签字

| 角色 | 姓名 | 日期 | 签字 |
|------|------|------|------|
| 发布工程师 | | | |
| 代码审查人 | | | |
| 运维确认人 | | | |
