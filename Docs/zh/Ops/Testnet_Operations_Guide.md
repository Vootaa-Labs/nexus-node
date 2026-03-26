# Nexus 测试网部署与运维指南

> 版本：v0.1.13 | 最后更新：2026-03-24
> 覆盖范围：stake-weighted quorum 到 multi-shard execution 的全部当前运维变更

## 1. 文档范围

本指南覆盖两类场景：

- 在已安装 Docker 的 macOS 机器上启动本地测试网或预演环境。
- 基于 GitHub Actions / CI-CD 把 Nexus 测试网发布到真实服务器环境。

本指南基于当前仓库已有资产：

- `Dockerfile`
- `docker-compose.yml`
- `scripts/setup-devnet.sh`
- `scripts/smoke-test.sh`
- `.github/workflows/ci.yml`
- `.github/workflows/release.yml`
- `.github/workflows/deploy-testnet.yml`

## 2. 统一前提

### 2.1 Rust 版本

整个仓库的统一 Rust 版本应视为 `1.85.0`。

这意味着以下位置都必须与之对齐：

- `rust-toolchain.toml`
- GitHub Actions 中的 stable toolchain
- Docker builder 镜像
- 本地开发机的 `rustup` 默认工具链

### 2.2 当前网络边界

当前网络层采用 `libp2p`，并已接入：

- QUIC
- Kademlia
- Identify
- Gossipsub
- Request-Response

但从当前代码看，还没有把以下 NAT 穿透能力接入主路径：

- AutoNAT
- relay v2
- DCUtR / hole punching
- UPnP / NAT-PMP 自动映射

因此当前最稳妥的部署前提是：

- 真实测试网节点应部署在具有公网 IP 的机房 VPS、云主机或裸金属服务器上。
- 如果部署在 NAT 后或企业防火墙后，则至少需要人工完成端口映射，不能把“自动穿墙”当成当前能力。
- 本地 macOS 的 Docker 场景更适合单机 devnet 预演，而不是对外可加入的真实测试网节点。

## 3. macOS + Docker 本地测试网部署

详细的本地预演命令、标准输出示例和失败排障请统一以 `Docs/zh/Guide/Local_Developer_Rehearsal_Guide.md` 为准；本节只保留运维侧摘要，避免多份手册并行漂移。

### 3.1 机器前提

建议满足以下条件：

- 已安装 Docker Desktop
- 已安装 Xcode Command Line Tools
- 磁盘至少预留 20 GB
- 内存至少 8 GB，推荐 16 GB

### 3.2 一次性准备

在仓库根目录执行：

```bash
rustup toolchain install 1.85.0
rustup override set 1.85.0

cargo build -p nexus-wallet
docker build -t nexus-node .
./scripts/setup-devnet.sh -o devnet -f
```

具体成功输出和失败分支请直接对照 `Docs/zh/Guide/Local_Developer_Rehearsal_Guide.md`。

如果你只需要 release 版 CLI，可改用：

```bash
cargo build --release -p nexus-wallet
./target/release/nexus-wallet move --help
```

### 3.3 启动本地 devnet

```bash
docker compose up -d
```

启动后建议先检查：

```bash
docker compose ps
curl -sf http://localhost:8080/health
curl -sf http://localhost:8080/ready
curl -sf http://localhost:8080/metrics | head
```

若要观察日志：

```bash
docker compose logs -f nexus-node-0
docker compose logs -f nexus-node-1
```

### 3.4 本地验证摘要

最低验证线：

1. `docker compose ps` 中 4 个节点都为 `healthy`。
2. `GET /health`、`GET /ready`、`GET /v2/network/health` 可返回。
3. `./scripts/smoke-test.sh` 通过。
4. `./scripts/contract-smoke-test.sh` 至少走通 build/deploy/call。

### 3.4a 多分片 devnet（v0.1.10+）

当前仓库脚本默认将 devnet 生成为 1 个分片；如需多分片，需要显式设置分片数量。分片数量通过以下路径配置：

- `devnet-n7s/genesis.json` → `num_shards` 字段（默认 1）。
- 每个 validator 的 `shard_id`（0 或 1，轮转分配）。
- `Makefile` 变量 `NEXUS_NUM_SHARDS`。
- `docker-compose.yml` 中每个服务的 `NEXUS_NUM_SHARDS` 环境变量。

#### 启动多分片 devnet

```bash
# 使用默认配置（1 分片）
make devnet-up

# 指定分片数量
make devnet-up NEXUS_NUM_SHARDS=3
```

