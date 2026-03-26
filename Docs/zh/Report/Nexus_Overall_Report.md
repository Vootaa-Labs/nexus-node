# Nexus 0.1.0 总体现状报告书

## 1. 报告范围与方法

本报告基于当前工作区源码、Cargo 工作区清单、测试与基准结构、Docker 与脚本、以及 `.github/workflows/` 中的 CI/CD 配置进行静态审阅。报告结论只陈述在代码中实际观察到的事实，不依赖未提供的外部说明。

本次未执行编译、测试、基准或容器部署，因此关于“是否可运行”的判断以代码结构和脚本/工作流定义为准，不把“已经验证通过”作为事实陈述。

## 2. 执行摘要

Nexus 当前是一个结构相当清晰的 Rust 区块链工作区，核心链路覆盖了配置、P2P 网络、Narwhal DAG 与 Shoal++ 共识、执行层、意图层、RPC 层以及节点装配层。工作区还包含了密钥生成、创世配置、Move 工具链、钱包、基准测试、形式化验证资产和 devnet 运维脚本。

从代码组织看，项目的工程纪律较强：核心 crate 普遍使用 `#![forbid(unsafe_code)]`，模块边界清晰，配置、存储、网络、执行与 RPC 之间有明确的 trait 或服务边界，测试与形式化验证材料也不是空壳目录，而是具备实际内容。

当前最值得团队注意的，不是“核心架构不存在”，而是“若干关键路径仍处于继续校准阶段”：`move-vm` 已进入默认构建路径，公共执行返回面也已去掉 `gas_used: 0` 占位值，但 gas 计费精度与覆盖率仍需继续增强；工作区工具链、GitHub workflow 与 Docker builder 现已统一到 Rust `1.85.0`，后续版本升级必须继续保持同步；Docker 健康检查已收敛到 HTTP readiness 探针；开发者 CLI 已收敛为 `nexus-wallet move ...` 单一入口；而整改与优化债务文档本质上反映的正是这些仍待持续打磨的交付缺口。

## 3. 面向产品总监：产品定位与设计现状

从源码组织推断，Nexus 的产品定位不是单一链上账本，而是一个面向高吞吐、分片、后量子密码学和智能合约执行的完整 L1 平台。其对外能力至少包括：

- 节点运行与多验证人 devnet。
- REST、WebSocket、MCP 三类外部接口。
- Move 合约构建、部署、调用与查询工具链。
- 原生钱包 CLI、测试水龙头、创世初始化与密钥管理工具。
- 用户意图到交易计划的编译与解析层，以及更上层的 agent core。

从产品设计成熟度看，底层链路已经具备“可讲清楚”的整体性，但部分上层能力仍更像阶段性产品能力而非完全闭环产品：

- `nexus-intent` 的 compiler、resolver、agent_core 模块已经成形，说明产品方向不只是“提交原始交易”，而是“提交语义化 intent”。
- `nexus-rpc` 中存在 MCP 适配层，表明项目明确考虑 AI/Agent 生态接入。
- `tools/nexus-wallet` 统一了终端用户和合约开发者路径，当前开发者入口已收敛为单一 CLI。

产品层面的主要不足是默认能力面与宣传能力面仍需继续统一：当前代码中没有单独的 GraphQL 模块，但 `nexus-rpc/src/lib.rs` 顶部注释仍提到 GraphQL；Move 默认路径虽然已经启用真实 `move-vm` 构建，但 gas 计费模型仍需继续校准；部分 DevEx 入口文档仍需要继续去重和收敛。

## 4. 面向架构师：系统架构与顶层设计

### 4.1 工作区拓扑

当前工作区共有 15 个 Cargo package：

- 基础库：`nexus-primitives`
- 基础设施库：`nexus-crypto`、`nexus-network`、`nexus-storage`、`nexus-config`
- 核心业务库：`nexus-consensus`、`nexus-execution`、`nexus-intent`、`nexus-rpc`
- 装配层：`nexus-node`
- 工具：`nexus-keygen`、`nexus-genesis`、`nexus-wallet`、`nexus-bench`
- 测试工具：`tests/nexus-test-utils`

依赖方向基本符合分层设计：`primitives` 在最底层，`node` 在最上层装配，`rpc` 通过 backend trait 与 `node` 对接，而不是反向侵入业务实现。

### 4.2 核心设计优点

