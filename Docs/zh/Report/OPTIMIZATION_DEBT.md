# Nexus 优化债务清单（v0.1.11 重审版）

> 重审日期：2026-03-22
> 基线版本：v0.1.10
> 重审依据：v0.1.11 路线图 Phase Y-2 — 关闭 OPT-003/004，更新 OPT-002 进度

## 1. 文档定位

本文件跟踪不阻塞发布但影响代码质量的优化债务。每项债务需有当前源码中的直接证据才保留。v0.1.7–v0.1.10 多个版本已消化了大量债务，本次重审更新全部状态。

## 2. 状态总览

| 编号 | 事项 | 状态 | 关闭/更新版本 |
|------|------|------|-------------|
| OPT-001 | Block-STM 共享 MVCC 数据结构 | ✅ Closed | v0.1.7 |
| OPT-002 | 过长参数链与桥接上下文收敛 | ✅ Closed | v0.1.11 Phase D |
| OPT-003 | WebSocket 事件覆盖收敛 | ✅ Closed | v0.1.10 |
| OPT-004 | CLI 客户端重复实现收敛 | ✅ Closed | v0.1.7 |

---

## 3. 已关闭债务

### OPT-001 Block-STM 共享 MVCC 数据结构 — ✅ Closed (v0.1.7)

- **关闭原因**：当前执行层已存在 `mvhashmap.rs` 等共享状态结构，旧问题描述已明显滞后于实现。

### OPT-003 WebSocket 事件覆盖收敛 — ✅ Closed (v0.1.10)

- **关闭原因**：四类 `NodeEvent` 现在均有完整发送路径：
  - `NewCommit` — `execution_bridge.rs` 提交后发送
  - `TransactionExecuted` — `execution_bridge.rs` 执行后发送
  - `ConsensusStatus` — `execution_bridge.rs` 共识状态变更时发送（v0.1.10 D-2 补齐）
  - `IntentStatusChanged` — `intent_watcher.rs` 监听 intent 状态变更后发送；REST endpoint 也会在提交/失败时发送
- **代码证据**：
  - `crates/nexus-node/src/intent_watcher.rs`: L107-L125 `IntentStatusChanged` 事件发送
  - `crates/nexus-node/src/execution_bridge.rs`: `ConsensusStatus` 发送
  - `crates/nexus-rpc/src/rest/intent.rs`: L69-L80 补充发送路径
  - intent_watcher 含完整单元测试验证事件发射

### OPT-004 CLI 客户端重复实现收敛 — ✅ Closed (v0.1.7)

- **关闭原因**：`nexus-wallet move ...` 已成为唯一开发者 CLI 入口。旧独立 Move CLI workspace 二进制已移除。实现收口到 `tools/nexus-wallet/src/move_tooling/`。
- **残余**：`tools/nexus-wallet/src/move_tooling/rpc_client.rs` 仍有独立 RPC 客户端实现（对应 BACKLOG BL-016），但这不构成"双份 CLI"问题，属低优先级代码整洁事项。

---

## 4. 仍开放债务

所有 OPT 项均已关闭。无仍开放债务。

### OPT-002 过长参数链与桥接上下文收敛 — ✅ Closed (v0.1.11)

#### 改善历程

- v0.1.10 Phase D-1：引入 `BridgeContext<S>` + `EpochContext`，将 `execution_bridge` 主入口从 14 参数收敛到 4 参数。
- v0.1.11 Phase D-1/D-2：完成全部残余收敛。

#### v0.1.11 收敛措施

| 文件 | 措施 | 效果 |
|------|------|------|
| `executor.rs` | 引入 `HtlcExecContext` 结构体 | 3 处 `#[allow]` → 0 |
| `move_adapter/mod.rs` | 移除不必要的 allow（参数数 ≤ 7） | 3 处 → 0 |
| `cert_aggregator.rs` | 引入 `CertAggContext` 结构体 | 3 处 → 0 |
| `execution_bridge.rs` | `process_committed_batch` / `execute_resolved_batch` 改为接受 `&BridgeContext<S>` | 2 处 → 0 |
| `batch_proposer.rs` | 移除未使用的 `_signing_key` 参数（降至 7 参数） | 1 处 → 0 |

#### 最终状态

自有代码中 `#[allow(clippy::too_many_arguments)]` 实例：**0 处**（目标 ≤ 4，超额完成）。

---

## 5. 不纳入本文件的事项

以下问题已超出"优化债务"范畴，不在本文件跟踪：

- Gas 校准精度（Phase D-4 跟踪）
- Agent Core 功能补齐（Phase Z 跟踪）
- 形式化验证证据链（Phase FV 跟踪）
- 文档运维债务（Phase Y 跟踪）

## 6. 历史处理顺序（已过时，保留参照）

原排序为 OPT-004 → OPT-003 → OPT-002。前两项已关闭，当前仅剩 OPT-002 待 v0.1.11 Phase D 收敛。