#### 重新生成 genesis 和 compose

```bash
# 手动重新生成（7 节点，2 分片）
./scripts/setup-devnet.sh -n 7 -s 2 -o devnet-n7s -f
NEXUS_NUM_SHARDS=2 ./scripts/generate-compose.sh -n 7
```

#### 分片相关 API 端点

| 端点 | 说明 |
|------|------|
| `GET /v1/shards` | 返回所有分片 ID 列表 |
| `GET /v1/shards/{id}/head` | 返回指定分片的链头高度和 state root |
| `GET /v1/status` | 包含 `num_shards` 字段 |

#### 多分片 smoke-test

`smoke-test.sh` 测试 20-22 会自动验证多分片配置的一致性：

- Test 20: genesis 中 `num_shards` 正确
- Test 21: 分片 API 端点可达
- Test 22: 所有节点报告一致的分片数量

### 3.5 Staking 运维（v0.1.9+）

v0.1.9 引入了链上 staking 合约与委员会轮换策略层。以下操作适用于 devnet 和 testnet 环境。

#### Staking 查询

```bash
# 查询当前验证人集合及其 stake 权重
curl -sf http://localhost:8080/v2/staking/validators | jq .

# 查询节点状态（包含 epoch 和 committee 信息）
curl -sf http://localhost:8080/v1/status | jq .
```

#### 关键概念

- **Stake-Weighted Quorum**（v0.1.7+）：共识投票按 stake 权重计算，而非简单节点数量。
- **委员会轮换**（v0.1.9+）：epoch 切换时根据链上 staking 合约结果重新选举委员会。
- **选举参数**：通过 `nexus-config` 配置，包括最小 stake 阈值、最大验证人数量等。

#### Staking 运维注意事项

1. 委员会变更发生在 epoch 边界，不会在 epoch 中途生效。
2. slash 事件会影响下一个 epoch 的委员会构成。
3. 多分片环境下每个分片共享同一个全局委员会是当前代码路径下需要重点校验的行为。
4. 详细的 staking 与 rotation 运维操作请参阅 `Docs/zh/Ops/Staking_Rotation_Runbook.md`。

### 3.6 Commitment / Proof 运维（v0.1.8+）

v0.1.8 引入了 BLAKE3 Merkle 状态承诺树，支持排除证明和增量持久化。

#### Proof 端点

| 端点 | 方法 | 说明 |
|------|------|------|
| `/v2/state/proof` | POST | 单 key 的 inclusion/exclusion 证明 |
| `/v2/state/proofs` | POST | 批量 key 证明 |

```bash
# 请求单 key inclusion/exclusion proof
curl -sf -X POST http://localhost:8080/v2/state/proof \
  -H 'Content-Type: application/json' \
  -d '{"key": "0x<hex-encoded-storage-key>"}' | jq .

# 批量 proof
curl -sf -X POST http://localhost:8080/v2/state/proofs \
  -H 'Content-Type: application/json' \
  -d '{"keys": ["0xkey1", "0xkey2"]}' | jq .
```

#### State Root 一致性校验

每次提交后，state root 会包含在区块头中。运维人员可通过以下方式验证一致性：

```bash
# 比较两个节点的链头 state root
curl -sf http://localhost:8080/v1/status | jq '.state_root'
curl -sf http://localhost:8081/v1/status | jq '.state_root'
```

多分片环境下，每个分片有独立的 state root，全局 state root 是所有分片 state root 的聚合。

### 3.7 常见问题与命令面

#### 开发者 CLI 命令约定

当前所有合约构建、部署、调用、查询命令都应通过：

```bash
./target/debug/nexus-wallet move <subcommand> ...
```

若已构建 release 二进制，则使用：

```bash
./target/release/nexus-wallet move <subcommand> ...
```

#### 端口冲突

默认使用：

- REST: `8080-8083`
- gRPC: `9090-9093`
- P2P: `7000-7003`

若本机端口冲突，需同时修改：

- `scripts/setup-devnet.sh` 生成的 `node.toml`
- `docker-compose.yml` 端口映射

#### macOS 上的网络可达性误区

Docker Desktop 里的容器并不等于你获得了公网可加入节点。它适合本地 devnet、自测、预演，不适合作为对外真实测试网验证人。

#### 数据保留

`devnet/validator-N/data/` 是本地持久目录。删除该目录会导致节点状态重置。

## 4. 真实测试网的服务器部署建议

### 4.1 推荐环境

当前更推荐的真实测试网环境是：