- `nexus-node` 保持“thin assembly crate”定位，`src/main.rs` 主要做配置加载、运行时初始化、服务装配和桥接，不把领域逻辑堆在入口文件里。
- `nexus-consensus` 将 Narwhal DAG 与 Shoal++ 排序分离，`engine.rs` 负责编排而不是把所有共识逻辑揉在一起。
- `nexus-execution` 将 Block-STM 与 Move adapter 分开，利于并行执行策略与 VM 适配层独立演进。
- `nexus-intent` 细分为 compiler、resolver、agent_core，说明上层语义处理没有直接和执行器绑死。
- `nexus-rpc` 通过 `QueryBackend`、`IntentBackend`、`ConsensusBackend`、`NetworkBackend`、`TransactionBroadcaster` 等边界从 `nexus-node` 注入实现，适合替换、测试和裁剪。

### 4.3 架构风险与约束

- `move-vm` 已进入默认构建路径，但执行层仍保留 feature gate 以支持有意识的对比构建。架构文档必须明确这一点，否则上层团队容易误判能力边界。
- `nexus-node/src/main.rs` 当前默认使用 `MemoryStore::new()` 初始化存储，而不是直接使用 RocksDB；与此同时配置、Docker、devnet 脚本又都围绕持久化路径展开。这说明装配层当前可能仍偏开发态或阶段态。
- `nexus-rpc/src/lib.rs` 注释写有 GraphQL，但当前源码目录未观察到独立 GraphQL 模块，外部接口叙述应统一到真实代码。

## 5. 面向 Rust 程序员：代码逻辑与模块状态

### 5.1 关键 crate 现状

- `nexus-primitives`：定义 ID、新类型、digest、地址、金额和基础 trait，是全仓最稳定的底层语义层。
- `nexus-crypto`：包含 Falcon、ML-DSA、ML-KEM、哈希与域分离模块，是后量子密码能力中心。
- `nexus-network`：基于 libp2p，具备 transport、discovery、gossip、rate_limit、service 模块，且启用了 QUIC、Yamux、Kad、Gossipsub 等能力。
- `nexus-storage`：有 `StateStorage` 等 trait、`MemoryStore` 和 RocksDB 后端，适合测试与生产双路径。
- `nexus-config`：集中管理 `NodeConfig` 与各子系统配置，并提供 genesis 和目录验证。
- `nexus-consensus`：共识层模块分工清晰，类型、证书、DAG、Shoal 排序器与 validator 管理分层明显。
- `nexus-execution`：`block_stm/` 和 `move_adapter/` 两条主线明确；`move_adapter` 内部模块粒度细，包含 abi、package、publisher、query、resources、verifier、gas_meter 等。
- `nexus-intent`：不仅有 intent compiler/resolver，还存在 agent core，说明这是当前项目差异化能力重点之一。
- `nexus-rpc`：真实对外层面以 REST、WS、MCP 为主；中间件、DTO、指标和 server builder 已独立出来。
- `nexus-node`：负责存储、网络、执行、意图服务、RPC 服务、发现、批处理、mempool、状态同步等实际装配。

### 5.2 工程风格观察

- 核心 crate 普遍使用 `#![forbid(unsafe_code)]`。
- 公开 crate root 都有较完整的模块说明和 re-export，适合做 API 定位。
- 错误类型分 crate 管理，边界比较清楚。
- 多数 `expect()` 出现在测试或受控内部假设场景，但 `nexus-rpc/src/server.rs`、`nexus-node/src/main.rs` 等生产路径仍存在少量 `expect()`，应持续收敛。

## 6. 面向 QA：测试、验证与质量门禁

### 6.1 测试资产

从目录与源码可见，测试资产并不薄弱：

- `crates/nexus-consensus/tests/` 下存在 `fv_property_tests.rs` 和 `fv_proptest.rs`。
- `crates/nexus-intent/tests/fv_proptest.rs` 存在。
- `tests/nexus-test-utils/src/` 下包含 `pipeline_tests.rs`、`multinode_tests.rs`、`network_integration_tests.rs`、`node_e2e_tests.rs`、`resilience_tests.rs`、`rpc_integration.rs`、`toolchain_tests.rs` 等。
- `Makefile` 暴露了 `test`、`nextest`、`doctest`、`test-kat`、`coverage`、`coverage-html`。
- CI 包含 lint、安全、测试、覆盖率、KAT 和 workspace check 多道 gate。

### 6.2 质量门禁的实际不足

- `Makefile` 的 `lint` 目标实际只依赖 `fmt-check` 与 `clippy`，但帮助文本仍把 `machete` 说成 lint gate 的一部分，定义与说明不一致。
- `ci.yml` 中的 `crypto-kat` 现在已经是阻断型门禁，失败会直接使 CI 失败。
- `bench.yml` 对超过阈值的性能回退现在会直接 fail PR，默认阈值为 10%。
- `fuzz.yml` 只有在存在 `fuzz/` 目录和 `fuzz/Cargo.toml` 时才运行；当前工作区未观察到 `fuzz/` 目录，因此该 workflow 目前更像占位保护而非真实门禁。
- `tests/nexus-test-utils/src/lib.rs` 中 `move_integration_tests` 使用 `#[cfg(not(feature = "move-vm"))]`，这使 Move 集成测试覆盖语义需要额外澄清，避免和真实 Move VM 目标相互混淆。

