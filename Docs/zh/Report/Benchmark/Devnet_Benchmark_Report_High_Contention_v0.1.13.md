# Devnet Benchmark Report v0.1.13

本报告由 `cargo run -p nexus-bench --bin devnet_bench --release -- ...` 自动生成。
以下数字属于 devnet 与集群可见性代理指标，不应直接等同于生产链路口径。

## 配置

- 节点: http://127.0.0.1:8080, http://127.0.0.1:8081, http://127.0.0.1:8082, http://127.0.0.1:8083, http://127.0.0.1:8084, http://127.0.0.1:8085, http://127.0.0.1:8086
- 并发 sweep: [8, 10, 12, 16]
- 每个 worker 交易数: 10
- 转账金额: 1000
- 分片数: 2
- 确认超时: 30000 ms
- 轮询间隔: 200 ms

## 指标解释

- `local_tps`: 从开始提交到提交节点可见 receipt 为止的确认吞吐。
- `cluster_visibility_tps`: 从开始提交到所有采样节点都可见 receipt 为止的集群可见性吞吐。
- `cluster_visibility_latency_ms`: 基于跨节点 receipt 可见性的 finality 代理，不是形式化 BFT finality 证明。

## 结果

| Workers | 计划交易数 | 本地确认数 | 集群可见数 | Local TPS | Cluster TPS | Local P50/P95/P99 (ms) | Cluster P50/P95/P99 (ms) |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 8 | 80 | 0 | 0 | 0.00 | 0.00 | n/a | n/a |
| 10 | 100 | 45 | 45 | 7.01 | 6.39 | 2645.92/6250.56/6360.60 | 6124.04/6848.89/6918.87 |
| 12 | 120 | 0 | 0 | 0.00 | 0.00 | n/a | n/a |
| 16 | 160 | 0 | 0 | 0.00 | 0.00 | n/a | n/a |

## 对外口径建议

- 可以使用 "7 节点 devnet sweep"、"集群可见性代理指标"、"receipt 可见延迟"、"测试网稳态 benchmark" 这类表述。
- 不要直接写成 "主网 TPS"、"正式 finality 延迟" 或任何生产容量承诺，除非后续有专门的发布级证据链支撑。
