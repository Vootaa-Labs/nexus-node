# Epoch 切换操作手册与故障回退策略

> **版本:** v0.1.13  
> **受众:** 节点运维、发布工程、值班工程师  
> **依据:** 当前仓库 `v0.1.13` 基线的 Phase B-4 epoch 切换能力

---

## 0. 术语

| 术语 | 定义 |
|------|------|
| **Epoch** | 验证人集合和共识参数保持一致的连续区间；epoch 切换意味着新集合生效 |
| **Committee** | 当前 epoch 的活跃验证人列表（含 stake 与 reputation） |
| **EpochTransition** | 记录从旧 epoch 到新 epoch 的元数据：触发类型、旧/新 epoch、committee 快照 |
| **EpochStore** | 基于 RocksDB 的持久化层，保存当前 epoch、committee、transition 历史 |
| **EpochManager** | 管理 epoch 生命周期的组件：时间/手动触发判断、状态持久化、恢复 |
| **Slash** | 将某个验证人标记为已惩罚（reputation 降至 0），仅内存修改，需显式 persist 后生效于重启 |

---

## 1. 正常 Epoch 切换流程

### 1.1 时间触发（自动）

1. `EpochManager::should_advance(now, duration)` 比较当前时间与上次 epoch 开始时间。
2. 若超过配置的 epoch duration，ConsensusEngine 调用 `advance_epoch()`。
3. `advance_epoch()` 创建新 Committee（epoch + 1），将旧 committee 转为下一 epoch 验证人。
4. `EpochManager::record_transition(trigger, committee)` 写入 `EpochTransition` 并持久化。
5. EpochStore 写入以下键值：
   - `__epoch_current__` → 新 epoch number
   - `__epoch_started_at__` → 当前时间戳
   - `__epoch_committee__:{epoch}` → BCS 序列化的 committee
   - `__epoch_transition__:{epoch}` → BCS 序列化的 transition 记录

### 1.2 手动触发（管理员 RPC）

```bash
curl -X POST http://<node>:8080/v2/admin/epoch/advance
```

- 仅限开发/测试网环境。
- 立即触发 epoch 切换，不等待时间阈值。
- 返回新 epoch 信息或错误。

### 1.3 验证切换成功

```bash
# 查询当前 epoch
curl -s http://<node>:8080/v2/consensus/epoch | jq .

# 查看 epoch 历史
curl -s http://<node>:8080/v2/admin/epoch/history | jq .

# 检查验证人列表
curl -s http://<node>:8080/v2/validators | jq .
```

**确认条件：**

- `epoch` 字段递增 1。
- `validators` 列表与预期一致。
- 所有节点 epoch 值一致（网络一致性）。

---

## 2. 治理操作

### 2.1 Slash 验证人

```bash
curl -X POST http://<node>:8080/v2/admin/validator/slash \
  -H "Content-Type: application/json" \
  -d '{"validator_index": <index>}'
```

**重要注意事项：**

- Slash 仅修改内存中的 Committee 状态（`reputation = 0`，`is_slashed = true`）。
- **当前实现中 slash 不自动持久化。** 节点重启后 slash 丢失——这是已知设计间隙（见 `governance_recovery_tests::slash_lost_without_re_persist`）。
- 若需 slash 在重启后生效，必须在 slash 后触发 epoch advance，使新 epoch 的 committee 包含 slash 结果，该新 committee 会由 EpochStore 持久化。

### 2.2 Slash 持久化最佳实践

1. 执行 slash。
2. 立即触发 manual epoch advance（如果不想等自动触发）。
3. 通过 `/v2/admin/epoch/history` 确认新 epoch 的 committee 已包含 slash 结果。
4. 在至少一个其他节点上验证 epoch 一致性。

### 2.3 查看 Epoch 历史

```bash
curl -s http://<node>:8080/v2/admin/epoch/history | jq '.transitions[] | {epoch, trigger, committee_size}'
```

---

## 3. 节点重启与恢复

### 3.1 单节点重启

```bash
# 停止单个节点
docker compose stop nexus-node-<N>

# 重启
docker compose start nexus-node-<N>

# 验证恢复
curl -s http://localhost:<port>/ready
curl -s http://localhost:<port>/v2/consensus/epoch
```

**预期行为：**