## 7. 面向安全专家：安全性与风险点

### 7.1 正向观察

- 工作区级依赖中使用了 `zeroize`，密码模块和敏感字节封装较为明确。
- 密码学域分离是显式设计，不是散落在调用点里的临时字符串。
- 使用 `cargo-audit`、`cargo-deny`、`deny.toml` 说明供应链安全已进入工程流程。
- Docker 运行时使用非 root 用户。

### 7.2 需要重点跟踪的风险

- 公共执行路径已经移除了 `gas_used: 0` 的占位逻辑，但 gas 计费当前仍主要是工程化的确定性估算模型，在资源定价、安全防护和 DoS 控制上仍需要继续做精度校准。
- `nexus-rpc/src/server.rs` 和 `nexus-node/src/main.rs` 仍可见生产路径 `expect()`，虽不等同于安全漏洞，但会把局部假设转化为进程级崩溃风险。
- Docker 与 compose 当前都已改用 HTTP `/ready` 作为健康检查探针；需要继续关注的是 readiness 与真实服务可用性是否长期保持一致，而不是 CLI 参数漂移。

## 8. 面向形式化验证团队：方案与落地状态

当前仓库中存在实际的形式化验证与规格资产：

- `proofs/agda/consensus/`
- `proofs/tla+/agent/` 与 `proofs/tla+/execution/`
- `proofs/haskell/consensus/` 与 `proofs/haskell/execution/`
- `proofs/move-prover/capabilities/`
- `proofs/property-tests/`

这说明形式化验证不是口头规划，而是已经进入仓库管理。

不过，当前从工作流和主代码路径中还没有看到“形式化结果自动约束主分支”的强绑定机制。现状更接近“并行存在的证明与属性测试资产”，而不是“proof artifact 自动门禁的一体化工程流程”。

## 9. 面向加密专家：密码算法与设计判断

代码明确采用后量子密码路线：

- Falcon-512：共识签名
- ML-DSA / Dilithium3：交易签名
- ML-KEM / Kyber-768：KEM
- BLAKE3：哈希与 digest

`nexus-keygen` 将这些算法直接映射为工具操作，说明密码设计不是停留在库层，而是已进入节点和工具链使用路径。

当前值得进一步审计的方向不是“是否使用了现代算法”，而是：

- 域分离是否在所有调用点保持一致。
- 密钥生命周期、导出格式、轮换流程是否在节点运行与运维流程中闭环。
- 是否需要补充更明确的算法升级和兼容策略。

### 9.1 后量子密码对存储容量规划的现实影响

此前报告低估了一个实际运维问题：后量子密码不仅改变算法选型，也直接改变磁盘、网络报文与日志留存压力。

根据当前 `nexus-crypto` 实现可直接确认的编码尺寸：

- `ML-DSA-65` 交易签名长度为 `3309` 字节。
- `ML-DSA-65` 验证公钥长度为 `1952` 字节。
- `ML-KEM-768` 封装公钥长度为 `1184` 字节。
- `ML-KEM-768` 密文长度为 `1088` 字节。
- 共识层 `Falcon-512` 仍通过 `pqcrypto-falcon` 暴露真实编码尺寸，意味着签名与公钥也不是传统椭圆曲线量级。

这会带来三类容量规划压力：

1. 交易回执、审计记录、mempool 与归档日志的单条对象体积会明显高于传统 ECDSA/Ed25519 体系。
2. 共识证书、批签名验证输入和网络广播负载会随签名体积扩大。
3. 若 Agent Core provenance、审计回放、法务留痕都要求长周期保留，则 PQ 签名相关字段会成为热存储和冷存储规划中的主要放大项。

因此 `0.1.1` 之后的容量规划不能只看状态数据库，还必须把以下对象纳入预算：

- 用户交易签名与公钥字段
- 共识证书签名集合
- provenance / audit 记录中的 plan、confirmation、tx 关联字段
- RPC 层返回值、事件流与离线归档副本

对测试网而言，建议至少按“传统签名链的数倍体积”估算日志和归档盘，而不是沿用常规 Web3 节点的经验值。

## 10. 面向性能工程师：基准、热点与性能门槛

### 10.1 已观察到的性能工程资产

