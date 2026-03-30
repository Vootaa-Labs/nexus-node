# 覆盖率报告 v0.1.15

## 统计范围

本报告记录 Nexus `v0.1.15` 工作区当前的 Rust 测试覆盖率基线。

- 统计范围仅包含 first-party 工作区包。
- `vendor-src/` 下的 vendored 源码已从 LCOV 与 HTML 覆盖率输出中排除。
- `target/` 下的生成内容不进入覆盖率展示。
- 本次覆盖率数据于 2026-03-31 在仓库根目录采集。

## 使用命令

在仓库根目录执行：

```bash
make coverage
make coverage-html
make coverage-json
make coverage-scorecard
make coverage-docs
```

`make coverage-docs` 会先执行一次覆盖率测试采样，再在不重跑测试的前提下导出 LCOV、HTML 与 JSON 汇总，并刷新本报告与 crate 级 scorecard。

## 实测结果

| 指标 | 已覆盖 | 总数 | 覆盖率 |
| --- | ---: | ---: | ---: |
| 行覆盖率 | 47,867 | 54,477 | 87.87% |
| 函数覆盖率 | 4,379 | 5,315 | 82.39% |
| 区域覆盖率 | 15,495 | 20,254 | 76.50% |
| 实例化覆盖率 | 5,446 | 8,883 | 61.31% |

本次运行的额外范围校验：

- 纳入统计的源码文件数：197
- 汇总结果中 `vendor-src` 文件数：0
- crate 级 scorecard 行数：15

## Package Scorecard

| Package | 优先级 | 目标 | 行覆盖率 | 差值 | 函数覆盖率 | 区域覆盖率 | 文件数 | 状态 |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| nexus-consensus | P0 | >= 90% | 96.99% | 0.00% | 91.53% | 89.27% | 9 | 达标 |
| nexus-crypto | P0 | >= 95% | 98.17% | 0.00% | 99.29% | 94.24% | 5 | 达标 |
| nexus-execution | P0 | >= 85% | 94.18% | 0.00% | 92.46% | 87.13% | 29 | 达标 |
| nexus-intent | P1 | >= 85% | 94.20% | 0.00% | 83.94% | 85.30% | 30 | 达标 |
| nexus-network | P1 | >= 75% | 81.73% | 0.00% | 82.03% | 72.70% | 15 | 达标 |
| nexus-storage | P1 | >= 85% | 92.61% | 0.00% | 79.16% | 83.13% | 11 | 达标 |
| nexus-config | P2 | >= 80% | 97.96% | 0.00% | 97.35% | 93.44% | 8 | 达标 |
| nexus-node | P2 | >= 70% | 79.58% | 0.00% | 78.13% | 65.03% | 29 | 达标 |
| nexus-primitives | P2 | >= 80% | 93.27% | 0.00% | 93.18% | 83.41% | 4 | 达标 |
| nexus-rpc | P2 | >= 80% | 90.96% | 0.00% | 79.83% | 80.62% | 33 | 达标 |
| nexus-bench | Support | 跟踪 | 0.00% | n/a | 0.00% | 0.00% | 1 | 跟踪 |
| nexus-genesis | Support | 跟踪 | 90.60% | n/a | 63.04% | 79.56% | 1 | 跟踪 |
| nexus-keygen | Support | 跟踪 | 91.39% | n/a | 65.38% | 72.04% | 1 | 跟踪 |
| nexus-test-utils | Support | 跟踪 | 100.00% | n/a | 100.00% | 100.00% | 0 | 跟踪 |
| nexus-wallet | Support | 跟踪 | 74.55% | n/a | 75.49% | 59.99% | 21 | 跟踪 |

## Top 10 覆盖率热点

| Package | 优先级 | 文件 | 行覆盖率 | 差值 | 未覆盖行数 |
| --- | --- | --- | ---: | ---: | ---: |
| nexus-crypto | P0 | crates/nexus-crypto/src/mlkem.rs | 97.35% | 0.00% | 7 |
| nexus-crypto | P0 | crates/nexus-crypto/src/falcon.rs | 97.99% | 0.00% | 6 |
| nexus-execution | P0 | crates/nexus-execution/src/block_stm/executor.rs | 88.14% | 0.00% | 179 |
| nexus-consensus | P0 | crates/nexus-consensus/src/dag.rs | 93.23% | 0.00% | 22 |
| nexus-crypto | P0 | crates/nexus-crypto/src/mldsa.rs | 98.31% | 0.00% | 4 |
| nexus-execution | P0 | crates/nexus-execution/src/move_adapter/move_runtime.rs | 89.40% | 0.00% | 85 |
| nexus-crypto | P0 | crates/nexus-crypto/src/csprng.rs | 100.00% | 0.00% | 0 |
| nexus-crypto | P0 | crates/nexus-crypto/src/hasher.rs | 100.00% | 0.00% | 0 |
| nexus-execution | P0 | crates/nexus-execution/src/move_adapter/builtin_vm.rs | 90.04% | 0.00% | 25 |
| nexus-consensus | P0 | crates/nexus-consensus/src/engine.rs | 95.32% | 0.00% | 26 |

## 产物位置

- LCOV 输出：`lcov.info`
- HTML 输出：`target/llvm-cov/html/index.html`
- 本报告使用的机器可读汇总：`target/llvm-cov/coverage-summary.json`
- crate 级 scorecard：`target/llvm-cov/package-scorecard.md`

## 说明

- 这是一份当前时点的覆盖率基线，不代表永久冻结的质量门槛。
- scorecard 中的核心 crate 目标对齐 v0.1.15 覆盖率治理计划。
- CI 覆盖率任务现在调用 `make coverage-docs`，会在 GitHub Actions step summary 中输出 package 摘要，并把刷新后的中英文覆盖率报告与 scorecard 作为 workflow artifact 保留。
