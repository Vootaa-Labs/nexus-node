# Nexus 技术报告 v0.1.13

## 1. 文档定位

本报告不是愿景文档，而是依据当前代码、测试、脚本、工具和工作流，反向提炼出的实际技术状态说明。

目标是让不直接阅读源码的技术人员，也能快速掌握：

- 当前产品能力
- 系统架构与模块边界
- 运行机制与主要数据流
- 测试和形式化验证覆盖面
- 已落地能力与仍待后续硬化的边界

## 2. 阅读方式

本报告按“从外到内”的顺序组织：

1. 先说明能力面
2. 再说明工作区和模块结构
3. 然后说明运行机制与数据流
4. 最后说明验证证据与解释边界

这样安排的目的，是让技术读者无需直接逐 crate 阅读源码，也能从对外能力面一路下钻到代码支持的运行事实。

## 3. 当前产品能力

截至 `v0.1.13`，Nexus 对外和对内已经形成以下主能力面：

### 3.1 节点与协议能力

- validator networking
- Narwhal DAG + Shoal ordering 共识路径
- stake-weighted quorum
- epoch lifecycle 与 committee rotation
- RocksDB 持久化与冷启动恢复

### 3.2 执行与状态能力

- Block-STM 并行执行
- Move 合约 build、deploy、call、query 开发链路
- 多分片运行时
- 跨分片 HTLC lock / claim / refund
- BLAKE3 state commitment 与 proof 接口

### 3.3 上层语义与对外接口

- intent compile / resolve / queue / execute 支撑链路
- Agent Core 会话、capability、A2A、confirm、provenance 主骨架
- REST、WebSocket、MCP 三类外部接口
- 开发者 CLI、keygen、genesis、bench、simulator、monitor 工具集

## 4. 系统结构

## 4.1 工作区分层

当前工作区共有 17 个 Cargo package，可按职责分为四层：

### 底层语义与密码层

- `nexus-primitives`
- `nexus-crypto`

### 基础设施层

- `nexus-network`
- `nexus-storage`
- `nexus-config`

### 协议与业务层

- `nexus-consensus`
- `nexus-execution`
- `nexus-intent`
- `nexus-rpc`

### 装配、工具与测试层

- `nexus-node`
- `nexus-keygen`
- `nexus-genesis`
- `nexus-wallet`
- `nexus-bench`
- `nexus-simulator`
- `nexus-monitor`
- `tests/nexus-test-utils`

## 4.2 核心装配边界

`nexus-node` 是主装配边界。它负责把配置、存储、网络、共识、执行、intent、RPC 与后台任务接到一起。

当前 `main.rs` 可观察到的关键事实包括：

- 默认使用 `RocksStore`
- session 与 provenance 也采用 RocksDB-backed store
- 启动时恢复 session、provenance、genesis、committee 等状态
- 启动长期任务处理 anchoring、intent watcher 与 session cleanup

## 5. 模块职责概览

### 5.1 `nexus-consensus`

负责证书构建与验证、DAG 存储、Shoal 排序、委员会与 epoch 生命周期。

当前不仅有 `engine.rs`、`certificate.rs`、`dag.rs`、`shoal.rs`、`validator.rs`，也有 `epoch_manager.rs` 支撑 epoch 侧的推进逻辑。

### 5.2 `nexus-execution`

负责交易执行、Block-STM 并发模型、Move 适配层与执行服务。

当前代码表明：

- 默认构建启用 `move-vm`
- 仍保留 feature gate，便于对比和兼容
- serial reference 与 parallel path 并存，利于正确性验证

### 5.3 `nexus-intent`

不是单一解析器，而是由 compiler、resolver 和 agent core 三条线组成。

Agent Core 当前包含：

- envelope 与 principal 模型
- session lifecycle
- capability snapshot 与 policy
- planner / dispatcher / intent planner bridge
- A2A negotiation
- provenance 记录
- 内存态与 RocksDB-backed 两类 session / provenance store

### 5.4 `nexus-rpc`

当前真实外部接口面是：

- REST
- WebSocket
- MCP

不存在独立 GraphQL 或 gRPC 实现。某些历史描述或环境变量仍保留了这些未来位点，但不应当成当前功能声明。

REST 面当前不只是基础 health/account/tx，也包括：

- chain head
- shard topology
- HTLC 查询
- state proof
- session 与 provenance 观察面

## 6. 运行机制

## 6.1 节点启动流程

当前启动路径可概括为：

1. 读取并校验 `NodeConfig`
2. 初始化 tracing 和 tokio runtime
3. 打开 RocksDB 存储
4. 恢复 session / provenance 状态
5. 读取 genesis 并校验 chain identity
6. 组装 committee、consensus、execution、intent、RPC 等子系统
7. 启动网络、后台任务与 readiness 跟踪

