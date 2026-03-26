# 基准测试报告 v0.1.13

## 统计范围

本报告记录 Nexus `v0.1.13` 工作区当前的 Criterion 基准测试基线。

- 执行范围仅包含 first-party 基准测试包 `nexus-bench`。
- `vendor-src/` 下的 vendored crate 可能仍会作为传递依赖参与编译，但其 benchmark target 不会被执行。
- 本报告基于 2026-03-24 在 `target/criterion/` 下生成的 Criterion 输出整理而成。

## 使用命令

在仓库根目录执行：

~~~bash
make bench
make bench-docs
~~~

`make bench` 会先执行 `cargo bench -p nexus-bench`，随后自动刷新这份中英文报告。
`make bench-docs` 仅使用现有 Criterion 输出重新生成报告。

## 汇总

### 宏观指标摘要

以下指标由代表性微基准推导而来，目的是让外部读者先看到更高层的性能轮廓。

- 它们适合作为工程代理指标，不应直接等同于端到端网络 TPS 或 finality 保证。
- 除非基准明确覆盖相关路径，否则这些数字不包含多节点共识延迟、mempool 传播延迟、RPC 排队和磁盘争用等因素。

- `单机流水线 TPS`：约 1.24 K笔/s，代表性时延 40.27 ms（`pipeline/execution/50`）。这是当前最接近本地执行流水线吞吐的代理指标。
- `Block-STM 乐观 TPS`：约 9.25 K笔/s，代表性时延 108.13 ms（`block_stm_no_conflict/1000`）。表示无冲突批次下的乐观上界型执行吞吐。
- `Mempool 写入加出队`：约 1.44 K笔/s，代表性时延 348.21 ms（`pipeline/mempool/insert_drain/500`）。适合观察共识前缓冲压力的上限。
- `Move 调用吞吐`：约 9.29 K次/s，代表性时延 10.77 ms（`move_call_throughput/100`）。单机合约调用执行代理指标。
- `Move 发布吞吐`：约 9.28 K次/s，代表性时延 10.78 ms（`move_publish_throughput/100`）。模块发布路径代理指标，不等于链级部署速率。
- `余额查询命中 QPS`：约 7.26 M次/s，代表性时延 137.74 ns（`pipeline/balance_query/hit`）。表示存储热路径查询能力的代理值。
- `线缆编码吞吐`：约 25.09 K帧/s，代表性时延 39.85 us（`wire_codec/encode/131072`）。表示大帧场景下纯编解码序列化能力。

### Devnet 结论

以下内容来自最新一次多节点 sweep 结果文件 
`target/devnet-bench/devnet_benchmark_results.json`。

- `7 节点 devnet 基线`：在 4 个 workers 之前，集群可见性仍可全部确认，约为 11.28 笔/秒，集群可见 p95 时延约 3533 ms。
  - `观测到的拐点`：第一处明显断崖出现在 8 个 workers，此时只有 2/80 笔交易在当前基准窗口内达到集群可见，观测到的集群可见吞吐下降到 0.52 笔/秒。
  

### 对外口径摘要

当这些 benchmark 结果对外传播时，建议按下面的口径使用。

可以对外说的数字：

- "单机流水线 TPS"、"Move 调用吞吐"、"Move 发布吞吐" 可以对外作为节点本地或组件级 benchmark 表述，但必须明确写成 "单机" 或 "组件级" 指标。
- "Block-STM 乐观 TPS" 可以对外作为 "无冲突、乐观路径" 下的执行上界代理指标表述，不代表整链吞吐。
- "余额查询命中 QPS" 与 "线缆编码吞吐" 可以对外作为存储热路径查询和编解码序列化能力的组件指标表述。
- 可以说明本报告仅基于 first-party Criterion benchmark 生成，未执行 vendored benchmark target。

没有端到端证据前，只能内部使用的数字：

- 任何整网 TPS、验证者集合 TPS、持续生产容量相关表述。
- 任何 finality latency、confirmation latency、commit latency 相关表述。
- 任何面向公网 RPC、WebSocket、MCP 的 p95 或 p99 延迟表述。
- 任何 durability、持久化、崩溃恢复、state sync 吞吐相关表述。
- 任何声称这些微基准足以代表真实合约负载、对抗性冲突或公网网络条件的表述。

