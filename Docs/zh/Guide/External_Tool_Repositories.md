# 外部工具仓库

## 概述

两个开发者工具 — `nexus-simulator` 和 `nexus-monitor` — 原属于 Nexus 单体工作区，但**未包含**在 `nexus-node` v0.1.13 仓库中。它们作为辅助工具保留在独立仓库中。

## 工具说明

### nexus-simulator

- **角色**：开发与测试场景模拟。提供脚本化交易回放和网络行为模拟。
- **范围**：开发时工具，节点运行和 CI 不需要。
- **状态**：外部仓库，非 `nexus-node` 依赖。

### nexus-monitor

- **角色**：实时运维监控终端 UI (TUI)。追踪共识就绪状态、节点 `/ready` 状态和 SLO 指标。
- **范围**：运维/可观测性工具，`Ops/Testnet_SLO.md` 中引用用于就绪故障检测。
- **状态**：外部仓库，非 `nexus-node` 依赖。

## 集成边界

- `nexus-node` **不**声明对任何一个工具的 Cargo 依赖。
- 两个工具作为外部客户端消费 `nexus-node` 的 RPC 端点（REST、WebSocket）。
- 运维手册可能引用 `nexus-monitor` 用于监控流程。
- 构建、测试和部署 `nexus-node` 均不需要这两个工具。