- EpochManager 从 EpochStore 恢复当前 epoch 和 committee。
- 节点应在 `/ready` 返回 200 后的数秒内追上网络 epoch。
- 若网络在节点停机期间发生了 epoch 切换，节点重启后通过状态同步追上。

### 3.2 多节点同时重启

**不推荐**——可能导致共识中断。若必须执行：

1. 每次最多重启 f 个节点（7 节点网络中 f = 2）。
2. 等第一批恢复完毕（`/ready` 返回 200 且 epoch 一致）后再重启下一批。
3. Bootstrap 节点最后重启。

### 3.3 全网重启（冷启动）

仅在以下场景使用：

- 创世重置
- 不可恢复的状态损坏
- 重大协议升级

```bash
# 停止所有节点
docker compose down

# 可选：清除状态
make devnet-clean

# 重新生成创世
make devnet-setup

# 启动
make devnet-up

# 验证
scripts/validate-startup.sh -n 7 -t 90
```

---

## 4. 常见故障与回退

### 4.1 Epoch 不一致（节点间 epoch 不同步）

**症状：** 不同节点 `/v2/consensus/epoch` 返回不同 epoch。

**诊断：**

```bash
for i in $(seq 0 6); do
  echo "Node $i: $(curl -s http://localhost:$((8080+i))/v2/consensus/epoch | jq -r .epoch)"
done
```

**处理步骤：**

1. 确认多数节点（≥ 2f+1 = 5）在同一 epoch。
2. 重启落后的节点——它们会通过状态同步追上。
3. 若多数节点在不同 epoch：检查日志 `WARN` / `ERROR` 级别关于 epoch 的条目。
4. 最坏情况：从最新一致快照恢复（见 §4.5）。

### 4.2 Epoch 切换后共识停摆

**症状：** `total_commits` 不再增长；`/v2/consensus/status` 中 `pending_commits > 0` 持续增长。

**诊断：**

```bash
curl -s http://localhost:8080/v2/consensus/status | jq '{epoch, dag_size, total_commits, pending_commits}'
# 等 10s 后再查，比较 total_commits 是否增长
```

**处理步骤：**

1. 检查 validator 列表是否有足够非 slash 验证人（需 2f+1 = 5）。
2. 若 slash 过多导致低于 quorum：
   - **不可自动恢复**——需要人工干预。
   - 选项 A：执行全网重启并从创世恢复。
   - 选项 B：手动修改配置增加新验证人。
3. 检查网络连通性——`/v2/network/status` 中 `routing_healthy` 是否为 true。

### 4.3 Slash 后重启导致状态回退

**症状：** 节点重启后，之前 slash 的验证人恢复为非 slash 状态。

**根因：** Slash 仅修改内存，未触发 epoch advance 持久化。

**预防：** 每次 slash 后立即触发 manual epoch advance（见 §2.2）。

**回退：** 重新执行 slash + epoch advance。

### 4.4 EpochStore 损坏

**症状：** 节点启动时日志报 `epoch store` 相关错误。

**处理步骤：**

1. 停止受影响节点。
2. 检查数据目录磁盘空间和 I/O 错误。
3. 从其他健康节点的快照恢复：
   ```bash
   # 在健康节点导出快照
   curl -X POST http://<healthy-node>:8080/v2/admin/snapshot/export -o snapshot.tar
   
   # 在故障节点导入快照
   curl -X POST http://<failed-node>:8080/v2/admin/snapshot/import \
     -H "Content-Type: application/octet-stream" \
     --data-binary @snapshot.tar
   ```
4. 重启并验证 epoch 一致性。

### 4.5 从快照恢复

当常规恢复无法解决问题时：

```bash
# 1. 停止故障节点
docker compose stop nexus-node-<N>

# 2. 清除该节点数据
rm -rf devnet-n7s/validator-<N>/data/*

# 3. 重启节点（将通过状态同步从网络恢复）
docker compose start nexus-node-<N>

# 4. 等待同步完成
scripts/validate-startup.sh -n 1 -t 120
```

---

## 5. 监控检查清单

epoch 切换前后应检查以下指标：

