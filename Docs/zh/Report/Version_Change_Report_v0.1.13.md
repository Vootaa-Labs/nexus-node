# Nexus 版本变更报告 v0.1.13

## 1. 文档定位

本报告用于在不保留旧 Git 历史的前提下，对外说明 Nexus 截止 `v0.1.13` 的主干能力演进过程。

它不是开发流水账，也不是任务管理记录，而是基于以下证据反推的功能进展结果：

- 当前 `v0.1.13` 代码与测试现状
- 非文档类 Git 提交主线
- `Docs_Dev` 中 `v0.1.1` 到 `v0.1.11` 的路线图与审计资料

`v0.1.0` 之前没有正式版本跟踪，因此本报告从“可识别的版本阶段”开始整理。

## 2. 使用方式

这份报告主要回答两个问题：

1. 当前基线是如何形成的
2. 哪些能力现在应被视为现实能力，而不是未来规划

因此，它更适合作为版本脉络与解释文档，而不是发布清单或开发日志。

## 3. 总体结论

从 `v0.1.1` 到 `v0.1.13`，Nexus 的主线演进并不是简单地堆叠功能，而是沿着以下顺序逐步收敛：

1. 先把基础工作区、密码、网络、存储、共识、执行、意图、RPC、节点装配建立起来。
2. 再把 devnet、工具链、测试、CI、Move 工具和默认运行路径打通。
3. 随后把系统从“可运行”推进到“可恢复、可验证、可治理、可扩展”。
4. 最后把 staking、epoch rotation、多分片、HTLC、Agent Core 与形式化验证 runner 接到实际代码闭环中。

截至 `v0.1.13`，项目已经越过“基础设施待补齐”的阶段，进入“运行时硬化、接口收敛、能力细化和对外表达重构”的阶段。

## 4. 版本主线

| 版本 | 功能推进重点 | 实际结果 |
| --- | --- | --- |
| `v0.1.0` | 任务驱动开发阶段 | 建立了工作区骨架与后续版本的模块边界 |
| `v0.1.1` | devnet、开发者路径、Move 工具链与整体闭环 | `nexus-wallet move ...` 成为统一 CLI；本地 devnet、smoke test、部署/调用/查询链路成形 |
| `v0.1.2` | 测试网硬化与安全边界 | 补强限流、校验、恢复与安全门禁，使系统从内部试运行走向更严格的 testnet 约束 |
| `v0.1.3` | 执行与共识正确性 | Block-STM、交易验证、共识安全、状态同步校验进一步收敛 |
| `v0.1.4` | 运行时真实状态与 reconfiguration 基础 | `/ready`、genesis 启动安全、epoch/committee 基础设施、proof RPC 和查询治理接入 |
| `v0.1.5` | testnet 级验证与校准 | 多轮 chaos、soak、proof、gas calibration 和配置漂移检查形成真实证据链 |
| `v0.1.6` | 持久化闭环 | DAG、BatchStore、冷启动恢复和 RocksDB 路径从内存态过渡到可恢复态 |
| `v0.1.7` | 经济与治理基础 | 代币精度调整、stake-weighted quorum 替代按节点数量计票 |
| `v0.1.8` | 状态承诺生产化 | inclusion/exclusion proof、持久化 commitment tree、canonical state root 完整收敛 |
| `v0.1.9` | staking 与委员会轮换 | staking 合约生命周期、快照、election、rotation 和恢复路径成形 |
| `v0.1.10` | 多分片与跨分片执行 | 多分片运行时、shard-aware mempool/gossip/state sync、HTLC lock/claim/refund 全链路落地 |
| `v0.1.11` | Agent Core 与验证证据链 | ACE 主骨架、MCP、session/provenance 持久化、differential/property-test runner 进入主线 |
| `v0.1.12` | 运行时加固 | Move VM 边界加固、gas/payload 接口规范化、配置外部化、PQC (ML-DSA-65) 集成 |
| `v0.1.13` | 对照现状收敛与对外重构准备 | 代码审计表明核心能力已基本成形，重点转向硬化、真实执行闭环和公开资料重组 |