- Linux VPS 或云主机
- 公网 IP
- 明确开放 P2P、REST、gRPC 端口
- 独立数据盘或足够 SSD

### 4.2 端口建议

单节点至少需要规划：

- P2P QUIC 端口
- REST 端口
- gRPC 端口

若要对外开放 Explorer、钱包或运维面，还要再增加反向代理与 ACL 设计。

### 4.3 容量规划

不要只按传统链节点估算容量。当前仓库采用后量子签名与 KEM，交易、公钥、审计记录与共识对象体积都更大。

最低建议：

- 系统盘与数据盘分离
- 日志与归档独立目录
- 提前规划 provenance / receipt / metrics / docker logs 的增长

## 5. GitHub CI/CD 上线测试网的推荐流程

### 5.1 当前仓库现实

当前 GitHub Actions 已有：

- `ci.yml`: 质量门禁（当前 v0.1.5 基线包含 `startup-readiness`、`epoch-reconfig`、`proof-surface` 三个阻断 gate）
- `release.yml`: 多目标二进制构建
- `deploy-testnet.yml`: 基于 GHCR + SSH + docker compose 的测试网部署入口

`ci.yml` 中的阻断 job 列表（v0.1.10+ 当前基线）：

| Job | Gate | 覆盖范围 |
|-----|------|----------|
| `lint` | 1 | clippy + fmt + deny + unused deps (cargo-machete) |
| `security` | 2 | cargo-audit + deny advisories + debug feature check |
| `test` | 3a | workspace 全量 unit + lib tests (nextest) |
| `correctness-negative` | 3b | Phase A–F 负面路径测试（含 Agent replay、MvHashMap 冲突验证、MCP auth、A2A 签名） |
| `startup-readiness` | 3c | readiness、lifecycle、node e2e |
| `epoch-reconfig` | 3d | epoch manager、consensus epoch、epoch store |
| `stake-weighted-quorum` | 3e | Stake-weighted 投票与 quorum 验证（v0.1.7+） |
| `proof-surface` | 3f | proof roundtrip、commitment、snapshot signing、exclusion proof |
| `recovery` | 4a | snapshot export/import、migration、prune |
| `cold-restart-recovery` | 4b | 冷重启恢复验证（v0.1.10+） |
| `capacity-curves` | 5 | 容量曲线与性能基线（v0.1.10+） |
| `config-drift` | 6 | 配置文档一致性检查（v0.1.10+） |
| `gas-calibration` | 7 | Gas 校准质量门禁（v0.1.10+） |
| `release-go-nogo` | 8 | 发布 go/no-go 检查（v0.1.10+） |
| `coverage` | 9 | tarpaulin 覆盖率 |
| `crypto-kat` | 10 | 全部密码学 KAT 向量 |
| `check` | 11 | feature-flag 交叉编译矩阵 |
| `move-vm-smoke` | 12 | Move VM compile + execute round-trip |

因此当前仓库已经具备 CI、release build 和一条可手动触发的 testnet CD 路径，但它要真正可用，仍依赖 GitHub environment secrets、远端 compose 目录以及部署主机上的 Docker/Compose 运行时准备完毕。

### 5.2 推荐的 GitHub 环境划分

建议至少设置三个环境：

- `dev`
- `staging-testnet`
- `public-testnet`

其中 `public-testnet` 必须开启：

- required reviewers
- 受保护 secrets
- 人工审批后发布

### 5.3 推荐的 CD 形态

对当前仓库而言，最稳妥的是两段式：

1. CI / Release workflow 产出二进制或镜像 artifact。
2. Deploy workflow 通过 `workflow_dispatch` 或 release event，把 artifact 推送到服务器并滚动发布。

更具体的推荐路径：

- 二进制分发：GitHub Release Artifact + SSH / rsync 到每台 validator。
- 容器分发：构建并推送到 GHCR，然后服务器端 `docker compose pull && docker compose up -d`。

### 5.4 推荐的 GitHub Secrets

至少需要：

- `TESTNET_SSH_PRIVATE_KEY`
- `TESTNET_DEPLOY_USER`
- `TESTNET_DEPLOY_HOST`
- `TESTNET_GHCR_USERNAME`
- `TESTNET_GHCR_TOKEN`

若走 API key 或对象存储分发，还需要额外凭据，但应尽量放在 environment secrets 而不是 repository secrets。

### 5.5 推荐的上线步骤

#### 路线 A：镜像化发布

