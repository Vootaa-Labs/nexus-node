# Nexus v0.1.13 Agent Core 与 MCP 现状报告

## 1. 报告目标

本报告面向产品、架构、协议、AI 能力和外部协作方，说明当前仓库中 Agent Core 与 MCP 的真实实现状态、工作原理、对外接口与成熟度判断。

这里陈述的是当前 `v0.1.13` 代码现实，而不是未来宣传口径。

## 2. 核心结论

当前的 Agent Core 不是空壳目录，也不是只有概念注释。它已经具备如下真实骨架：

- 统一的协议无关请求包络 `AgentEnvelope`
- 明确的 session 生命周期状态机
- 策略判断层 `policy`
- 计划哈希绑定逻辑
- provenance 记录与锚定数据结构
- A2A 协商状态机基础结构
- 面向 MCP 的薄适配层、工具注册表、会话桥接和错误映射

但它仍然是偏 `alpha` 的控制平面骨架，不应被描述成完整闭环的智能体执行平台。当前最主要的未闭环点是：

- `PlannerBackend` 仍然更偏抽象边界而不是完整产品级 planner 生态。
- session 与 provenance 已进入当前节点装配与持久化路径，但整体智能体执行闭环仍未达到“完整自治平台”级别。
- 确认流虽然在策略与 session 状态上已有结构，但“谁确认、怎样确认、如何回填 `confirmation_ref`”仍需要继续收敛。

## 3. 实现结构

### 3.1 Agent Core 模块群

当前 Agent Core 主要位于 `crates/nexus-intent/src/agent_core/`，关键模块如下：

- `mod.rs`: 定义 ACE 作为统一控制平面的边界，强调外部适配器不得绕过权限、session 与 provenance 语义。
- `engine.rs`: `AgentCoreEngine<P: PlannerBackend>`，负责调度、session 获取/创建、策略判断和 planner 调用。
- `envelope.rs`: 定义规范化请求结构，包括协议来源、调用主体、约束、请求类型等。
- `session.rs`: 定义 session 状态机与状态迁移规则。
- `planner.rs`: 定义模拟结果、执行回执、计划绑定相关结构和辅助逻辑。
- `policy.rs`: 定义批准、需要确认、拒绝等决策模型。
- `capability_snapshot.rs`: 定义能力快照、委托链校验与撤销传播。
- `provenance.rs` / `provenance_store.rs`: 定义可审计的 provenance 记录与存储边界。
- `a2a.rs` / `a2a_negotiator.rs`: 定义 Agent-to-Agent 协商状态机与其扩展接口。

### 3.2 MCP 适配层模块群

当前 MCP 主要位于 `crates/nexus-rpc/src/mcp/`，关键模块如下：

- `mod.rs`: 说明 MCP 的职责仅限协议适配，不持有业务真相。
- `handler.rs`: 执行工具名校验、参数翻译、会话派生、ACE 调度与结果回写。
- `registry.rs`: 定义允许工具、禁止工具、工具性质。
- `schema.rs`: 负责 MCP JSON 参数与内部请求结构之间的翻译。
- `session_bridge.rs`: 把 MCP 侧会话标识稳定映射到内部 session/request/idempotency key。
- `error_map.rs`: 负责从内部错误模型映射到 MCP 可见错误语义。

## 4. 工作原理

### 4.1 Agent Core 的运行模型

当前 ACE 的设计目标很明确：不管请求来自 MCP、A2A 还是 REST 风格工具入口，都要先归一化为同一个 `AgentEnvelope`，再通过统一的调度逻辑处理。这样可以保证：

- session 生命周期只有一套定义
- policy gate 只有一套定义
- plan hash 绑定只有一套定义
- provenance 记录语义只有一套定义

这比“每个外部接口各自维护一套权限/状态/回执语义”明显更稳健。

### 4.2 MCP 到 ACE 的端到端流程

典型流程如下：

