# 覆盖率报告 v0.1.13

## 统计范围

本报告记录 Nexus `v0.1.13` 工作区当前的 Rust 测试覆盖率基线。

- 统计范围仅包含 first-party 工作区包。
- `vendor-src/` 下的 vendored 源码已从 LCOV 与 HTML 覆盖率输出中排除。
- `target/` 下的生成内容不进入覆盖率展示。
- 本次覆盖率数据于 2026-03-24 在仓库根目录采集。

## 使用命令

在仓库根目录执行：

```bash
make coverage
make coverage-html
make coverage-docs
```

`make coverage-docs` 会先执行一次覆盖率测试采样，再在不重跑测试的前提下导出 LCOV、HTML 与 JSON 汇总，并用 `target/llvm-cov/coverage-summary.json` 自动刷新本报告。

## 实测结果

以下汇总来自在相同 first-party 包 allowlist 与文件排除规则下执行的 `cargo llvm-cov --json --summary-only`：

| 指标 | 已覆盖 | 总数 | 覆盖率 |
| --- | ---: | ---: | ---: |
| 行覆盖率 | 38,872 | 51,229 | 75.88% |
| 函数覆盖率 | 3,538 | 4,751 | 74.47% |
| 区域覆盖率 | 12,763 | 19,332 | 66.02% |
| 实例化覆盖率 | 4,529 | 7,890 | 57.40% |

本次运行的额外范围校验：

- 纳入统计的源码文件数：216
- 汇总结果中 `vendor-src` 文件数：0

## 产物位置

- LCOV 输出：`lcov.info`
- HTML 输出：`target/llvm-cov/html/index.html`
- 本报告使用的机器可读汇总：`target/llvm-cov/coverage-summary.json`

## 说明

- 这是一份当前时点的覆盖率基线，不代表永久冻结的质量门槛。
- 如果 first-party 包清单发生变化，应先更新 Makefile 中的 allowlist，再重新执行 `make coverage-docs`。
- CI 覆盖率任务调用的是 `make coverage-docs`，并会把刷新后的中英文覆盖率报告作为 workflow artifact 保留。
