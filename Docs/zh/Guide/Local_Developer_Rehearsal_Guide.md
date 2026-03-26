# Nexus 本地开发演练手册

## 1. 目标

本手册提供一条可以直接复制执行的本地演练路径，串联以下步骤：

1. 生成本地 devnet 配置与密钥。
2. 启动 Docker devnet。
3. 执行基础 smoke test。
4. 执行合约 build / deploy / call / query 演练。

统一开发者 CLI 入口为：

```bash
nexus-wallet move <subcommand> ...
```

## 2. 前置条件

- Rust toolchain `1.85.0`
- Docker Desktop
- `curl`
- `jq`

建议先在仓库根目录执行：

```bash
rustup toolchain install 1.85.0
rustup override set 1.85.0

cargo build -p nexus-wallet
docker build -t nexus-node .
```

## 3. Step A: 生成本地 devnet

执行：

```bash
./scripts/setup-devnet.sh -o devnet -f
```

成功时的标准输出示例：

```text
Using keygen: ./target/release/nexus-keygen
Using genesis: ./target/release/nexus-genesis

=== Step 1: Generating 4 validator key bundles ===
  validator-0 keys generated (peer_id=12D3KooW...)
  validator-1 keys generated (peer_id=12D3KooW...)
  validator-2 keys generated (peer_id=12D3KooW...)
  validator-3 keys generated (peer_id=12D3KooW...)

=== Step 2: Generating genesis configuration ===
  genesis: ./devnet/genesis.json

=== Step 3: Generating per-node configurations ===
  validator-0: config=./devnet/validator-0/config/node.toml, data=./devnet/validator-0/data
  validator-1: config=./devnet/validator-1/config/node.toml, data=./devnet/validator-1/data
  validator-2: config=./devnet/validator-2/config/node.toml, data=./devnet/validator-2/data
  validator-3: config=./devnet/validator-3/config/node.toml, data=./devnet/validator-3/data

=== Step 4: Validating genesis ===

=== Devnet Setup Complete ===
  Chain ID:     nexus-devnet-1
  Validators:   4
  Shards:       1
  Output:       ./devnet
```

排障说明：

- 若看到 `Error: output directory already exists`，说明 `devnet/` 已存在；删除目录或追加 `-f`。
- 若看到 `BFT requires at least 4 validators`，说明传入的 `-n` 小于 4。
- 若脚本停在 `Building nexus-keygen and nexus-genesis...`，通常表示本地尚未构建 release 工具；等待构建完成即可。
- 若 `genesis validate` 失败，优先检查 `devnet/validator-N/keys/validator-public-keys.json` 是否完整生成。

## 4. Step B: 启动本地 devnet

执行：

```bash
docker compose up -d
./scripts/smoke-test.sh
```

建议至少确认：

```bash
docker compose ps
curl -sf http://localhost:8080/health
curl -sf http://localhost:8080/ready
```

如果 `smoke-test.sh` 失败：

- 先看 `docker compose ps` 是否有节点未进入 `healthy`。
- 再看 `docker compose logs -f nexus-node-0` 与 `docker compose logs -f nexus-node-1`。
- 若 `/ready` 不通，优先检查端口是否被本机其他进程占用。
- 当前 `smoke-test.sh` 还会额外校验 `GET /v2/consensus/status`，因此如果基础健康探针正常但 smoke 仍失败，需要继续检查共识后端是否已正确装配进 RPC。

## 5. Step C: 合约演练

执行：

```bash
./scripts/contract-smoke-test.sh
```

脚本内部会依次执行：

1. `nexus-wallet move build`
2. `nexus-wallet move deploy`
3. `nexus-wallet move call --function counter::initialize`
4. `nexus-wallet move call --function counter::increment`
5. `POST /v2/contract/query`

当前脚本还会对 deploy / initialize / increment 三类交易回执执行额外断言：`gas_used` 必须大于 `0`，用于显式覆盖默认 `move-vm` 执行路径。

成功时的标准输出示例：

```text
=== Contract Smoke Test ===
  RPC URL:    http://localhost:8080
  Key dir:    ./devnet/validator-0/keys
  Counter:    ./contracts/examples/counter
  Wallet CLI: ./target/debug/nexus-wallet move

--- Step 0: Checking node readiness ---
  Node is ready

--- Step 1: Building counter contract ---
  Counter contract built

--- Step 2: Deploying counter contract ---
  Deploy output: Submitted publish tx: <tx-digest>
  Contract address: 0x<64-hex>

--- Step 3: Initializing counter ---
  Init output: Submitted call tx: <tx-digest>

--- Step 4: Incrementing counter ---
  Increment output: Submitted call tx: <tx-digest>

--- Step 5: Querying counter value ---
  Query result: { ... }

=== Contract Smoke Test Complete ===
  Build:      OK
  Deploy:     0x<64-hex>
  Initialize: attempted
  Increment:  attempted
  Query:      { ... }
```

排障说明：

- 若看到 `Error: jq is required but not installed`，先安装 `jq`。
- 若看到 `Error: node not ready`，先执行 `./scripts/smoke-test.sh`，确认 devnet 已正常起来。
- 若 build 失败，先单独执行：

```bash
./target/debug/nexus-wallet move build --package-dir contracts/examples/counter --named-addresses counter_addr=0xCAFE --skip-fetch
```

- 若 deploy 失败，优先检查 `devnet/validator-0/keys/dilithium-secret.json` 是否存在。
- 若 `Contract address: unknown`，说明 deploy 输出没有成功解析出地址，先检查 deploy 输出中的 `tx digest` 和节点日志。
- 若脚本报 `non-positive gas_used=0`，说明默认执行路径又回退到了占位 gas 或交易回执查询链路异常，应立即检查 `GET /v2/tx/<digest>/status` 返回值和最近执行层改动。
- 若 query 返回 `{}` 或接口失败，说明当前节点的 query/read path 还未完全接线，先把它视为执行路径问题，而不是 CLI 构建问题。

## 6. 最短人工排查顺序

本地演练失败时，建议始终按这个顺序缩小范围：

1. `cargo build -p nexus-wallet`
2. `./scripts/setup-devnet.sh -o devnet -f`
3. `docker compose up -d`
4. `./scripts/smoke-test.sh`
5. `./scripts/contract-smoke-test.sh`
6. `docker compose logs -f nexus-node-0`

## 7. 结论

如果 `setup-devnet.sh` 成功、`smoke-test.sh` 通过、`contract-smoke-test.sh` 至少走通 build/deploy/call 路径，那么当前本地开发演练链路就是可复制的。