建议的对外措辞：

- 优先使用 "组件级 benchmark"、"单机代理指标"、"乐观执行上界"、"热路径查询指标" 这类词。
- 在补齐专门的端到端、多节点 benchmark 之前，避免直接写成 "整条链可达 X TPS" 或 "finality 为 Y ms"。

- 已采集基准组数：65
- 已采集基准用例数：165
- 按均值估计最慢的用例：`pipeline/mempool/insert_drain/500`，均值为 348.21 ms

各基准组用例数：

- `blake3_digest`: 5 个基准用例
- `blake3_incremental`: 3 个基准用例
- `blake3_merkle_root`: 5 个基准用例
- `block_stm_conflict`: 3 个基准用例
- `block_stm_empty`: 1 个基准用例
- `block_stm_no_conflict`: 4 个基准用例
- `block_stm_parallelism`: 4 个基准用例
- `block_stm_single`: 1 个基准用例
- `commitment_insert`: 3 个基准用例
- `commitment_prove_batch`: 3 个基准用例
- `commitment_prove_exclusion`: 4 个基准用例
- `commitment_prove_key`: 4 个基准用例
- `commitment_root`: 4 个基准用例
- `commitment_verify_proof`: 4 个基准用例
- `config_validate`: 1 个基准用例
- `consensus/certificate_to_commit`: 2 个基准用例
- `consensus/proposal_to_certificate`: 3 个基准用例
- `consensus/round_advance`: 3 个基准用例
- `consensus_cert_build`: 3 个基准用例
- `consensus_cert_verify`: 3 个基准用例
- `consensus_dag_causal_history`: 1 个基准用例
- `consensus_dag_insert`: 2 个基准用例
- `consensus_engine_process`: 1 个基准用例
- `dilithium3_keygen`: 1 个基准用例
- `dilithium3_sign`: 3 个基准用例
- `dilithium3_verify`: 3 个基准用例
- `e2e/submit_to_receipt_proxy`: 3 个基准用例
- `falcon512_keygen`: 1 个基准用例
- `falcon512_sign`: 3 个基准用例
- `falcon512_verify`: 3 个基准用例
- `gas_cost_baseline`: 3 个基准用例
- `intent_compile_contract_call`: 1 个基准用例
- `intent_compile_swap`: 1 个基准用例
- `intent_compile_transfer`: 1 个基准用例
- `intent_cross_shard`: 4 个基准用例
- `intent_gas_estimation`: 1 个基准用例
- `intent_validation`: 1 个基准用例
- `kyber768_decaps`: 1 个基准用例
- `kyber768_encaps`: 1 个基准用例
- `kyber768_keygen`: 1 个基准用例
- `mixed_workload`: 3 个基准用例
- `move_call`: 1 个基准用例
- `move_call_contention`: 3 个基准用例
- `move_call_throughput`: 3 个基准用例
- `move_parallelism`: 4 个基准用例
- `move_publish`: 1 个基准用例
- `move_publish_bytecode_size`: 4 个基准用例
- `move_publish_throughput`: 3 个基准用例
- `network_service_build`: 1 个基准用例
- `pipeline/balance_query`: 2 个基准用例
- `pipeline/batch_store`: 2 个基准用例
- `pipeline/execution`: 3 个基准用例
- `pipeline/genesis_boot`: 1 个基准用例
- `pipeline/mempool`: 4 个基准用例
- `query_view`: 1 个基准用例
- `recovery/batch_restore`: 3 个基准用例
- `recovery/dag_restore`: 3 个基准用例
- `rpc/mcp_call`: 1 个基准用例
- `rpc/rest_query`: 2 个基准用例
- `rpc/rest_submit`: 1 个基准用例
- `rpc/ws_fanout`: 3 个基准用例
- `storage/rocksdb_checkpoint`: 1 个基准用例
- `storage/rocksdb_read`: 2 个基准用例
- `storage/rocksdb_write`: 4 个基准用例
- `wire_codec`: 10 个基准用例

