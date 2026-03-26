# Nexus 0.1.11 形式化验证落地指南

## 1. 这份指南写给谁

本指南写给两类人：

- 第一次接触 Nexus 形式化验证资产的内部同事
- 希望快速判断 Nexus 当前验证深度的外部审阅者

目标不是把所有理论工具一次讲完，而是回答四个更实际的问题：

1. 现在仓库里到底有哪些验证资产。
2. 哪些今天就能运行，哪些还只是骨架。
3. 新人第一周应该先跑什么。
4. 各层验证工具之间是什么关系。

## 2. 当前验证资产全景

`proofs/` 目录已从骨架阶段推进为多层可执行验证体系：

### 2.1 已纳入 CI（自动门禁）

| 资产 | 位置 | 运行命令 | 覆盖范围 |
|------|------|----------|----------|
| 差异语料自动运行器 | `tests/nexus-test-utils/src/fv_differential_runner.rs` | `cargo test -p nexus-test-utils --lib fv_diff_` | 18 corpus / 60 scenarios / 19 harnesses |
| 执行层 property-test | `crates/nexus-execution/tests/fv_proptest.rs` | `cargo test -p nexus-execution --test fv_proptest` | 多分片确定性、HTLC 原子性、跨分片 state root |
| 共识层 property-test | `crates/nexus-consensus/tests/fv_proptest.rs` | `cargo test -p nexus-consensus --test fv_proptest` | DAG 因果关系、证书仲裁、提交序列 |
| Agent Core property-test | `crates/nexus-intent/tests/fv_proptest.rs` | `cargo test -p nexus-intent --test fv_proptest` | 委托链单调性、会话 FSM、重放保护 |

CI 中的 `formal-verification` job 在每次 PR 上自动运行以上全部测试。

### 2.2 可独立运行（需外部工具）

| 资产 | 位置 | 工具要求 | 运行命令 |
|------|------|----------|----------|
| TLA+ 会话状态机 | `proofs/tla+/agent/FV-AG-002_session_forward.tla` | TLC 或 Apalache | `tlc AgentSession -config AgentSession.cfg` |
| Haskell 提交序列规范 | `proofs/haskell/consensus/CommitSequence.hs` | GHC / runghc | `runghc CommitSequence.hs` |
| Move Prover 质押不变式 | `proofs/move-prover/capabilities/staking_spec.move` | move-prover | `move prove --path contracts/staking` |

### 2.3 差异语料库

`proofs/differential/corpus/` 包含 18 份 JSON 语料文件，覆盖 5 层 17 个验证对象：

| 层 | 文件数 | 场景数 | 覆盖对象 |
|----|--------|--------|----------|
| 共识 | 6 | 18 | VO-CO-001/002/003/006/007/008 |
| 执行 | 4 | 13 | VO-EX-001/003/004/007 |
| 存储 | 3 | 11 | VO-ST-001/004/007 |
| Agent | 4 | 15 | VO-AG-001/002/003/004 |
| 密码学 | 1 | 3 | VO-CR-001 |

差异报告可通过 `FV_GENERATE_REPORTS=1 cargo test -p nexus-test-utils --lib fv_diff_` 自动生成到 `proofs/differential/reports/`。

### 2.4 尚未落地

| 方向 | 状态 | 预期时间 |
|------|------|----------|
| Agda 机器检验证明 | 目录已预留 | v0.1.13+ |
| TLA+ 执行层模型 | 目录已预留 | v0.1.13+ |
| Haskell 执行参考规范 | 目录已预留 | v0.1.13+ |

## 3. 哪些今天就能运行

### 3.1 最先跑的：CI 门禁测试（无需额外工具）

当前最现实、最容易运行的是 Rust 验证测试套件，已全部纳入 CI：

```bash
# 差异语料运行器 — 19 harnesses，覆盖所有 18 份 JSON 语料
cargo test -p nexus-test-utils --lib fv_diff_

# 执行层 property-test — 多分片确定性、HTLC 原子性、跨分片 state root
cargo test -p nexus-execution --test fv_proptest

# 共识层 property-test
cargo test -p nexus-consensus --test fv_proptest

# Agent Core property-test — 委托链、会话 FSM、重放保护
cargo test -p nexus-intent --test fv_proptest
```

这些都是实际贴着生产代码执行的测试，不需要安装任何外部证明器。

### 3.2 Haskell 参考规范（需 GHC）

`proofs/haskell/consensus/CommitSequence.hs` 是提交序列单调性的可执行参考实现：

```bash
runghc proofs/haskell/consensus/CommitSequence.hs
```

输出 10 个属性检查结果，包括单调性、无间隔、锚点轮次偶数性、领导者轮转。

### 3.3 TLA+ 会话状态机（需 TLC 或 Apalache）

`proofs/tla+/agent/FV-AG-002_session_forward.tla` 是 Agent 会话状态机的完整 TLA+ 规范：

- 验证属性：`TypeInvariant`、`ForwardOnly`、`PlanConsistency`、`TerminalAbsorbing`、`AllSessionsTerminate`
- 配置文件：`proofs/tla+/agent/AgentSession.cfg`