| 检查项 | 命令/端点 | 预期 |
|--------|-----------|------|
| 所有节点 epoch 一致 | `/v2/consensus/epoch` × N | 相同 epoch |
| 共识正在推进 | `/v2/consensus/status` → `total_commits` | 持续增长 |
| 验证人状态正确 | `/v2/validators` | 正确的 slash 状态 |
| 节点全部就绪 | `/ready` × N | 200 |
| 无待处理积压 | `/v2/consensus/status` → `pending_commits` | 接近 0 |
| Proof 端点可用 | `/v2/state/commitment` | 200 + 非空 |
| 网络路由健康 | `/v2/network/status` → `routing_healthy` | true |
| Metrics 正常 | `/metrics` | `rpc_requests_total` 在增长 |

---

## 6. 操作日志模板

每次 epoch 操作应记录：

```
日期: YYYY-MM-DD HH:MM UTC
操作: [epoch advance / slash / restart / rollback]
操作人: <operator>
节点范围: [全网 / node-N]
操作前 epoch: <N>
操作后 epoch: <N+1>
验证结果: [通过 / 异常 — 描述]
备注: <异常处理细节>
```

---

## 7. 升级与协议变更期间的 Epoch 处理

若发布涉及共识或 epoch 逻辑变更：

1. **先在 staging 环境演练至少 10 个 epoch 切换**（包含 slash + restart 组合）。
2. 确认 `governance_recovery_tests` 和 `epoch_stress_tests` 全部通过。
3. 滚动升级期间不触发手动 epoch advance。
4. 升级完成后，观察至少 3 个自然 epoch 切换无异常。

---

## 8. 冷重启持久化恢复（v0.1.6 新增）

> **背景：** v0.1.6 为 DAG 证书（`cf_certificates`）和 BatchStore（`cf_batches`）新增了 write-through 持久化层。冷重启后节点可从本地磁盘自主恢复共识 DAG 和 batch 映射，不再完全依赖对等节点。

### 8.1 恢复覆盖范围

| 组件 | 列族 | 恢复内容 | 限制 |
|------|------|----------|------|
| 共识 DAG | `cf_certificates` | 所有 epoch retention window 内的证书 | 超出 `epoch_retention_count` 窗口的旧 epoch 证书已被清理 |
| BatchStore | `cf_batches` | 所有通过 write-through 写入的 batch → 交易映射 | 已被执行桥消费并 `remove()` 的 batch 不在磁盘上 |
| Epoch 状态 | `cf_epoch_store` | 当前 epoch、committee、transition 历史 | 已有（v0.1.5 功能） |

### 8.2 单节点冷重启恢复流程

```bash
# 1. 停止节点
docker compose stop nexus-node-<N>

# 2. 重启节点（持久化数据保留）
docker compose start nexus-node-<N>

# 3. 验证恢复
curl -s http://localhost:<port>/ready           # 应返回 200
curl -s http://localhost:<port>/v2/consensus/epoch | jq .
```

**节点启动时的自动恢复序列：**

1. `DagPersistence::restore_certificates()` — 扫描 `cf_certificates`，按 `(round, origin)` 排序，返回证书列表。
2. 若证书列表非空 → 跳过 genesis 注入，通过 `insert_verified_certificate()` 重放证书到 DAG。
3. 若证书列表为空 → 正常执行 genesis 注入流程。
4. `BatchStore::restore_from_disk()` — 扫描 `cf_batches`，将所有 batch 加载到 DashMap。
5. 节点日志输出恢复的证书数和 batch 数。

### 8.3 验证冷重启恢复成功

```bash
# 检查 DAG 大小（应 > 0 如果之前有共识活动）
curl -s http://localhost:<port>/v2/consensus/status | jq '.dag_size'

# 检查 epoch 与其他节点一致
for i in $(seq 0 6); do
  echo "Node $i: $(curl -s http://localhost:$((8080+i))/v2/consensus/epoch | jq -r .epoch)"
done

# 检查共识是否恢复推进（等待 10s 比较 total_commits）
c1=$(curl -s http://localhost:<port>/v2/consensus/status | jq '.total_commits')
sleep 10
c2=$(curl -s http://localhost:<port>/v2/consensus/status | jq '.total_commits')
echo "Commits: $c1 → $c2 (should increase)"
```

### 8.4 Epoch 保留窗口与磁盘清理

- 配置项: `storage.epoch_retention_count`（默认 100）。
- 当 epoch 从 N 推进到 N+1 时，epoch `N - epoch_retention_count` 的证书会被自动清理。
- 例如：`epoch_retention_count=100`，从 epoch 150 → 151 时，epoch 50 的证书被清除。
- 若设为 0：每次 epoch 推进时删除当前 epoch 的全部证书（旧行为）。