## 6.2 交易与意图流

对于传统交易路径，主要流向是：

1. 客户端通过 REST 提交交易
2. 节点进入 mempool 与 gossip 广播
3. batch proposer 和共识路径生成可执行批次
4. execution bridge 执行批次并更新链头、回执与状态

对于 intent 路径，则增加：

1. compiler / resolver 做语义化约束与规划
2. planner / dispatcher 形成 simulate-plan-confirm-execute 流程
3. session 与 provenance 持续记录上下文与结果

## 6.3 状态与证明流

当前代码显示的状态证明链路包括：

- 状态写入存储
- BLAKE3 commitment 更新
- inclusion / exclusion proof 生成与校验
- proof RPC 暴露给外部观察者

这意味着状态承诺不再只是内部结构，而是对外可查询、可验证的能力面。

## 7. 数据与控制流

可以把系统看成五条相互配合的主线：

### 7.1 配置控制流

`nexus-config` 汇总节点、网络、存储、genesis、RPC 等配置，并由 `nexus-node` 在启动时消费。

### 7.2 共识控制流

批次从 proposer 进入 DAG，经证书验证、Shoal 排序与 quorum 规则推进到可执行序列。

### 7.3 执行数据流

交易或跨分片操作进入执行器，通过状态视图与存储接口完成状态更新、回执写入和 chain head 更新。

### 7.4 Agent Core 语义流

请求先被包装成 envelope，再在 session、capability 和 policy 约束下走 planning、dispatch、confirm 和 provenance 记录路径。

### 7.5 外部接口流

REST、WebSocket 与 MCP 都是适配层，它们通过 backend traits 调用 `nexus-node` 注入的真实后端，而不是直接嵌入业务逻辑。

## 8. 测试与验证

## 8.1 测试结构

`tests/nexus-test-utils` 已经不是单一 fixture 库，而是包含大量场景模块，例如：

- pipeline
- multinode
- node e2e
- rpc integration
- resilience
- recovery
- readiness
- persistence
- staking rotation
- multi-shard
- HTLC
- release regression

这说明项目的测试重心已经延伸到跨 crate、跨模块、跨运行阶段的系统行为验证。

## 8.2 CI 与证据门禁

当前工作流覆盖：

- lint
- 安全审计
- 测试
- 覆盖率
- crypto KAT
- workspace check
- bench 对比
- fuzz 条件工作流

这些门禁使“代码存在”进一步转化为“代码持续被验证”。

## 8.3 形式化验证与差分验证

仓库中存在：

- TLA+
- Haskell
- Agda
- Move Prover
- property tests
- differential corpus

其中，differential runner 和 property tests 与当前工程实践的结合度更高；其余 proof 资产已进入仓库结构，但自动化强度仍不完全均衡。

## 9. 当前技术特点

### 9.1 明确的装配边界

`nexus-node` 保持“thin assembly”风格，避免把领域逻辑堆到入口。

### 9.2 适合多能力并行演进

consensus、execution、intent、rpc 分层清楚，便于不同方向独立推进。

### 9.3 以现实代码为准的说明边界

对于 `v0.1.13`，更合适的写法不是放大未来规划，而是只描述当前代码、测试、脚本与工作流已经共同支持的能力。这一点尤其影响 API 面、持久化语义和 Agent Core 成熟度的表述。

## 10. 总结

`v0.1.13` 的 Nexus 已经体现出“成系统工程代码基线”的主要特征，而不再只是功能雏形集合。它的核心价值在于以下几件事同时成立：

- crate 边界清楚
- 持久化与重启恢复语义真实存在
- 对外接口可观察、可验证
- 测试与运维证据链持续存在

因此，后续工作的重点不再是证明“有没有”，而是继续做硬化、校准和表达收敛。

### 8.3 面向 AI / Agent 的接口意识

MCP 与 Agent Core 表明项目不仅面向传统链交互，也面向更高层语义与工具调用场景。

### 8.4 强调证据而不是口头描述

上下文导航、测试、CI、proof 资产、Ops runbook 共同构成了工程证据链。

## 9. 当前边界与后续工作

从当前代码反推，仍需继续演进的重点主要有：

1. Move VM 的 gas metering 质量与资源约束强度
2. Agent Core execute 到真实签名与上链提交的闭环
3. 运维参数和治理门禁的进一步配置化
4. 形式化验证资产在更多子系统上的自动化接线深度

## 10. 结论

`v0.1.13` 的 Nexus 已经具备完整系统工程的主要特征：

- 模块边界清楚
- 主链路可运行
- 持久化与恢复存在
- 验证与证据链存在
- 上层 Agent 和多分片能力已经落地到代码

因此，当前最合适的对外表达方式，不是展示一堆未来规划，而是准确说明这套代码已经实现了什么、如何组织、如何运行、如何验证。