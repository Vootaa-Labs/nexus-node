# 容量校准参考 — Query / Intent / MCP 配额与 Gas Budget

> **版本:** v0.1.13  
> **依据:** 当前仓库 `v0.1.13` 基线的 Phase D-3 能力面  
> **校准来源:** `gas_calibration.rs` (18 测试)、`middleware.rs` D-2 测试套件 (8 测试)、`capacity_matrix.rs` 验证

---

## 1. Gas Budget 校准结果

### 1.1 测试方法

使用 6 种代表性合约原型对 `query_gas_budget` (10M) 进行校准：

| 合约原型 | 操作类型 | 估算 Gas | 相对预算余量 |
|----------|---------|----------|-------------|
| Counter | increment + query | ~5,040 | 1,984× |
| Token | transfer + balance query | ~50,000 | 200× |
| Voting | create_proposal + vote + tally | ~150,000 | 66× |
| Multisig | submit + approve + execute | ~200,000 | 50× |
| Registry | register + lookup | ~80,000 | 125× |
| Worst-case (深度循环) | 100 次嵌套调用 | ~900,000 | 11× |

### 1.2 结论

- **10M gas budget** 为所有已知合约工作负载提供 11× 至 1,984× 的余量。
- 即使 worst-case 场景也有 11× headroom，足够覆盖公开 testnet 的合理使用。
- 不需要立即调整。

### 1.3 Timeout 校准

- **5,000 ms (5s)** timeout 覆盖所有合约原型的执行时间。
- worst-case 合约在标准硬件上执行时间 < 200ms。
- 保守余量足够，无需调整。

---

## 2. 配额矩阵 (Tier × Class)

### 2.1 当前默认值

以下默认值同时存在于 `RpcConfig` (代码)、`Docs/en/Ops/Testnet_Access_Policy.md` 与 `Docs/zh/Ops/Testnet_Access_Policy.md`。
`config-doc-drift-check.sh` (E-1) 持续验证代码与双语文档三方一致性。

| 端点类别 | 路径 | Anonymous | Authenticated | Whitelisted | 单位 |
|----------|------|-----------|---------------|-------------|------|
| **Query** | `/v2/contract/query` | 60 | 600 | 3,000 | rpm |
| **Intent** | `/v2/intent/submit`, `/v2/intent/estimate-gas` | 30 | 300 | 1,500 | rpm |
| **MCP** | `/v2/mcp/*` | 30 | 300 | 1,500 | rpm |

### 2.2 全局限制

| 参数 | 默认值 | 单位 |
|------|--------|------|
| `rate_limit_per_ip_rps` | 100 | 请求/秒 |
| `rate_limit_rps` | 1,000 | 请求/秒 (全局) |
| `max_ws_connections` | 10,000 | 连接 |
| `grpc_max_message_size` | 4 MiB | 字节 |
| `query_gas_budget` | 10,000,000 | gas units |
| `query_timeout_ms` | 5,000 | ms |

### 2.3 Tier 阶梯不变量

代码中由 `tier_hierarchy_invariant` 测试保证：

```
Anonymous < Authenticated < Whitelisted
```

对每个端点类别，低 tier 的限额严格小于高 tier。

### 2.4 跨类别独立性

每个类别的配额追踪完全独立：

- 耗尽 Query 配额不影响 Intent 或 MCP 配额。
- 由 `cross_class_independence_under_sustained_load` 测试验证。

### 2.5 Fail-Closed 行为

当配额管理器的 IP 追踪表达到容量上限时：

- 新 IP 的请求被拒绝（返回 429）。
- 不会静默放行。
- 由 `quota_manager_fail_closed_at_capacity` 测试验证。

---

## 3. Faucet 配额

| 参数 | 默认值 |
|------|--------|
| `faucet_enabled` | `false` |
| `faucet_amount` | 10⁹ voo (1 NXS) |
| `faucet_per_addr_limit_per_hour` | 10 |

- Faucet 默认关闭，需显式启用。
- 地址追踪表容量 100,000 条；达到上限时 fail-closed。

---

## 4. 配置同步清单

修改配额/预算参数时，以下位置必须同步更新：

| # | 位置 | 文件 |
|---|------|------|
| 1 | 代码默认值 | `crates/nexus-config/src/rpc.rs` → `RpcConfig::default()` |
| 2 | 运维策略文档 | `Docs/en/Ops/Testnet_Access_Policy.md` 与 `Docs/zh/Ops/Testnet_Access_Policy.md` → §3 + §7 |
| 3 | 校准参考文档 | `Docs/zh/Ops/Capacity_Calibration_Reference.md` (本文件) |
| 4 | 漂移检查脚本 | `scripts/config-doc-drift-check.sh` |
| 5 | CI 门禁 | `.github/workflows/ci.yml` → Config-Doc Drift Check |

`config-doc-drift-check.sh` 会自动检测 #1 和 #2 的偏差，并要求中英文访问策略文档彼此一致，在 CI 中作为阻断门禁。

---

## 5. 多分片与 HTLC 场景校准（v0.1.10+）

### 5.1 多分片 Gas 消耗

多分片执行下，交易 gas 消耗与单分片一致（每个分片独立计量），但跨分片 HTLC 操作引入额外开销：

| 操作 | 估算 Gas | 说明 |
|------|----------|------|
| HTLC Lock（单分片） | ~100,000 | 创建锁定条目并写入 `cf_htlc_locks` |
| HTLC Claim（跨分片） | ~150,000 | 验证 preimage + 跨分片状态写入 |
| HTLC Refund（超时） | ~80,000 | 超时后退款到源地址 |

当前 10M gas budget 对 HTLC 操作提供 66×–125× headroom，无需调整。

### 5.2 多分片容量影响

- 每增加一个分片，节点的存储 I/O 和网络带宽线性增长。
- 当前 devnet 默认 2 分片；公开 testnet 建议不超过 4 分片。
- 分片数量变更需重新执行 `setup-devnet.sh` 生成 genesis。

### 5.3 HTLC 超时配置

HTLC 超时窗口默认为 100 个区块（约 5 分钟）。运维人员应确保超时窗口大于最大网络延迟。

---

## 6. 校准建议（面向公开 testnet）

| 建议 | 依据 |
|------|------|
| 保持当前 gas budget (10M) | worst-case 合约有 11× headroom |
| 保持当前 timeout (5s) | 实测执行时间远低于阈值 |
| Anonymous query 60 rpm 可降至 30 | 若出现滥用模式 |
| Whitelisted 上限3000可提至5000 | 若合作方需更高吞吐 |
| Intent/MCP 保持对称 | 当前无差异化需求 |

任何调整必须经过完整的 D-2 容量测试套件验证后才能合并。

---

## 7. 相关自动化检查

| 检查 | 位置 |
|------|------|
| Config ↔ Access Policy 漂移 | `scripts/config-doc-drift-check.sh` (E-1) |
| 容量曲线边界 | `middleware.rs` D-2 测试 (8 个) |
| Gas 校准 | `gas_calibration.rs` (18 个) |
| Release go/no-go | `scripts/release-go-nogo.sh` (E-2) |