**调优建议：**

- 生产环境建议保留至少 50 个 epoch 的证书以支持审计和回溯。
- 磁盘空间紧张时可降低该值，但不宜低于 10。
- 修改后需重启节点生效。

### 8.5 故障场景与处理

#### 恢复的证书数为 0（预期有数据）

**可能原因：** 数据目录被清除、`epoch_retention_count=0` 导致证书被清理。

**处理：**

1. 检查数据目录 `devnet-n7s/validator-<N>/data/` 是否存在 RocksDB 文件。
2. 检查日志是否有 `DAG restore failed` 警告。
3. 若数据确实丢失：节点将以空 DAG 启动，通过 genesis 注入和对等同步恢复。

#### BatchStore 恢复丢失 "已提交未执行" batch

**可能原因：** 执行桥在崩溃前已 `remove()` 该 batch（正常行为），或 write-through 写入失败。

**处理：**

1. 检查日志是否有 `batch persist failed` 错误。
2. 若 batch 确实丢失：依赖对等节点的 batch 重传机制（共识层会在需要时重新请求）。

#### 恢复后 epoch 与网络不一致

**处理：** 同 §4.1（epoch 不一致处理步骤）。冷重启恢复只恢复本地数据，网络状态同步由状态同步协议完成。

### 8.6 CI 验证

冷重启恢复路径由以下自动化测试覆盖，纳入 CI 门禁：

| 测试模块 | 测试数 | 覆盖 |
|----------|--------|------|
| `cold_restart_tests` | 5 | 端到端 DAG+BatchStore 管道冷重启、mid-epoch 崩溃、多 epoch retention 清理、committed-unexecuted 恢复、空库首启 |
| `persistence_tests` | 9 | DAG write-through、崩溃恢复、跨 epoch 清理、保留窗口、BatchStore write-through/恢复/淘汰/committed-unexecuted |

CI job: `cold-restart-recovery` (Gate 3j)
Go/No-Go: Check 12 (`cold_restart_recovery`), Check 13 (`persistence_tests`)

---

## 附录 A：Staking 驱动的委员会轮换（v0.1.9+）

v0.1.9 引入链上 staking 合约后，epoch 切换时的委员会选举不再仅基于静态配置，而是由链上 staking 状态驱动。

### A.1 轮换流程

1. 每个 epoch 结束时，`EpochManager` 读取链上 staking 合约的验证人权重。
2. 根据选举策略（最小 stake 阈值、最大验证人数量）计算下一 epoch 的委员会。
3. 新委员会写入 `EpochStore`，所有节点在 epoch 边界同步。

### A.2 运维操作

```bash
# 查询当前验证人 stake 权重
curl -sf http://<node>:8080/v2/staking/validators | jq '.validators[] | {address, stake, is_active}'
```

### A.3 多分片环境下的注意事项（v0.1.10+）

- 所有分片共享同一个全局委员会。
- epoch 切换对所有分片同时生效。
- 分片间的 epoch 一致性通过共识层保证。

### A.4 详细操作流程

完整的 staking 与 rotation 操作手册请参阅 `Docs/zh/Ops/Staking_Rotation_Runbook.md`。

---

## 附录 B：关键 RPC 端点速查

| 端点 | 方法 | 说明 |
|------|------|------|
| `/v2/consensus/epoch` | GET | 当前 epoch 信息 |
| `/v2/consensus/status` | GET | 共识引擎状态 |
| `/v2/validators` | GET | 验证人列表 |
| `/v2/validators/:index` | GET | 单个验证人详情 |
| `/v2/admin/epoch/history` | GET | Epoch 转换历史 |
| `/v2/admin/epoch/advance` | POST | 手动触发 epoch 切换 |
| `/v2/admin/validator/slash` | POST | Slash 验证人 |
| `/v2/staking/validators` | GET | 验证人 stake 权重列表（v0.1.9+） |
| `/v1/shards` | GET | 分片列表（v0.1.10+） |
| `/v2/state/commitment` | GET | 状态承诺摘要 |
| `/v2/chain/head` | GET | 最新提交区块 |
| `/ready` | GET | 就绪状态探针 |
| `/health` | GET | 健康状态探针 |
