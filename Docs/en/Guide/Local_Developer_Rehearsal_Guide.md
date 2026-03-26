# Nexus Local Developer Rehearsal Guide

## 1. Purpose

This guide provides the shortest reproducible local path for the `v0.1.13`
baseline. It connects the current developer workflow end to end:

1. install the pinned toolchain
2. build the wallet CLI
3. generate local devnet keys and config
4. start the Docker devnet
5. run smoke tests
6. run contract build, deploy, call, and query rehearsal

The unified CLI entry for contract work is:

```bash
nexus-wallet move <subcommand> ...
```

## 2. Prerequisites

- Rust toolchain `1.85.0`
- Docker Desktop or equivalent Docker Engine + Compose v2
- `curl`
- `jq`

Recommended preparation from the repository root:

```bash
rustup toolchain install 1.85.0
rustup override set 1.85.0

cargo build -p nexus-wallet
docker build -t nexus-node .
```

## 3. Step A: Generate the local devnet

`setup-devnet.sh` builds `nexus-keygen` and `nexus-genesis`, generates validator
key bundles, emits `genesis.json`, and writes one `node.toml` per validator.

```bash
./scripts/setup-devnet.sh -o devnet -f
```

Current defaults derived from the script:

- validators: `7`
- chain id: `nexus-devnet-1`
- shards: `1`
- output directory: `./devnet-n7s`
- REST base port: `8080`
- P2P base port: `7000`

## 4. Step B: Start the devnet

```bash
docker compose up -d
docker compose ps
```

The node containers expose readiness and health endpoints on the REST ports.
Node `0` is normally reachable at `http://localhost:8080`.

## 5. Step C: Run the smoke test set

```bash
./scripts/smoke-test.sh
```

The current smoke script checks these categories:

- cold start readiness
- `/health`, `/ready`, and `/metrics`
- consensus, validator, and network endpoints
- faucet and balance flow
- restart recovery and late join
- cross-node consistency and shard endpoints
- state commitment, inclusion proof, and exclusion proof
- staking election and rotation-related endpoints

If the test fails early, inspect:

```bash
docker compose logs nexus-node-0
docker compose logs nexus-node-1
```

## 6. Step D: Run the contract rehearsal

```bash
./scripts/contract-smoke-test.sh
```

The current script exercises the example Move packages under
`contracts/examples/` and uses `nexus-wallet` for all developer-facing contract
operations.

Representative manual build command:

```bash
./target/debug/nexus-wallet move build \
  --package-dir contracts/examples/counter \
  --named-addresses counter_addr=0xCAFE \
  --skip-fetch
```

## 7. Fast Triage Order

When a local rehearsal breaks, use this order:

1. confirm the Rust toolchain is `1.85.0`
2. confirm the Docker image was rebuilt after local code changes
3. rerun `./scripts/setup-devnet.sh -o devnet -f` to refresh keys and config
4. check `docker compose ps` and container logs
5. probe `http://localhost:8080/ready`
6. rerun `./scripts/smoke-test.sh` before rerunning contract flows

## 8. Expected Outcome

At the end of this rehearsal you should have verified that:

- the devnet boots from generated config
- the REST surface is reachable
- the faucet and account endpoints respond
- Move example packages can be built and executed
- the current `v0.1.13` baseline is reproducible without hidden local steps
*** Add File: /Users/vootaa/Projects@Vootaa_Labs/Nexus_Devnet_0.1.13_Pre/Docs/en/Guide/Formal_Verification_Guide.md
# Nexus Formal Verification Guide

## 1. Audience

This guide is for two groups:

- engineers who need a practical entry into the verification assets in this repository
- reviewers who want to understand what evidence already exists in the `v0.1.13` baseline

The point is not to teach each formal method in full. The point is to answer:

1. what assets exist today
2. which assets are runnable now
3. which assets are skeletons or forward-looking placeholders
4. what to run first during the first week on the codebase

## 2. Verification Asset Map

The repository includes several layers of evidence under `proofs/`, crate-level
tests, and shared test utilities.

### 2.1 Assets that are already part of engineering evidence

- property tests for consensus under `crates/nexus-consensus/tests/`
- property tests for execution under `crates/nexus-execution/tests/`
- property tests for intent and Agent Core under `crates/nexus-intent/tests/`
- differential and shared verification helpers under `tests/nexus-test-utils/`
- operational smoke and soak scripts under `scripts/`

### 2.2 Assets that require external tools

- TLA+ models under `proofs/tla+/`
- Move Prover assets under `proofs/move-prover/`
- differential reference material under `proofs/differential/`

### 2.3 Assets that should be treated as roadmap or scaffolding

- proof directories that exist but are not wired into a default developer flow
- deeper theorem-proving tracks under `proofs/agda/` and related areas

## 3. What The Current Baseline Proves Best