- `tools/nexus-bench/benches/` 下包含 `crypto_bench.rs`、`consensus_bench.rs`、`execution_bench.rs`、`hash_bench.rs`、`intent_bench.rs`、`network_bench.rs`、`pipeline_bench.rs`。
- `bench.yml` 具备 PR 基准对比框架。
- 共识 crate 顶部文档中直接写了吞吐和延迟目标，执行层也有专门 metrics 模块。

### 10.2 当前不足

- `tools/nexus-bench/src/lib.rs` 顶部文档仍把 benchmark 文件描述为 “stubs, to be populated after subsystem implementation”，与当前已有 bench 文件清单不完全匹配，说明文档尚未跟上实现演化。
- Benchmark workflow 当前已经可以阻断超过阈值的回退，但仍需继续验证基准稳定性与误报成本。
- 仓库存在覆盖率与性能工具链，但从代码和 workflow 本身看，尚未形成“关键指标未达标就阻断主分支”的强约束。

## 11. 面向运维工程师：环境、Docker、CI/CD 与交付状态

### 11.1 运维资产现状

- `docker-compose.yml` 定义了 4 个 validator 的 devnet。
- `scripts/setup-devnet.sh` 能生成 key bundle、genesis、每节点 `node.toml` 和目录布局。
- `scripts/smoke-test.sh` 覆盖冷启动、健康检查、重启恢复、late-join。
- `.github/workflows/` 下存在 `ci.yml`、`bench.yml`、`fuzz.yml`、`release.yml`。

### 11.2 需要优先整改的运维问题

- 工作区工具链、CI workflow、Docker builder 与发布流程现已统一到 Rust `1.85.0`；后续任何升级都必须按同一版本同步推进，避免再次引入“本地与 CI 漂移”的风险。
- Dockerfile 当前已把 `nexus-wallet` 纳入 builder 与 runtime 交付，可支持部署后最小化诊断与合约辅助操作；后续仍需继续审视节点镜像与工具镜像是否应长期分离。
- Dockerfile 预热阶段已复制 `tools/nexus-wallet/Cargo.toml`，workspace 预热清单与正式构建现在保持一致。
- libp2p 当前已接入 QUIC、Identify、Kademlia、Gossipsub 与 Request-Response，但未看到 hole punching、AutoNAT、relay v2 等 NAT 穿透能力接线，因此公网机房或至少具备明确端口映射的环境仍是当前测试网部署前提。

## 12. 当前优势

- 分层结构清楚，装配层和业务层职责划分合理。
- 核心子系统齐全，且不是空目录式原型。
- 测试、基准、形式化验证、devnet、CI/CD 资产都已进入仓库。
- 文档基础较好，适合继续打造对 AI 友好的导航层。

## 13. 当前不足

- 部分旧文档和源码注释已滞后于真实实现。
- Move VM 已进入默认构建，gas 计量仍处于“去除零值占位后继续校准”的阶段。
- CI、Docker、Makefile 仍有少量策略级问题未完全收敛，但 KAT 失败与性能回退已经提升为真实门禁。
- 根级 README 与本地演练手册已经补齐，但 `Docs/Report/BACKLOG.md` 与 `Docs/Report/OPTIMIZATION_DEBT.md` 仍反映出后续阶段需要持续推进的交付项。

## 14. 建议的持续改进路线

### 14.1 第一优先级

1. 继续固化 `move-vm` 默认路径的 CI、测试与文档一致性。
2. 在已移除零值占位后，继续提升 gas metering 的校准精度，并把它纳入安全和性能门禁。
3. 持续验证 Docker readiness 探针与真实服务可用性的一致性。
4. 保持 Rust `1.85.0` 的统一版本纪律，避免后续重新漂移。

### 14.2 第二优先级

1. 把基准回退从 warning 提升为可配置 fail gate。
2. 让 KAT 真正阻断失败，而不是 `|| true` 放行。
3. 为意图层和 agent core 增加更清晰的能力边界说明与失败模式测试。
4. 继续把剩余文档入口收敛到根级 README 与统一演练手册，避免重复维护。

### 14.3 第三优先级

1. 将 proofs 与主代码验证流程建立更强的自动化关联。
2. 为容器交付策略定义清晰的镜像分层：节点镜像、运维工具镜像、开发工具镜像。
3. 补齐 backlog 与 optimization debt 的可见跟踪面，避免历史文档继续指向不存在的文件。

## 15. 结论

Nexus 当前不是“概念仓库”，而是一个已经具备完整链路骨架和多类工程资产的 Rust 区块链系统。它的主要任务不是推倒重来，而是把若干关键路径从“设计已成形、实现已落地一半”推进到“默认行为、测试门禁、部署接口、文档叙述全部一致”。

对后续团队协作而言，最重要的不是继续扩张模块数量，而是先让已有模块在默认构建、测试覆盖、运维脚本和对外叙述上收敛到同一版本事实。