```bash
# TLC
tlc proofs/tla+/agent/FV-AG-002_session_forward.tla \
    -config proofs/tla+/agent/AgentSession.cfg

# Apalache
apalache-mc check \
    --config=AgentSession.cfg \
    proofs/tla+/agent/FV-AG-002_session_forward.tla
```

模型检查 3 个并发会话的完整状态空间，验证前向性、终端吸收性和活性。

### 3.4 Move Prover 质押合约不变式（需 move-prover）

`proofs/move-prover/capabilities/staking_spec.move` 包含质押合约的关键不变式规范：

- INV-1: `penalty_total <= bonded`（经济安全）
- INV-2: 状态值合法性
- INV-3: 活跃验证者 unbond_epoch = 0
- INV-4: 活跃验证者质押 ≥ 最小值
- 所有入口函数的前置/后置条件

```bash
move prove --path contracts/staking --named-addresses staking_addr=0xBEEF
```

### 3.5 尚未可运行的：Agda

`proofs/agda/consensus/` 仍是空目录占位。这是团队 theorem proving 方向的长期规划，预计 v0.1.13+ 启动。

## 4. 差分语料库

`proofs/differential/corpus/` 已完全接入自动化流水线：

- **18 份 JSON 语料**，覆盖 5 层 17 个验证对象，共 60 个场景
- **自动运行器**（`fv_differential_runner.rs`）读取每份语料，根据 category 分发到对应 harness
- **报告生成**：设置 `FV_GENERATE_REPORTS=1` 后自动输出 Markdown 报告到 `proofs/differential/reports/`
- **CI 集成**：`formal-verification` job 中自动执行

## 5. 新人第一周建议路线

### 第 1 步：跑通 CI 门禁测试

目的：确认本地环境、理解当前最真实的验证入口。

```bash
cargo test -p nexus-test-utils --lib fv_diff_
cargo test -p nexus-execution --test fv_proptest
cargo test -p nexus-consensus --test fv_proptest
cargo test -p nexus-intent --test fv_proptest
```

产出：本地运行记录 + 对各层关键不变量的第一轮理解。

### 第 2 步：对照 session.rs 和 TLA+ 规范

目的：训练"代码实现"和"规格语义"之间的对照能力。

建议对照对象：

- `crates/nexus-intent/src/agent_core/session.rs`
- `proofs/tla+/agent/FV-AG-002_session_forward.tla`

重点关注：`ValidTransitions` 与 `can_transition_to()` 是否一致，`plan_bound` 与 `bind_plan()` 逻辑是否对齐。

### 第 3 步：运行 Haskell 参考规范

```bash
runghc proofs/haskell/consensus/CommitSequence.hs
```

目的：理解共识排序逻辑的语义约束，与 `shoal.rs` 做交叉验证。

### 第 4 步：阅读差异语料

浏览 `proofs/differential/corpus/` 下的 JSON 文件结构，理解 seed→value 映射策略和 expected→actual 比对模式。

## 6. 验证梯度

Nexus 的形式化验证体系按成本和严格度从低到高排列：

| 层级 | 工具 | 当前状态 | 价值 |
|------|------|----------|------|
| L1 | Rust property tests (proptest) | ✅ CI 门禁 | 贴着真实实现、全自动、零外部依赖 |
| L2 | Differential corpus runner | ✅ CI 门禁 | 跨版本回归、可审计输入/输出对 |
| L3 | Haskell 参考规范 | ✅ 可执行 | 语义级交叉验证 |
| L4 | TLA+ 模型检查 | ✅ TLC-ready | 状态空间穷举、时序性质 |
| L5 | Move Prover | ✅ 规范完成 | 合约级不变式机器证明 |
| L6 | Agda 定理证明 | 📋 规划中 | 最高级别安全证明 |

## 7. 面向外部审阅者的真实表述

当前最稳妥的说法：

- Nexus 已拥有 5 层可运行的形式化验证资产（L1–L5），其中 L1/L2 已纳入每次 PR 的 CI 门禁。
- 差分语料库（18 JSON / 60 scenarios）已完全接入自动化 pipeline，可自动生成审计报告。
- TLA+ 规范覆盖 Agent 会话状态机的完整状态空间，Haskell 参考规范覆盖共识提交序列单调性。
- Move Prover 规范覆盖质押合约 4 项模块级不变式 + 全部入口函数前/后置条件。
- Agda 定理证明为 v0.1.13+ 的长期规划方向，不纳入当前版本的验证证据链。

## 8. 推荐的操作手册

### 基础环境

先保证 Rust 工作区能正常构建和测试，然后按需安装额外工具：

- Rust toolchain（必需）
- GHC / runghc（运行 Haskell 参考规范）
- TLC 或 Apalache（运行 TLA+ 模型检查）
- move-prover（运行 Move Prover 规范验证）

### 每周例行动作

1. 跑一次全部 CI 门禁测试（见 §3.1）。
2. 检查新增代码是否影响现有不变量。
3. 如有差异语料更新，用 `FV_GENERATE_REPORTS=1` 生成报告。

### 报告习惯

每次验证工作留下三类信息：

- 跑了什么
- 对应源码边界是什么
- 结论是"已验证通过"、"部分覆盖"还是"仍是骨架"
