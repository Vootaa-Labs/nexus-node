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
make fmt-check
make clippy
make check
make test-all
make devnet-smoke
```

本地与 CI 统一采用以下边界规则：

- 只以 first-party 包作为门禁范围
- `make clippy` 与 `make check` 都开启 `--all-targets`，覆盖库、二进制、测试、bench
- `vendor-src/` 下的 Move 或 Aptos 代码即使被 Cargo 传递构建，也不作为本仓发布阻断诊断

发布分支至少应通过以下类别：

| CI 任务 | 门序 | 覆盖范围 |
| --- | --- | --- |
| `lint` | 1 | first-party 格式检查与 clippy |
| `check` | 2 | first-party `cargo check`，包含 test 与 bench |
| `security` | 3 | cargo-audit 与 cargo-deny |
| `test` | 4a | workspace 单测与库测试 |
| `correctness-negative` | 4b | Phase A-F 负路径测试 |
| `startup-readiness` | 4c | readiness、lifecycle、node e2e |
| `epoch-reconfig` | 4d | epoch manager、consensus epoch、epoch store |
| `proof-surface` | 4e | proof roundtrip、commitment、snapshot signing |
| `recovery` | 5 | snapshot export/import、migration、prune |
| `coverage` | 6 | 覆盖率门槛 |
| `crypto-kat` | 7 | 全部密码学 KAT 向量 |
| `move-vm-smoke` | 8 | Move VM 编译与执行冒烟 |

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
5. 记录 go or no-go 结论和后续修复项

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