1. 手动触发 `.github/workflows/deploy-testnet.yml`。
2. Workflow 按指定 `deploy_ref` 构建镜像并推送到 GHCR。
3. Workflow 通过 SSH 登录部署主机。
4. 远端以 `NEXUS_IMAGE=<ghcr image>` 方式执行 `docker compose pull` 与 `docker compose up -d`。
5. Workflow 自动轮询 `/ready`。
6. 运维人员再补充检查 `/health`、`/v2/network/health` 和日志状态。

#### 路线 B：二进制发布

1. `release.yml` 生成 `nexus-node`、`nexus-keygen`、`nexus-genesis`、`nexus-wallet` artifact。
2. Deploy workflow 下载 artifact。
3. 通过 `scp`/`rsync` 分发到各节点。
4. 用 `systemd` 或 process supervisor 逐台滚动重启。
5. 每次重启后验证 readiness、health 和网络健康摘要。

若运维节点需要在服务器侧执行合约或钱包辅助操作，应明确分发 `nexus-wallet`，并统一使用：

```bash
./nexus-wallet move --help
./nexus-wallet move build --package-dir <package-dir> --named-addresses <name=0xaddr>
```

当前仓库的默认容器策略是：节点镜像同时携带 `nexus-node`、`nexus-keygen`、`nexus-genesis`、`nexus-wallet`，用于部署后诊断和最小运维操作；运行入口仍是 `nexus-node`。

### 5.6 滚动发布顺序

建议顺序：

1. 非 bootstrap 节点先升级一台。
2. 验证网络恢复与共识状态。
3. 再继续升级其他非 bootstrap 节点。
4. 最后升级 bootstrap 节点。

不要一次性同时重启全部验证人。

## 6. 当前 Deploy Workflow 用法

当前仓库已经提供：

- `.github/workflows/deploy-testnet.yml`

其当前能力包括：

- `workflow_dispatch` 触发
- 目标 GitHub environment 选择
- 指定 `deploy_ref`
- 构建并推送 GHCR 镜像
- SSH 到远端部署主机
- 通过 `NEXUS_IMAGE` 环境变量驱动 `docker compose pull` / `up -d`
- 发布后 readiness 健康检查

建议使用方式：

1. 在 GitHub `Environments` 中创建 `staging-testnet` 和 `public-testnet`。
2. 为每个 environment 配置所需 secrets。
3. 确保远端主机的 compose 文件使用 `${NEXUS_IMAGE:-nexus-node}` 形式引用镜像。
4. 用 `workflow_dispatch` 手动发布，`public-testnet` 配置 required reviewers。

## 7. 运维检查清单

每次上线后至少检查：

1. `GET /health`
2. `GET /ready`
3. `GET /v2/network/health`
4. 节点日志中无持续 panic / crash loop
5. Docker / systemd 自动重启策略有效
6. 磁盘占用增长正常
7. metrics 可抓取

完整的发布演练步骤（包含 staging → go/no-go → promote → rollback 全流程）请参阅 `Docs/zh/Ops/Testnet_Release_Runbook.md`。

公开测试网的接入分层、配额限制与滥用处置流程请参阅 `Docs/zh/Ops/Testnet_Access_Policy.md`。

## 8. 版本演进记录

| 版本 | 运维影响 |
|------|----------|
| v0.1.7 | Stake-weighted quorum：共识投票按权重计算；CI 新增 `stake-weighted-quorum` gate |
| v0.1.8 | BLAKE3 Merkle state commitment：排除证明端点 `/v2/state/proof`；RocksDB 增量持久化；CI 新增 `proof-surface` 扩展 |
| v0.1.9 | 链上 staking 合约：`/v2/staking/validators` 端点；epoch 边界委员会轮换 |
| v0.1.10 | 多分片执行：`/v1/shards` 系列端点；跨分片 HTLC；CI 新增 `cold-restart-recovery`、`capacity-curves`、`config-drift`、`gas-calibration`、`release-go-nogo` |

## 9. 结论

当前仓库已经具备本地 devnet 和 release build 的基础，但真实测试网的上线自动化仍需要单独的 CD workflow 与服务器运维约束配合。

现阶段最准确的运维结论是：

- macOS + Docker 适合本地测试网预演。
- 真实测试网更适合公网可达的 Linux 服务器。
- 现有 libp2p 接线不能直接被写成"自动穿墙"。
- v0.1.7–v0.1.10 新增的 staking、proof、multi-shard 功能均有 API 端点和 CI 门禁覆盖。
- GitHub Actions 的 release build 会同时产出 `nexus-wallet`，CLI 文档与运维命令应统一以 `nexus-wallet move ...` 为准。