# Nexus 测试网 SLO / 错误预算 / 回滚阈值

> **版本:** v0.1.13  
> **受众:** 节点运维、值班工程师、发布决策者  
> **依据:** 当前仓库 `v0.1.13` 基线的 Phase E-4 目标

---

## 1. 概述

本文定义 Nexus 公开测试网的服务水平目标 (SLO)、错误预算和触发回滚的量化阈值。
目标不是承诺生产级可用性，而是为测试网运营提供可量化的健康基线和决策依据。

---

## 2. SLO 定义

### 2.1 可用性 SLO

| SLI | 定义 | 测量方式 | SLO 目标 |
|-----|------|---------|---------|
| **API 可用性** | `/ready` 返回 200 的时间占比 | 每 30s 从外部探针采样 | ≥ 99.0% (7d 滚动窗口) |
| **共识活跃度** | `total_commits` 在 60s 内递增 | 每 30s 检查 `/v2/consensus/status` | ≥ 98.0% (7d 滚动窗口) |
| **节点参与率** | 处于 ready 状态的节点占比 | `count(ready=200) / total_nodes` | ≥ 71% (即 5/7) |

### 2.2 延迟 SLO

| SLI | 定义 | 测量方式 | SLO 目标 |
|-----|------|---------|---------|
| **读查询 P50** | `/v2/contract/query` 延迟中位数 | Prometheus `proof_request_duration_seconds` | ≤ 100ms |
| **读查询 P99** | `/v2/contract/query` 延迟 99th 百分位 | 同上 | ≤ 1,000ms |
| **Proof 请求 P50** | `/v2/state/proof` 延迟中位数 | `proof_request_duration_seconds{endpoint="proof"}` | ≤ 50ms |
| **Proof 请求 P99** | 同上 99th 百分位 | 同上 | ≤ 500ms |

### 2.3 正确性 SLO

| SLI | 定义 | 测量方式 | SLO 目标 |
|-----|------|---------|---------|
| **Epoch 一致性** | 所有 ready 节点的 epoch 值一致 | 对比 N 个节点的 `/v2/consensus/epoch` | 100% |
| **State root 一致性** | 所有 ready 节点的 state root 匹配 | 对比 `/v2/chain/head` → `state_root` | 100% |
| **RPC 错误率** | 500 状态码占总请求比例 | `rpc_requests_total{status="500"}` / `rpc_requests_total` | ≤ 0.1% |

---

## 3. 错误预算

错误预算定义了在 SLO 达标前提下允许的累计不可用时间。

### 3.1 预算计算（7 天滚动窗口）

| SLO | 允许停机 | 计算 |
|-----|---------|------|
| 99.0% API 可用性 | **100.8 分钟 / 7 天** | 7 × 24 × 60 × 0.01 |
| 98.0% 共识活跃度 | **201.6 分钟 / 7 天** | 7 × 24 × 60 × 0.02 |

### 3.2 预算消耗规则

- **正常运维窗口**（配置变更、升级）消耗错误预算。
- **计划内维护**应提前通知并在低谷时段执行。
- **当剩余预算 ≤ 25%** 时：
  - 冻结非关键变更。
  - 仅允许紧急修复上线。
  - 值班人持续监控至预算恢复。

### 3.3 预算耗尽时的处理

若 7 天窗口内错误预算耗尽：

1. 暂停所有版本发布和配置变更。
2. 启动事后回顾 (postmortem)。
3. 制定改进措施并验证后方可恢复正常发布节奏。

---

## 4. 回滚触发阈值

以下任一条件满足时，应立即执行回滚：

### 4.1 强制回滚条件

| # | 条件 | 阈值 | 检测方式 |
|---|------|------|---------|
| R1 | API 不可用 | 连续 > 5 分钟所有节点 `/ready` 非 200 | 外部探针或 `nexus-monitor` |
| R2 | 共识停摆 | `total_commits` 连续 10 分钟无增长 | `/v2/consensus/status` |
| R3 | 数据不一致 | ≥ 2 个 ready 节点 state root 不同 | `/v2/chain/head` 对比 |
| R4 | 五百错误激增 | 500 错误率 > 5% 持续 5 分钟 | Prometheus `rpc_requests_total` |
| R5 | 节点大面积宕机 | ready 节点 < 2f+1 = 5（7 节点网络）| `/ready` 探针 |

### 4.2 建议回滚条件

| # | 条件 | 阈值 | 说明 |
|---|------|------|------|
| R6 | 延迟退化 | P99 > 5s 持续 15 分钟 | 值班人判断是否回滚 |
| R7 | 内存泄漏 | 单节点 RSS > 4 GiB 且持续增长 | Docker stats / Prometheus |
| R8 | 错误预算消耗过快 | 24h 内消耗 > 50% 7d 预算 | 预算追踪 |

### 4.3 回滚流程