1. MCP 客户端发起工具调用。
2. `handler.rs` 先做 forbidden-tool 检查，再查注册表。
3. `schema.rs` 把外部 JSON 参数翻译成内部请求类型。
4. `session_bridge.rs` 从 MCP 会话上下文派生 `session_id`、`request_id`、`idempotency_key`。
5. 构造 `AgentEnvelope`。
6. 调用 ACE dispatcher。
7. ACE 执行预校验、session 获取/创建、能力解析、policy 判断、模拟或执行。
8. 结果再被 `handler.rs` 映射回 MCP 工具响应。

这意味着当前 MCP 适配层更像“协议翻译器”，而不是独立业务子系统。

## 5. 当前已实现能力

### 5.1 统一包络与请求类型

当前 `AgentEnvelope` 已能表达：

- 请求来源协议
- 调用主体
- 约束条件
- 请求类型
- deadline / parent session / delegated capability 等上下文

`AgentRequestKind` 已覆盖：

- `IntentRequest`
- `SimulateIntent`
- `ExecutePlan`
- `Query`
- `QueryProvenance`

### 5.2 Session 生命周期

当前 session 状态机已具备明确的前向流转关系：

- `Received`
- `Simulated`
- `AwaitingConfirmation`
- `Executing`
- `Finalized`

以及：

- `Aborted`
- `Expired`

这类实现的价值在于，确认流、重放保护、计划绑定都不必靠零散布尔值拼接。

### 5.3 Policy 与计划绑定

当前 `policy.rs` 已支持至少以下维度的决策：

- 允许/拒绝
- 需要人工确认
- 基于 value threshold、合约 allowlist、能力快照做约束判断

同时 `planner.rs` 与相关校验逻辑已经建立“先模拟、后绑定 plan hash、再执行”的基本安全轮廓。这个轮廓对防止“确认内容与最终执行内容不一致”非常关键。

### 5.4 Provenance 与 A2A 基础

当前代码并不只是简单记录日志，而是已经为 provenance record、锚定 digest、A2A 协商状态机提供了结构基础。这说明 Agent Core 的设计方向不是单纯的“工具执行器”，而是朝向可审计、可委托、可协商的控制平面演进。

### 5.4.1 原生溯源模块的设计理念

此前报告没有把这一点说透。当前 provenance 模块的设计重点，不是“给智能体行为补一份普通日志”，而是把每次 agent 驱动的执行都压缩成可追责、可复算、可反查的原生链路记录。

其设计理念可以概括为四点：

1. 把 `session -> request -> capability -> plan -> tx` 串成一条原生追踪链，而不是依赖应用侧自行拼接日志。
2. 把“热查询记录”和“冷锚定证明”分层，既保留查询性能，也提供篡改可检测性。
3. 把 provenance 视为控制平面的一部分，而不是附属运维日志，因此它与 session、plan hash、confirmation_ref 同时建模。
4. 把 delegated capability 与 parent agent 关系纳入记录结构，使多智能体委托链条天然可回放、可审计。

### 5.4.2 原生溯源模块的运作机制

当前 `provenance.rs` 已清晰表达了运作机制：

- 每次 agent 驱动动作都会生成 `ProvenanceRecord`。
- 单条记录最核心的字段包括：`session_id`、`request_id`、`agent_id`、`parent_agent_id`、`capability_token_id`、`intent_hash`、`plan_hash`、`confirmation_ref`、`tx_hash`、`status`、`created_at_ms`。
- 这类记录首先进入便于查询的热路径存储。
- 多条记录再被组织成 `AnchorBatch`，计算 `anchor_digest`。
- `anchor_digest` 被写入链上，形成冷锚定证明。
- 后续任何人都可以通过 `verify_anchor(record_ids, receipt)` 重新计算摘要，验证一组记录是否与链上锚点一致。

这套机制的价值在于，它允许系统同时支持：

- 按 `agent_id` 查询最近行为
- 按 `session_id` 回放一次完整决策链
- 按 `capability_token_id` 追踪委托使用情况
- 按 `tx_hash` 反查执行来源、确认依据和能力授权关系

### 5.4.3 为什么这不是普通审计日志

普通日志通常只回答“发生了什么”。当前 provenance 设计试图回答的是：

- 谁发起了这次动作
- 是否经过委托
- 用了哪张能力令牌
- 模拟出的 plan 是什么
- 最终执行落链的 tx 是什么
- 这些记录后来有没有被离线篡改