The `v0.1.13` baseline provides the strongest practical evidence in these areas:

- consensus safety and ordering invariants through crate tests
- execution determinism and multi-shard scenarios through execution tests
- intent and agent-session invariants through property tests
- end-to-end devnet behavior through smoke and contract rehearsal scripts

It is more accurate to describe the repository as having a layered verification
surface than as having a single formal-verification pipeline.

## 4. Recommended First Week Path

### Day 1

- read `README.md`
- run `./scripts/smoke-test.sh`
- run `./scripts/contract-smoke-test.sh`

### Day 2

- inspect `tests/nexus-test-utils/`
- run one consensus and one execution property test target

### Day 3

- inspect `proofs/tla+/` and `proofs/move-prover/`
- map which artifacts are runnable in your environment and which are retained reference assets

## 5. Practical Commands

Representative commands for the current repo:

```bash
cargo test -p nexus-consensus --test fv_proptest
cargo test -p nexus-execution --test fv_proptest
cargo test -p nexus-intent --test fv_proptest
```

If you are validating the local developer path first, run:

```bash
./scripts/smoke-test.sh
./scripts/contract-smoke-test.sh
```

## 6. Interpretation Guidance

- Treat property tests and smoke scripts as current executable evidence.
- Treat proof directories as mixed: some are runnable now, some are reference material.
- Do not claim more than the repository actually wires into CI or local flows.

## 7. Current Position At v0.1.13

At `v0.1.13`, Nexus already has meaningful verification depth, but it should not
be described as a finished theorem-proving program. The accurate claim is that
the codebase combines:

- runnable property and differential testing
- operational evidence from reproducible scripts
- retained formal-method assets for deeper assurance work
*** Add File: /Users/vootaa/Projects@Vootaa_Labs/Nexus_Devnet_0.1.13_Pre/Docs/zh/Ops/Testnet_Access_Policy.md
# Nexus 公开测试网访问与滥用处置策略

## 1. 文档目的

本文定义 `v0.1.13` 基线下公开测试网的访问边界、配额分层、认证方式和滥用处置原则。
它服务于三类对象：

- 节点与平台运维
- 对外开放 RPC 的发布负责人
- 接入测试网的开发者与合作方

## 2. 当前接口面与适用范围

当前仓库中实际可见的公开接口面是：

- REST
- WebSocket
- MCP

本文的配额与处置策略主要针对 HTTP/REST 与与之配套的公开入口；若对 MCP 暴露能力，
应复用相同的身份分层与审计要求。

## 3. 访问分层

### 3.1 匿名访问

- 无 `x-api-key` 请求头
- 仅用于公开只读试用
- 应施加最低配额与最严格速率限制

### 3.2 已认证开发者

- 携带合法 `x-api-key`
- 用于 SDK 集成、测试与持续查询
- 可获得较高但仍受控的请求额度

### 3.3 白名单合作方

- `x-api-key` 属于白名单集合
- 用于审计、压测协作或重点集成
- 可分配最高额度，但仍必须保留审计与手动熔断能力

## 4. 配额原则

### 4.1 全局原则

- 未认证请求默认走最低配额
- 高成本接口必须比健康检查或元数据接口更低频
- faucet、proof、query、session/provenance 类接口应分别设置限额
- 所有公网入口都应保留按 IP、按 API key、按路径三个维度的限速能力

### 4.2 建议分类

可以按以下四类配置：

1. 低成本元数据接口：`/health`、`/ready`、网络和共识状态查询
2. 中成本查询接口：账户、合约、分片、交易状态查询
3. 高成本证明接口：state commitment、inclusion proof、exclusion proof
4. 有状态或资源敏感接口：faucet、session、provenance、MCP 工具执行

## 5. 认证与传输要求

- 若启用 API key，公网入口应启用 TLS 或反向代理 TLS
- API key 不应硬编码在镜像中，应通过安全配置注入
- API key 泄露后需要支持快速吊销与轮换

## 6. 滥用判定

以下行为应视为滥用或疑似滥用：

- 高频 faucet 请求
- 反复触发 proof 或高 gas query 以制造资源压力
- 扫描式枚举路径或参数
- 长时间维持异常高错误率或超时率
- 通过 MCP 暴露面反复触发重型工具

## 7. 处置流程

### 7.1 自动化处置

- 先做限速
- 再做临时封禁
- 对高风险 key 执行立即吊销

### 7.2 人工处置

- 记录来源 IP、API key、时间窗、命中路径、响应码与限流次数
- 评估是否需要调整配额或临时关闭部分高成本接口
- 若影响到公开测试网稳定性，按发布/回滚流程执行 go or no-go 决策

## 8. 与当前代码基线的关系

当前仓库已经明确存在：