## 实测结果

下表中的均值来自 Criterion `estimates.json`，区间为其报告的 95% 置信区间。

| 分组 | 基准项 | 吞吐量 | 均值 | 95% 置信区间 |
| --- | --- | ---: | ---: | ---: |
| `blake3_digest` | `blake3_digest/bytes/1024` | 1,024 bytes | 877.97 ns | 877.00 ns to 879.17 ns |
| `blake3_digest` | `blake3_digest/bytes/10240` | 10,240 bytes | 6.58 us | 6.57 us to 6.58 us |
| `blake3_digest` | `blake3_digest/bytes/102400` | 102,400 bytes | 42.41 us | 42.35 us to 42.50 us |
| `blake3_digest` | `blake3_digest/bytes/32` | 32 bytes | 112.95 ns | 112.79 ns to 113.11 ns |
| `blake3_digest` | `blake3_digest/bytes/512` | 512 bytes | 444.44 ns | 443.83 ns to 445.18 ns |
| `blake3_incremental` | `blake3_incremental/chunk_bytes/1024` | 10,240 bytes | 8.19 us | 8.18 us to 8.20 us |
| `blake3_incremental` | `blake3_incremental/chunk_bytes/256` | 10,240 bytes | 8.29 us | 8.29 us to 8.30 us |
| `blake3_incremental` | `blake3_incremental/chunk_bytes/32` | 10,240 bytes | 8.95 us | 8.93 us to 8.97 us |
| `blake3_merkle_root` | `blake3_merkle_root/leaves/1024` | 1,024 elements | 116.01 us | 113.47 us to 119.39 us |
| `blake3_merkle_root` | `blake3_merkle_root/leaves/16` | 16 elements | 1.76 us | 1.76 us to 1.77 us |
| `blake3_merkle_root` | `blake3_merkle_root/leaves/256` | 256 elements | 28.35 us | 28.32 us to 28.38 us |
| `blake3_merkle_root` | `blake3_merkle_root/leaves/4` | 4 elements | 387.01 ns | 386.28 ns to 388.09 ns |
| `blake3_merkle_root` | `blake3_merkle_root/leaves/64` | 64 elements | 7.13 us | 7.12 us to 7.14 us |
| `block_stm_conflict` | `block_stm_conflict/10` | 10 elements | 1.05 ms | 1.04 ms to 1.05 ms |
| `block_stm_conflict` | `block_stm_conflict/100` | 100 elements | 10.83 ms | 10.82 ms to 10.83 ms |
| `block_stm_conflict` | `block_stm_conflict/50` | 50 elements | 5.37 ms | 5.36 ms to 5.37 ms |
| `block_stm_empty` | `block_stm_empty/empty_block` | 0 elements | 68.31 ns | 68.20 ns to 68.42 ns |
| `block_stm_no_conflict` | `block_stm_no_conflict/10` | 10 elements | 1.11 ms | 1.11 ms to 1.12 ms |
| `block_stm_no_conflict` | `block_stm_no_conflict/100` | 100 elements | 10.87 ms | 10.86 ms to 10.87 ms |
| `block_stm_no_conflict` | `block_stm_no_conflict/1000` | 1,000 elements | 108.13 ms | 108.08 ms to 108.18 ms |
| `block_stm_no_conflict` | `block_stm_no_conflict/500` | 500 elements | 54.07 ms | 54.05 ms to 54.09 ms |
| `block_stm_parallelism` | `block_stm_parallelism/workers/1` | 200 elements | 21.53 ms | 21.52 ms to 21.54 ms |
| `block_stm_parallelism` | `block_stm_parallelism/workers/2` | 200 elements | 21.56 ms | 21.55 ms to 21.57 ms |
| `block_stm_parallelism` | `block_stm_parallelism/workers/4` | 200 elements | 21.69 ms | 21.67 ms to 21.70 ms |
| `block_stm_parallelism` | `block_stm_parallelism/workers/8` | 200 elements | 21.68 ms | 21.66 ms to 21.69 ms |
| `block_stm_single` | `block_stm_single/single_transfer` | 1 elements | 120.28 us | 120.16 us to 120.43 us |
| `commitment_insert` | `commitment_insert/batch_10_into/0` | 10 elements | 3.25 us | 3.24 us to 3.28 us |
| `commitment_insert` | `commitment_insert/batch_10_into/100` | 10 elements | 28.83 us | 28.76 us to 28.90 us |
| `commitment_insert` | `commitment_insert/batch_10_into/1000` | 10 elements | 255.70 us | 255.09 us to 256.37 us |
| `commitment_prove_batch` | `commitment_prove_batch/entries/100` | 10 elements | 238.74 us | 237.88 us to 240.22 us |
| `commitment_prove_batch` | `commitment_prove_batch/entries/1000` | 10 elements | 2.33 ms | 2.33 ms to 2.34 ms |
| `commitment_prove_batch` | `commitment_prove_batch/entries/500` | 10 elements | 1.16 ms | 1.16 ms to 1.17 ms |
| `commitment_prove_exclusion` | `commitment_prove_exclusion/entries/10` | - | 2.71 us | 2.70 us to 2.71 us |
| `commitment_prove_exclusion` | `commitment_prove_exclusion/entries/100` | - | 23.79 us | 23.76 us to 23.82 us |
| `commitment_prove_exclusion` | `commitment_prove_exclusion/entries/1000` | - | 232.05 us | 231.60 us to 232.67 us |
| `commitment_prove_exclusion` | `commitment_prove_exclusion/entries/500` | - | 116.36 us | 116.22 us to 116.53 us |
| `commitment_prove_key` | `commitment_prove_key/entries/10` | - | 2.70 us | 2.70 us to 2.71 us |
| `commitment_prove_key` | `commitment_prove_key/entries/100` | - | 23.99 us | 23.85 us to 24.24 us |
| `commitment_prove_key` | `commitment_prove_key/entries/1000` | - | 232.07 us | 231.65 us to 232.65 us |
| `commitment_prove_key` | `commitment_prove_key/entries/500` | - | 116.94 us | 116.69 us to 117.24 us |
| `commitment_root` | `commitment_root/entries/10` | 10 elements | 2.60 us | 2.59 us to 2.60 us |
| `commitment_root` | `commitment_root/entries/100` | 100 elements | 23.72 us | 23.68 us to 23.75 us |
| `commitment_root` | `commitment_root/entries/1000` | 1,000 elements | 232.98 us | 232.49 us to 233.54 us |
| `commitment_root` | `commitment_root/entries/500` | 500 elements | 122.37 us | 117.78 us to 128.24 us |
| `commitment_verify_proof` | `commitment_verify_proof/entries/10` | - | 580.84 ns | 580.15 ns to 581.59 ns |
| `commitment_verify_proof` | `commitment_verify_proof/entries/100` | - | 921.68 ns | 919.96 ns to 924.24 ns |
| `commitment_verify_proof` | `commitment_verify_proof/entries/1000` | - | 1.25 us | 1.25 us to 1.25 us |
| `commitment_verify_proof` | `commitment_verify_proof/entries/500` | - | 1.14 us | 1.14 us to 1.15 us |
| `config_validate` | `config_validate` | - | 1.28 ns | 1.28 ns to 1.28 ns |
| `consensus/certificate_to_commit` | `consensus/certificate_to_commit/validators/10` | 10 elements | 1.60 ms | 1.60 ms to 1.61 ms |
| `consensus/certificate_to_commit` | `consensus/certificate_to_commit/validators/4` | 4 elements | 236.15 us | 235.80 us to 236.58 us |
| `consensus/proposal_to_certificate` | `consensus/proposal_to_certificate/validators/10` | 1 elements | 914.53 us | 912.64 us to 917.14 us |
| `consensus/proposal_to_certificate` | `consensus/proposal_to_certificate/validators/30` | 1 elements | 2.83 ms | 2.76 ms to 2.94 ms |
| `consensus/proposal_to_certificate` | `consensus/proposal_to_certificate/validators/4` | 1 elements | 366.23 us | 365.68 us to 366.91 us |
| `consensus/round_advance` | `consensus/round_advance/validators_10/4` | 4 elements | 53.26 ms | 53.17 ms to 53.39 ms |
| `consensus/round_advance` | `consensus/round_advance/validators_10/8` | 8 elements | 96.23 ms | 96.07 ms to 96.43 ms |
| `consensus/round_advance` | `consensus/round_advance/validators_4/4` | 4 elements | 8.49 ms | 8.47 ms to 8.52 ms |
| `consensus_cert_build` | `consensus_cert_build/validators/10` | 1 elements | 594.88 ns | 593.26 ns to 596.81 ns |
| `consensus_cert_build` | `consensus_cert_build/validators/30` | 1 elements | 1.20 us | 1.20 us to 1.20 us |
| `consensus_cert_build` | `consensus_cert_build/validators/4` | 1 elements | 368.69 ns | 367.70 ns to 369.92 ns |
| `consensus_cert_verify` | `consensus_cert_verify/validators/10` | 1 elements | 144.04 us | 143.79 us to 144.36 us |
| `consensus_cert_verify` | `consensus_cert_verify/validators/30` | 1 elements | 437.11 us | 435.36 us to 438.99 us |
| `consensus_cert_verify` | `consensus_cert_verify/validators/4` | 1 elements | 58.47 us | 57.71 us to 59.90 us |
| `consensus_dag_causal_history` | `consensus_dag_causal_history/5_rounds_4_validators` | 1 elements | 2.05 us | 2.05 us to 2.05 us |
| `consensus_dag_insert` | `consensus_dag_insert/validators/10` | 10 elements | 7.31 us | 7.03 us to 7.87 us |
| `consensus_dag_insert` | `consensus_dag_insert/validators/4` | 4 elements | 1.55 us | 1.54 us to 1.55 us |
| `consensus_engine_process` | `consensus_engine_process/genesis_plus_round1` | 4 elements | 470.67 us | 469.78 us to 471.87 us |
| `dilithium3_keygen` | `dilithium3_keygen/generate_keypair` | 1 elements | 143.12 us | 142.95 us to 143.38 us |
| `dilithium3_sign` | `dilithium3_sign/msg_bytes/1024` | 1 elements | 272.67 us | 272.42 us to 272.97 us |
| `dilithium3_sign` | `dilithium3_sign/msg_bytes/256` | 1 elements | 388.31 us | 387.91 us to 388.81 us |
| `dilithium3_sign` | `dilithium3_sign/msg_bytes/32` | 1 elements | 447.74 us | 447.30 us to 448.25 us |
| `dilithium3_verify` | `dilithium3_verify/msg_bytes/1024` | 1 elements | 98.41 us | 98.27 us to 98.57 us |
| `dilithium3_verify` | `dilithium3_verify/msg_bytes/256` | 1 elements | 97.66 us | 97.58 us to 97.78 us |
| `dilithium3_verify` | `dilithium3_verify/msg_bytes/32` | 1 elements | 97.63 us | 97.48 us to 97.81 us |
| `e2e/submit_to_receipt_proxy` | `e2e/submit_to_receipt_proxy/hotspot/50` | 50 elements | 39.83 ms | 39.22 ms to 40.43 ms |
| `e2e/submit_to_receipt_proxy` | `e2e/submit_to_receipt_proxy/mixed_conflict/50` | 50 elements | 40.31 ms | 39.86 ms to 40.80 ms |
| `e2e/submit_to_receipt_proxy` | `e2e/submit_to_receipt_proxy/non_conflict/10` | 10 elements | 8.12 ms | 7.99 ms to 8.21 ms |
| `falcon512_keygen` | `falcon512_keygen/generate_keypair` | 1 elements | 3.29 ms | 3.25 ms to 3.33 ms |
| `falcon512_sign` | `falcon512_sign/msg_bytes/1024` | 1 elements | 93.15 us | 92.97 us to 93.39 us |
| `falcon512_sign` | `falcon512_sign/msg_bytes/256` | 1 elements | 92.44 us | 92.27 us to 92.63 us |
| `falcon512_sign` | `falcon512_sign/msg_bytes/32` | 1 elements | 92.10 us | 91.98 us to 92.23 us |
| `falcon512_verify` | `falcon512_verify/msg_bytes/1024` | 1 elements | 15.20 us | 15.18 us to 15.25 us |
| `falcon512_verify` | `falcon512_verify/msg_bytes/256` | 1 elements | 14.58 us | 14.56 us to 14.60 us |
| `falcon512_verify` | `falcon512_verify/msg_bytes/32` | 1 elements | 14.42 us | 14.41 us to 14.44 us |
| `gas_cost_baseline` | `gas_cost_baseline/call_gas` | 1 elements | 121.04 us | 115.65 us to 128.16 us |
| `gas_cost_baseline` | `gas_cost_baseline/publish_gas` | 1 elements | 114.98 us | 114.81 us to 115.15 us |
| `gas_cost_baseline` | `gas_cost_baseline/transfer_gas` | 1 elements | 121.16 us | 121.00 us to 121.33 us |
| `intent_compile_contract_call` | `intent_compile_contract_call/same_shard` | 1 elements | 103.82 us | 103.49 us to 104.16 us |
| `intent_compile_swap` | `intent_compile_swap/single_shard` | 1 elements | 101.37 us | 101.18 us to 101.59 us |
| `intent_compile_transfer` | `intent_compile_transfer/single_shard` | 1 elements | 101.69 us | 101.47 us to 101.94 us |
| `intent_cross_shard` | `intent_cross_shard/16` | 1 elements | 101.72 us | 101.60 us to 101.85 us |
| `intent_cross_shard` | `intent_cross_shard/256` | 1 elements | 101.83 us | 101.67 us to 102.03 us |
| `intent_cross_shard` | `intent_cross_shard/4` | 1 elements | 101.77 us | 101.58 us to 102.05 us |
| `intent_cross_shard` | `intent_cross_shard/64` | 1 elements | 102.03 us | 101.84 us to 102.24 us |
| `intent_gas_estimation` | `intent_gas_estimation/transfer` | 1 elements | 180.81 ns | 180.15 ns to 181.50 ns |
| `intent_validation` | `intent_validation/dilithium3_verify` | 1 elements | 100.26 us | 100.11 us to 100.44 us |
| `kyber768_decaps` | `kyber768_decaps/decapsulate` | 1 elements | 59.08 us | 59.01 us to 59.16 us |
| `kyber768_encaps` | `kyber768_encaps/encapsulate` | 1 elements | 28.33 us | 28.30 us to 28.37 us |
| `kyber768_keygen` | `kyber768_keygen/generate_keypair` | 1 elements | 28.22 us | 28.19 us to 28.25 us |
| `mixed_workload` | `mixed_workload/10` | 10 elements | 993.25 us | 991.40 us to 995.31 us |
| `mixed_workload` | `mixed_workload/100` | 100 elements | 10.73 ms | 10.72 ms to 10.74 ms |
| `mixed_workload` | `mixed_workload/50` | 50 elements | 5.21 ms | 5.20 ms to 5.21 ms |
| `move_call` | `move_call/single_call` | 1 elements | 114.85 us | 114.59 us to 115.23 us |
| `move_call_contention` | `move_call_contention/10` | 10 elements | 1.07 ms | 1.07 ms to 1.07 ms |
| `move_call_contention` | `move_call_contention/100` | 100 elements | 10.82 ms | 10.75 ms to 10.95 ms |
| `move_call_contention` | `move_call_contention/50` | 50 elements | 5.34 ms | 5.33 ms to 5.34 ms |
| `move_call_throughput` | `move_call_throughput/10` | 10 elements | 1.07 ms | 1.07 ms to 1.07 ms |
| `move_call_throughput` | `move_call_throughput/100` | 100 elements | 10.77 ms | 10.76 ms to 10.78 ms |
| `move_call_throughput` | `move_call_throughput/50` | 50 elements | 5.38 ms | 5.34 ms to 5.45 ms |
| `move_parallelism` | `move_parallelism/workers/1` | 100 elements | 10.69 ms | 10.68 ms to 10.69 ms |
| `move_parallelism` | `move_parallelism/workers/2` | 100 elements | 10.71 ms | 10.70 ms to 10.72 ms |
| `move_parallelism` | `move_parallelism/workers/4` | 100 elements | 10.77 ms | 10.76 ms to 10.77 ms |
| `move_parallelism` | `move_parallelism/workers/8` | 100 elements | 10.83 ms | 10.83 ms to 10.84 ms |
| `move_publish` | `move_publish/single_module` | 1 elements | 114.82 us | 114.69 us to 114.97 us |
| `move_publish_bytecode_size` | `move_publish_bytecode_size/kib/1` | 1,024 bytes | 116.27 us | 116.15 us to 116.41 us |
| `move_publish_bytecode_size` | `move_publish_bytecode_size/kib/256` | 262,144 bytes | 327.11 us | 313.95 us to 344.36 us |
| `move_publish_bytecode_size` | `move_publish_bytecode_size/kib/64` | 65,536 bytes | 167.42 us | 167.15 us to 167.74 us |
| `move_publish_bytecode_size` | `move_publish_bytecode_size/kib/8` | 8,192 bytes | 123.22 us | 123.05 us to 123.42 us |
| `move_publish_throughput` | `move_publish_throughput/10` | 10 elements | 1.07 ms | 1.07 ms to 1.07 ms |
| `move_publish_throughput` | `move_publish_throughput/100` | 100 elements | 10.78 ms | 10.77 ms to 10.79 ms |
| `move_publish_throughput` | `move_publish_throughput/50` | 50 elements | 5.34 ms | 5.34 ms to 5.35 ms |
| `network_service_build` | `network_service_build` | - | 38.89 ms | 38.65 ms to 39.17 ms |
| `pipeline/balance_query` | `pipeline/balance_query/hit` | 1 elements | 137.74 ns | 134.13 ns to 142.38 ns |
| `pipeline/balance_query` | `pipeline/balance_query/miss` | 1 elements | 313.44 ns | 312.52 ns to 314.42 ns |
| `pipeline/batch_store` | `pipeline/batch_store/get_hit` | 1 elements | 124.16 ns | 123.72 ns to 124.70 ns |
| `pipeline/batch_store` | `pipeline/batch_store/insert_100` | 100 elements | 31.77 us | 31.71 us to 31.83 us |
| `pipeline/execution` | `pipeline/execution/1` | 1 elements | 855.71 us | 845.20 us to 867.67 us |
| `pipeline/execution` | `pipeline/execution/10` | 10 elements | 8.11 ms | 8.04 ms to 8.19 ms |
| `pipeline/execution` | `pipeline/execution/50` | 50 elements | 40.27 ms | 39.93 ms to 40.60 ms |
| `pipeline/genesis_boot` | `pipeline/genesis_boot` | - | 63.75 us | 63.59 us to 63.95 us |
| `pipeline/mempool` | `pipeline/mempool/insert_drain/1` | 1 elements | 702.19 us | 693.34 us to 711.20 us |
| `pipeline/mempool` | `pipeline/mempool/insert_drain/10` | 10 elements | 6.95 ms | 6.88 ms to 7.03 ms |
| `pipeline/mempool` | `pipeline/mempool/insert_drain/100` | 100 elements | 69.70 ms | 69.00 ms to 70.40 ms |
| `pipeline/mempool` | `pipeline/mempool/insert_drain/500` | 500 elements | 348.21 ms | 346.66 ms to 349.76 ms |
| `query_view` | `query_view/single_query` | 1 elements | 301.99 ns | 301.54 ns to 302.46 ns |
| `recovery/batch_restore` | `recovery/batch_restore/10` | 10 elements | 184.50 us | 183.87 us to 185.14 us |
| `recovery/batch_restore` | `recovery/batch_restore/100` | 100 elements | 1.86 ms | 1.84 ms to 1.92 ms |
| `recovery/batch_restore` | `recovery/batch_restore/500` | 500 elements | 4.66 ms | 4.65 ms to 4.68 ms |
| `recovery/dag_restore` | `recovery/dag_restore/10` | 10 elements | 46.44 us | 46.35 us to 46.55 us |
| `recovery/dag_restore` | `recovery/dag_restore/100` | 100 elements | 469.09 us | 468.29 us to 470.17 us |
| `recovery/dag_restore` | `recovery/dag_restore/500` | 500 elements | 1.22 ms | 1.21 ms to 1.22 ms |
| `rpc/mcp_call` | `rpc/mcp_call/simulate_intent` | 1 elements | 181.86 us | 181.79 us to 181.93 us |
| `rpc/rest_query` | `rpc/rest_query/account_balance_hit` | 1 elements | 3.10 us | 3.10 us to 3.10 us |
| `rpc/rest_query` | `rpc/rest_query/tx_status_hit` | 1 elements | 3.23 us | 3.20 us to 3.27 us |
| `rpc/rest_submit` | `rpc/rest_submit/transfer` | 1 elements | 54.38 us | 54.33 us to 54.42 us |
| `rpc/ws_fanout` | `rpc/ws_fanout/1` | 1 elements | 2.02 us | 2.02 us to 2.02 us |
| `rpc/ws_fanout` | `rpc/ws_fanout/10` | 10 elements | 2.15 us | 2.15 us to 2.16 us |
| `rpc/ws_fanout` | `rpc/ws_fanout/100` | 100 elements | 3.44 us | 3.44 us to 3.44 us |
| `storage/rocksdb_checkpoint` | `storage/rocksdb_checkpoint/snapshot` | 1 elements | 779.24 us | 768.53 us to 790.26 us |
| `storage/rocksdb_read` | `storage/rocksdb_read/hit` | 1 elements | 6.39 us | 6.38 us to 6.40 us |
| `storage/rocksdb_read` | `storage/rocksdb_read/miss` | 1 elements | 6.03 us | 6.03 us to 6.04 us |
| `storage/rocksdb_write` | `storage/rocksdb_write/batch/1` | 1 elements | 7.60 us | 7.47 us to 7.72 us |
| `storage/rocksdb_write` | `storage/rocksdb_write/batch/10` | 10 elements | 10.10 us | 10.08 us to 10.13 us |
| `storage/rocksdb_write` | `storage/rocksdb_write/batch/100` | 100 elements | 28.58 us | 28.46 us to 28.69 us |
| `storage/rocksdb_write` | `storage/rocksdb_write/batch/500` | 500 elements | 108.58 us | 107.34 us to 110.16 us |
| `wire_codec` | `wire_codec/decode/1024` | 1,024 bytes | 586.98 ns | 586.24 ns to 587.89 ns |
| `wire_codec` | `wire_codec/decode/131072` | 131,072 bytes | 80.49 us | 80.38 us to 80.64 us |
| `wire_codec` | `wire_codec/decode/256` | 256 bytes | 162.74 ns | 161.67 ns to 164.63 ns |
| `wire_codec` | `wire_codec/decode/4096` | 4,096 bytes | 2.35 us | 2.34 us to 2.38 us |
| `wire_codec` | `wire_codec/decode/65536` | 65,536 bytes | 40.58 us | 40.49 us to 40.71 us |
| `wire_codec` | `wire_codec/encode/1024` | 1,024 bytes | 561.59 ns | 560.78 ns to 562.44 ns |
| `wire_codec` | `wire_codec/encode/131072` | 131,072 bytes | 39.85 us | 38.51 us to 41.62 us |
| `wire_codec` | `wire_codec/encode/256` | 256 bytes | 259.43 ns | 259.08 ns to 259.85 ns |
| `wire_codec` | `wire_codec/encode/4096` | 4,096 bytes | 1.50 us | 1.50 us to 1.51 us |
| `wire_codec` | `wire_codec/encode/65536` | 65,536 bytes | 20.18 us | 20.03 us to 20.44 us |

## 产物位置

- Criterion 输出根目录：`target/criterion/`
- Criterion HTML 首页：`target/criterion/report/index.html`
- 基准测试源码包：`tools/nexus-bench`

## 说明

- 这是一份 first-party 基准测试基线，不代表冻结不变的性能 SLA。
- 如果 `tools/nexus-bench` 中新增或删减了 benchmark 文件，应重新执行 `make bench` 刷新报告。
- CI 中的 benchmark 回归对比也应保持同一包边界，避免执行 vendored benchmark target。