## 5. 跨版本能力演进

### 5.1 从原型到真实运行时

早期版本主要解决“有没有这条链路”的问题；后续版本持续把这些链路从原型态改造成真实运行路径，例如：

- Move VM 从基础适配走向默认启用的真实执行路径
- readiness 从接口存在走向真实反映子系统状态
- proof 从接口占位走向可验证、可持久化、可恢复的生产形态

### 5.2 从内存态到持久化系统

版本推进中的一个清晰主题是持久化边界不断上移：

- 先有 session 与 provenance 的持久化
- 再有 DAG、BatchStore 与 cold restart 恢复
- 随后 state commitment 和多类 RocksDB column family 固化下来

这使得节点不再只是“运行中正确”，而是“重启后仍保持协议状态连续”。

### 5.3 从静态配置到治理驱动

`v0.1.7` 之后，项目明显从“人工配置固定委员会”的思路，转向“由 stake、snapshot、rotation policy 驱动”的链上治理结构：

- quorum 改为按 stake 权重判定
- staking 合约提供注册、bond、unbond、withdraw、slash 生命周期
- committee rotation 与 epoch lifecycle 接入节点实际运行路径

### 5.4 从单分片准备态到多分片运行态

早期代码的数据模型已经有分片意识，但真正的多分片运行时在 `v0.1.10` 才完成系统性接线。其结果包括：

- shard-aware mempool
- shard-aware gossip 与 state sync
- 分片 chain head 与 RPC 观察面
- HTLC 跨分片锁定、领取与退款闭环

### 5.5 从接口存在到证据存在

项目的后续阶段越来越强调“代码可运行之外，还必须有证据证明它能持续成立”：

- 集成测试和场景测试模块数量不断扩展
- CI 门禁覆盖 lint、安全、覆盖率、KAT、Move smoke、bench 对比等
- differential corpus、property tests 与 proof 资产逐步接入主线验证链路

## 6. 截止 v0.1.13 的实际状态

从当前代码与 `v0.1.13` 审计可见，以下能力已经属于主干现状，而不是未来愿景：

- REST、WebSocket、MCP 三类外部接口
- RocksDB 持久化节点、session、provenance 路径
- stake-weighted 共识、epoch lifecycle、staking rotation
- 多分片执行与 HTLC
- BLAKE3 state commitment 与 proof surface
- Agent Core 主骨架、A2A、confirm flow、provenance 记录
- Move contract build/deploy/call/query 开发链路
- 测试、脚本、CI、形式化验证 runner 的持续证据结构

## 7. 仍处于后续阶段的事项

本报告也需要明确当前还没有完全收敛的部分，避免对外误导：

1. 真实 Move VM 已接入，但 gas metering 质量仍需继续硬化。
2. Agent Core 的 execute 回执与真实签名/上链提交流程仍需进一步打通。
3. 某些历史元数据与注释仍残留 GraphQL、gRPC 等未来位点，但实际对外接口仍以 REST、WebSocket、MCP 为准。
4. 项目已经具备多分片和治理基础设施，但生产级容量、权限和调优策略还需要继续演进。

## 8. 对外说明建议

如果新的公开仓库不携带旧 Git 历史，对外描述应采用以下口径：

- 这是一个已经完成多轮代码审计与能力收敛的 `v0.1.13` 基线。
- 文档中的重点是“当前代码真实具备什么”，而不是“未来可能做什么”。
- 旧版本的路线图和审计仅作为形成功能脉络的输入，不作为对外展示主体。

## 9. 结语

Nexus 到 `v0.1.13` 的主线结果，可以概括为一句话：

它已经从一个具备完整链路雏形的 Rust 工作区，发展为一个拥有治理、分片、Move、Agent Core、验证证据与运维路径的成系统代码基线。