- 健康与就绪端点
- 网络、共识、账户、合约、proof、分片等 REST 路由
- faucet 接口
- MCP 适配层

因此本策略应与实际 `RpcConfig` 默认值、反向代理 TLS 开关以及发布运行手册一起维护。

## 9. 变更管理

每次发版前至少核对以下内容：

- `Docs/zh/Ops/Testnet_Release_Runbook.md`
- `Docs/zh/Ops/Capacity_Calibration_Reference.md`
- `Docs/zh/Ops/Testnet_SLO.md`

若配额或认证边界发生变化，应先更新这份策略，再更新发布核对清单。
*** Add File: /Users/vootaa/Projects@Vootaa_Labs/Nexus_Devnet_0.1.13_Pre/Docs/zh/Ops/Testnet_Release_Runbook.md
# Nexus 公开测试网发布演练手册

> **版本:** v0.1.13  
> **受众:** 节点运维、发布工程、值班负责人  
> **目标:** 提供一份从发布前核验到发布后验证再到回滚的完整手册。

## 0. 前置条件

| 项目 | 位置 | 检查方式 |
| --- | --- | --- |
| Rust 1.85.0 工具链 | `rust-toolchain.toml` | `rustup show` |
| Docker + Compose v2 | 发布机 | `docker compose version` |
| `cargo-nextest` | CI 与本地 | `cargo nextest --version` |
| GHCR 或镜像仓库凭据 | Secret 管理 | 验证登录流程 |
| 目标环境 SSH 权限 | 运维 | `ssh <host>` |

## 1. 发布前核验

### 1.1 本地与 CI 门禁

从仓库根目录执行：

```bash
make test-all
make devnet-smoke
```

发布分支至少应通过以下类别：

- lint
- security
- tests
- coverage 或关键验证任务
- devnet smoke

### 1.2 文档一致性

在发布前核对以下文档是否已同步到 `v0.1.13` 基线：

- `Docs/zh/Ops/Testnet_Operations_Guide.md`
- `Docs/zh/Ops/Testnet_Access_Policy.md`
- `Docs/zh/Ops/Testnet_SLO.md`
- `Docs/zh/Ops/Schema_Migration_Guide.md`

## 2. 镜像与制品准备

### 2.1 构建镜像

```bash
docker build -t nexus-node .
```

若走发布流水线，应确保镜像标签与待发布版本一致，并能够回查源码提交。

### 2.2 配置与创世文件

确保以下内容已经就绪：

- `genesis.json`
- 节点 `node.toml`
- validator key bundle
- 目标环境的端口、TLS、反向代理和 API key 配置

## 3. 预发布演练

先在 staging 或本地预演环境执行完整流程：

```bash
./scripts/setup-devnet.sh -o devnet -f
docker compose up -d
./scripts/smoke-test.sh
./scripts/contract-smoke-test.sh
```

至少确认：

- `/ready` 全部返回 `200`
- 共识提交计数持续增长
- faucet 和账户查询可用
- proof 与 shard 相关端点可访问
- 合约示例可完成 build、deploy、call、query

## 4. 正式发布步骤

1. 冻结待发布提交与配置版本
2. 推送镜像到目标仓库
3. 在目标环境滚动或整批更新节点
4. 验证所有节点恢复 ready
5. 复查网络、共识、proof、staking 与 shard 相关接口

## 5. 发布后验证

发布后优先检查以下端点：

- `/health`
- `/ready`
- `/metrics`
- `/v2/consensus/status`
- `/v2/network/status`
- `/v2/validators`
- `/v2/shards`

还应核对：

- 节点数量与委员会信息符合预期
- 提交高度或提交计数持续增加
- 没有异常重启风暴
- 没有明显的 proof、query、faucet 错误峰值

## 6. 回滚触发条件

满足任一条件时，进入回滚评估：

- 大量节点无法 ready
- 共识停止推进
- 核心只读接口普遍报错
- proof 或 shard 路由失效
- API 错误率持续超出 SLO 预算

## 7. 回滚步骤

1. 停止继续扩大发布范围
2. 切回上一个可用镜像与配置版本
3. 保留日志、指标和失败时间窗信息
4. 重新验证 `/ready`、共识状态与关键 RPC 路由
5. 记录 go/no-go 结论和后续修复项

## 8. 多分片与配置注意事项

- 分片数由创世配置决定，变更分片数量通常需要重建 genesis
- 若发布涉及 RocksDB schema 或 Column Family 调整，先执行 `Schema_Migration_Guide`
- 若发布涉及 staking 或 committee 行为变化，联动检查 rotation 手册与 epoch 手册

## 9. 发布记录模板

建议每次发布记录以下信息：

- 版本号
- 提交哈希
- 镜像标签
- 发布时间窗
- 负责人与值班人
- 验证结果
- 回滚与否
- 已知问题与后续动作