1. **确认** — 至少两名工程师确认触发条件成立。
2. **通知** — 通知相关方回滚即将执行。
3. **执行** — 按 `Testnet_Release_Runbook.md` §7 回滚步骤操作。
4. **验证** — 确认回滚后所有 SLO 恢复达标。
5. **记录** — 归档事件时间线、根因和改进措施。

---

## 5. 监控与告警

### 5.1 推荐 Prometheus 指标

| 指标 | 类型 | 用途 |
|------|------|------|
| `rpc_requests_total` | Counter | API 吞吐量和错误率 |
| `rpc_request_duration_seconds` | Histogram | 延迟 SLO |
| `proof_requests_total` | Counter | Proof 端点成功率 |
| `proof_request_duration_seconds` | Histogram | Proof 延迟 SLO |
| `proof_commitment_queries_total` | Counter | Commitment 查询量 |
| `rpc_rate_limited_total` | Counter | 限流触发频率 |
| `rpc_active_connections` | Gauge | WebSocket 连接数 |

### 5.2 告警规则（建议）

```yaml
# API 可用性告警
- alert: NexusAPIDown
  expr: up{job="nexus-node"} == 0
  for: 2m
  labels:
    severity: critical

# 共识停摆告警
- alert: NexusConsensusStalled
  expr: increase(nexus_total_commits[5m]) == 0
  for: 5m
  labels:
    severity: critical

# 500 错误率告警
- alert: NexusHighErrorRate
  expr: >
    rate(rpc_requests_total{status="500"}[5m])
    / rate(rpc_requests_total[5m]) > 0.05
  for: 5m
  labels:
    severity: warning

# P99 延迟告警
- alert: NexusHighLatency
  expr: >
    histogram_quantile(0.99,
      rate(rpc_request_duration_seconds_bucket[5m])) > 5
  for: 10m
  labels:
    severity: warning
```

### 5.3 `nexus-monitor` 集成

`tools/nexus-monitor` TUI 已收集以下与 SLO 相关的数据：

| TUI 视图 | 相关 SLI |
|----------|---------|
| F1 Chain Dashboard | 共识活跃度、finality lag |
| F2 Node Dashboard | 节点可用性 |
| F4 Consensus | Epoch 一致性、commit rate |
| F7 Validators | 验证人健康状态 |
| F9 RPC | 请求吞吐和错误率 |
| F10 Metrics | Prometheus 原始指标 |

---

## 6. SLO 审查周期

| 时间 | 动作 |
|------|------|
| **每次发布后 24h** | 确认所有 SLO 达标 |
| **每周** | 审查 7 天错误预算消耗 |
| **每次事故后** | 审查 SLO 是否需要调整 |
| **每个大版本** | 全面审查 SLO 是否反映实际运营需求 |

---

## 7. 版本演进

| 版本 | SLO 变化 |
|------|---------|
| v0.1.5 | 初始 SLO 定义；测试网级别 |
| v0.1.7 | Stake-weighted quorum：验证人参与率 SLI 按 stake 权重加权 |
| v0.1.8 | State commitment：新增 Proof 请求延迟 SLI (P50 ≤50ms, P99 ≤500ms) |
| v0.1.9 | Staking-driven rotation：新增 epoch 一致性 SLI 100% |
| v0.1.10 | 多分片执行：新增分片健康 SLI（下方 §7.1） |
| v0.1.13 | 文档结构调整为中英双语并对齐当前代码基线 |
| 主网 | SLO 需升级至 99.9%+，且需外部审计 |

### 7.1 多分片 SLO 扩展（v0.1.10+）

| SLI | 定义 | 测量方式 | SLO 目标 |
|-----|------|---------|---------|
| **分片健康度** | 所有分片 head 高度差 ≤ 5 | 对比 `/v1/shards/{id}/head` 各分片高度 | 100%（7d 滚动窗口） |
| **跨分片 HTLC 成功率** | HTLC lock→claim 成功占 HTLC lock 总量 | 链上 HTLC 状态统计 | ≥ 95%（排除用户主动超时退款） |
| **全局 State Root 一致性** | 多分片聚合 state root 所有节点一致 | 对比 N 个节点的 `/v1/status` → `state_root` | 100% |

---

## 附录：SLO 参数快速查表

| 参数 | 值 |
|------|-----|
| API 可用性 SLO | ≥ 99.0% / 7d |
| 共识活跃度 SLO | ≥ 98.0% / 7d |
| 节点参与率 SLO | ≥ 71% |
| 读查询 P99 | ≤ 1,000ms |
| Proof P99 | ≤ 500ms |
| RPC 错误率 | ≤ 0.1% |
| 允许 API 停机 | ≤ 100.8 min / 7d |
| 允许共识停摆 | ≤ 201.6 min / 7d |
| 强制回滚：连续不可用 | > 5 min |
| 强制回滚：共识停摆 | > 10 min |
| 强制回滚：500 错误率 | > 5% × 5 min |
