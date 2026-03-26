# nexus-move v0.1.1 依赖基线

## 概述

`nexus-node` v0.1.13 通过外部 git 依赖消费 Move 智能合约子系统，来源为 [`nexus-move`](https://github.com/vootaa-labs/nexus-move) 的 `v0.1.1` 标签。

## 依赖声明

```toml
# nexus-node 根 Cargo.toml
nexus-move-types    = { git = "https://github.com/vootaa-labs/nexus-move", tag = "v0.1.1" }
nexus-move-bytecode = { git = "https://github.com/vootaa-labs/nexus-move", tag = "v0.1.1" }
nexus-move-runtime  = { git = "https://github.com/vootaa-labs/nexus-move", tag = "v0.1.1" }
nexus-move-package  = { git = "https://github.com/vootaa-labs/nexus-move", tag = "v0.1.1" }
```

## 消费的 Facade Crate

| Crate | 在 nexus-node 中的角色 |
|---|---|
| `nexus-move-types` | 共享类型：`VmOutput`、`FunctionCall`、`UpgradePolicy` 等 |
| `nexus-move-bytecode` | 字节码策略与发布预检验证 |
| `nexus-move-runtime` | 执行门面、VM 后端、Gas 计量、上游类型再导出 |
| `nexus-move-package` | 包构建管线（由 `nexus-wallet move build` 使用） |

## 关键 Feature Flag

| Flag | nexus-node 启用情况 | 效果 |
|---|---|---|
| `vm-backend` | 是（nexus-execution, nexus-rpc） | 真实 Move VM 执行 + `upstream` 再导出模块 |
| `verified-compile` | 可选 | 包构建时的字节码验证 |
| `native-compile` | 可选 | 通过 vendored `move-compiler-v2` 编译 |

## 上游类型访问

所有上游 Move 类型（来自 `move-core-types`、`move-binary-format`、`move-vm-runtime`、`move-vm-types`）**必须且仅能**通过 `nexus_move_runtime::upstream::*` 访问。禁止直接导入 vendor crate。

```rust
// 正确
use nexus_move_runtime::upstream::move_core_types::account_address::AccountAddress;

// 禁止 — 不可直接依赖 vendor crate
// use move_core_types::account_address::AccountAddress;
```

## 版本固定

- `nexus-move` 通过 git tag (`v0.1.1`) 固定，而非分支或修订号。
- `Cargo.lock` 记录精确的 commit hash 以确保可复现性。
- 升级需修改 `Cargo.toml` 中的 tag 并运行 `cargo update`。

## 文档

`nexus-move` 的完整文档维护在其独立仓库中：
- 架构、门面映射、开发和发布文档位于 `docs/` 目录
- 详见 [nexus-move README](https://github.com/vootaa-labs/nexus-move)
