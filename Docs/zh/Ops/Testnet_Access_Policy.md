# Nexus 公开测试网访问与滥用处置策略

_版本 0.1.15_

---

## 1. 概述

本文定义 Nexus 公开 devnet/testnet RPC 端点的访问规则，包括速率限制、认证分层、faucet 策略与滥用处置流程。

---

## 2. 访问分层

| 分层 | 识别方式 | 说明 |
|------|----------|------|
| **匿名** | 无 `x-api-key` 请求头 | 所有调用方默认档位，配额最低。 |
| **已认证** | 携带合法 `x-api-key` | 已注册开发者，中档配额。 |
| **白名单** | `x-api-key` 位于白名单集合 | 可信合作方或审计方，配额最高。 |

当配置 API key 时，HTTP 请求头中的 `x-api-key` 至少需要 16 字节。若启用 API key，则启动时强制要求 TLS。

---

## 3. 速率限制

### 3.1 全局每 IP 限制

所有端点共享统一的每 IP 速率限制：

| 参数 | 默认值 |
|------|--------|
| Per-IP RPS | 100 |
| Window | 1 second |

超过全局限制时返回 **429 Too Many Requests**，并附带 `retry-after` 响应头。

### 3.2 按端点类别配额 (E-2)

高计算成本端点按类别和调用方分层分别限流，单位为每分钟请求数：

| 端点类别 | 路径 | Anonymous | Authenticated | Whitelisted |
|----------|------|-----------|---------------|-------------|
| **Query** | `/v2/contract/query` | 60 rpm | 600 rpm | 3 000 rpm |
| **Intent** | `/v2/intent/submit`, `/v2/intent/estimate-gas` | 30 rpm | 300 rpm | 1 500 rpm |
| **MCP** | `/v2/mcp/*` | 30 rpm | 300 rpm | 1 500 rpm |

每个类别都**独立计数**，耗尽 query 配额不会影响 intent 或 MCP 配额。

配额保护端点的响应头包括：

- `x-quota-tier` — 解析出的调用方分层
- `x-quota-class` — 端点类别（query / intent / mcp）
- `x-quota-remaining` — 当前窗口剩余令牌数

### 3.3 Query gas 预算

只读 view 查询（`/v2/contract/query`）受 gas 预算限制：

| 参数 | 默认值 |
|------|--------|
| Gas budget | 10 000 000 units |
| Timeout | 5 000 ms |

超过 gas 预算或超时的查询分别返回 **400** 或 **503**。响应中会携带 `gas_used` 与 `gas_budget` 字段，便于观测与审计。

---

## 4. Faucet 策略

`/v2/faucet/mint` 端点用于发放测试网开发代币。

| 参数 | 默认值 |
|------|--------|
| Enabled | `false`（必须显式开启） |
| Amount per request | 10⁹ voo (1 NXS) |
| Per-address limit | 10 requests per hour |

- 每个地址独立计数。
- 当地址追踪表达到上限（100 000 条）时，新地址请求按 fail-closed 策略拒绝，直到旧条目过期。

---

## 5. 审计日志

所有请求都会以结构化 JSON 记录到 `nexus::audit` tracing target。记录字段包括：

| 字段 | 说明 |
|------|------|
| `method` | HTTP 方法 |
| `path` | 请求 URI 路径 |
| `status` | HTTP 响应码 |
| `latency_ms` | 端到端时延 |
| `ip` | 客户端 IP（来自 TCP peer，而非 `X-Forwarded-For`） |
| `request_id` | 唯一 `x-request-id` 头 |
| `tier` | 解析后的配额分层 |
| `endpoint_class` | 端点类别（query / intent / mcp） |

合约 query handler 还会追加 `gas_used`、`gas_budget` 与 `elapsed_ms` 等 gas 指标。

---

## 6. 滥用检测与响应

### 6.1 自动化保护

- **Rate limiting** — 每 IP token-bucket，并在异常状态下 fail-closed。
- **Quota tiering** — 高成本端点对匿名流量采用更严格配额。
- **Gas budget** — 防止 view 查询消耗无限资源。
- **Timeout** — 超长查询在截止时间后终止。
- **Body size limit** — 请求体上限 512 KiB。
- **API key authentication** — 启用后，变更类端点（POST）必须携带合法 key。

### 6.2 人工响应流程

当自动化保护不足时：

1. **识别** — 检查 `nexus::audit` 日志中的异常模式，如高请求率、轮换 IP 的重复 429、gas 预算违规等。
2. **阻断** — 在反向代理或防火墙层封禁来源 IP，而不是直接在应用内操作，以获得即时效果。
3. **吊销** — 从 `api_keys` 配置中移除受损 key，并重启节点生效。
4. **升级** — 若攻击针对协议级资源（如存储放大），需要通知核心团队评估更深层的缓解措施。

### 6.3 事后复盘

发生滥用事件后：

- 归档覆盖事件时间窗的审计日志。
- 评估是否需要收紧自动化限额。
- 若发现新的攻击路径，更新本文档。

---

## 7. 配置参考

所有参数都位于 `nexus-config` 的 `RpcConfig` 中，可通过节点配置文件或环境变量覆盖。

| Field | Type | Default | Notes |
|-------|------|---------|-------|
| `rate_limit_per_ip_rps` | u32 | 100 | Global per-IP RPS |
| `query_rate_limit_anonymous_rpm` | u32 | 60 | Query class, anonymous |
| `query_rate_limit_authenticated_rpm` | u32 | 600 | Query class, authenticated |
| `query_rate_limit_whitelisted_rpm` | u32 | 3 000 | Query class, whitelisted |
| `intent_rate_limit_anonymous_rpm` | u32 | 30 | Intent class, anonymous |
| `intent_rate_limit_authenticated_rpm` | u32 | 300 | Intent class, authenticated |
| `intent_rate_limit_whitelisted_rpm` | u32 | 1 500 | Intent class, whitelisted |
| `mcp_rate_limit_anonymous_rpm` | u32 | 30 | MCP class, anonymous |
| `mcp_rate_limit_authenticated_rpm` | u32 | 300 | MCP class, authenticated |
| `mcp_rate_limit_whitelisted_rpm` | u32 | 1 500 | MCP class, whitelisted |
| `query_gas_budget` | u64 | 10 000 000 | Max gas per view query |
| `query_timeout_ms` | u64 | 5 000 | View query timeout |
| `faucet_enabled` | bool | false | Enable faucet endpoint |
| `faucet_amount` | u64 | 10⁹ voo | Tokens per faucet request |
| `faucet_per_addr_limit_per_hour` | u32 | 10 | Faucet per-address hourly limit |
| `api_keys` | Vec | [] | Valid API keys (min 16 bytes each) |
| `whitelisted_api_keys` | Vec | [] | Whitelisted-tier keys (subset of api_keys) |
| `cors_allowed_origins` | Vec | [] | Fail-closed when empty |

---

## 8. 变更管理

每次发版前至少核对以下内容：

- `Docs/en/Ops/Testnet_Release_Runbook.md`
- `Docs/zh/Ops/Testnet_Release_Runbook.md`
- `Docs/en/Ops/Capacity_Calibration_Reference.md`
- `Docs/zh/Ops/Capacity_Calibration_Reference.md`
- `Docs/en/Ops/Testnet_SLO.md`
- `Docs/zh/Ops/Testnet_SLO.md`

若配额或认证边界发生变化，必须先同步更新中英文访问策略文档，再更新发布清单和 CI 校验说明。