因此 provenance 在 Nexus 里更接近“原生可验证审计层”，而不是“日志侧车”。

### 5.5 MCP 已暴露工具

当前 `registry.rs` 暴露的 MCP 工具包括：

- `query_balance`
- `query_intent`
- `query_contract`
- `simulate_intent`
- `execute_plan`
- `query_provenance`

同时明确禁止：

- `raw_move_payload`
- `direct_broadcast`
- `admin_override`

这说明当前团队已经在显式阻断“旁路执行”能力，而不是把所有底层动作都直接暴露给 MCP。

## 6. 当前不足与成熟度判断

### 6.1 为什么说它是真实现，不是 PPT

因为以下部件都已在源码中形成可读、可测、可关联的结构：

- envelope schema
- session FSM
- policy decision
- capability snapshot
- provenance record
- MCP tool registry
- handler 到 dispatcher 的真实调用链

### 6.2 为什么仍然只能算 alpha

因为最关键的闭环仍未完全完成：

- `PlannerBackend` 更像预留扩展点，而不是成熟后端。
- session 存储当前还是进程内内存结构，崩溃后语义无法延续。
- human confirmation 仍缺少外部操作闭环。
- A2A 与 provenance 的不少价值目前停留在数据结构和控制面规则，离完整产品行为还有距离。

### 6.3 现实风险

- 若外部团队把它当成“已完整可运营的 agent runtime”，会高估系统闭环程度。
- 若 PlannerBackend 未尽快接线，MCP 的 mutating tool 价值会被压缩在骨架层。
- 若确认流没有闭环，`RequiresConfirmation` 只能算半实现状态。
- 若 provenance 只停留在结构体层而没有形成稳定存储、查询和锚定节奏，它的价值会退化成“可描述但不可运营的审计设计”。

## 7. 对外接口与合作潜力

### 7.1 当前最适合对外说明的能力

当前最适合对外说明的是：

- Nexus 已有统一 Agent Control Envelope 思路。
- MCP 不是独立权限系统，而是通往 ACE 的薄桥接层。
- 系统已经显式考虑会话、幂等、计划绑定、能力限制和 provenance。

### 7.2 当前不宜过度承诺的能力

当前不宜把以下内容作为“已完全完成”的外宣：

- 全功能多智能体协同执行
- 完整人工确认与审批工作流
- 持久化可恢复的 agent session runtime
- 与真实执行后端完全闭环的 planner 驱动执行

### 7.3 潜力判断

从结构上看，ACE + MCP 是这个仓库里最有差异化潜力的能力之一，原因在于：

- 它已经不是“把 RPC 端点换个名字”的 MCP 适配。
- 它试图把工具调用、策略控制、session 语义和可审计性统一起来。
- 如果 `PlannerBackend`、确认流、持久化 session 补齐，它可以自然承接更强的 agent workflow。

## 8. 面向 0.1.1 的建议

### 必做

1. 明确 `0.1.1` 的 Agent Core 范围，避免 backlog 漫无边界扩展。
2. 给 `PlannerBackend` 制定明确接线路线和验收标准。
3. 定义确认流最小闭环，补齐 `confirmation_ref` 的真实来源与状态转换。
4. 决定 session 是否需要持久化，若需要则在 `0.1.1` 前完成最小实现。

### 建议但可阶段化

1. 对 provenance 与 A2A 单独设后续阶段路线图。
2. 为 MCP/ACE 写一份外部接口稳定性说明，明确哪些工具和字段已稳定、哪些仍在演化。

## 9. 结论

Agent Core 与 MCP 当前已经形成了相当明确的内部控制平面方向，其价值不在于“功能已经全部打通”，而在于“统一语义骨架已经成立”。

对 `0.1.1` 而言，最正确的推进方式不是继续增加名词，而是把以下三件事做实：

- planner backend 接线
- confirmation flow 闭环
- session/runtime 持久化边界澄清

只要这三件事没有落地，ACE/MCP 就应被描述为“强骨架、弱闭环”的 alpha 子系统；一旦落地，它会成为 Nexus 最值得对外展示的差异化能力之一。