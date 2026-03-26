# Staking 与 Committee Rotation 运维手册

> **版本:** v0.1.13  
> **受众:** 节点运维、发布工程、值班工程师  
> **依据:** 当前 `v0.1.13` 代码与运维基线  
> **前置阅读:** `Docs/zh/Ops/Epoch_Operations_Runbook.md`

---

## 0. 术语

| 术语 | 定义 |
|------|------|
| **Staking Snapshot** | 在 epoch 边界从 committed state 读取的只读 staking 快照，包含所有 validator 的 bonded stake、penalty、status |
| **Election** | 从 staking snapshot 确定性推导下一届 committee 的过程；纯函数，无随机性 |
| **CommitteeRotationPolicy** | 控制轮换时机和门槛的策略配置：选举间隔、最小委员会/stake 门槛、slash 排除 |
| **RotationOutcome** | 轮换结果：`Elected`（成功选举）/ `NotElectionEpoch`（非选举边界）/ `Fallback`（选举失败，沿用当前委员会）|
| **ValidatorIdentityRegistry** | 内存中 `AccountAddress → FalconVerifyKey` 映射，持久化到 RocksDB |
| **PersistedElectionResult** | BCS 序列化的选举结果，写入 epoch store 供冷启动恢复 |

---

## 1. 架构概览

```
  ┌─────────────┐    epoch boundary     ┌───────────────────┐
  │ Committed   ├──────────────────────►│ Snapshot Provider │
  │ State (BCS) │                       │  (cf_state scan)  │
  └─────────────┘                       └────────┬──────────┘
                                                 │ StakingSnapshot
                                                 ▼
  ┌─────────────┐    policy + epoch     ┌───────────────────┐
  │ Rotation    ├──────────────────────►│ attempt_rotation  │
  │ Policy      │                       │  (pure function)  │
  └─────────────┘                       └────────┬──────────┘
                                                 │ RotationOutcome
                                                 ▼
  ┌──────────────────┐  Elected         ┌───────────────────┐
  │ Epoch Store      │◄────────────────│ Execution Bridge   │
  │ (persist result) │                  │  (wire to engine) │
  └──────────────────┘                  └───────────────────┘
```

**关键不变量：**

1. 选举是确定性的 — 同一 snapshot + 同一 policy → 所有节点得出同一 committee。
2. 选举结果与 epoch transition 一同持久化 — 冷启动可恢复。
3. 选举失败时安全降级 — 沿用当前 committee，不会产生空 committee。

---

## 2. 配置参数

以下配置字段控制 staking 和轮换行为（`nexus-config` → `ConsensusConfig`）：

| 字段 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `validator_election_epoch_interval` | `u64` | 1 | 每 N 个 epoch 触发一次选举。0 视为 1。 |
| `epoch_length_rounds` | `u64` | 1000 | 每个 epoch 包含的共识 round 数 |
| `slashing_double_sign_pct` | `u8` | 50 | 双签惩罚百分比 |
| `slashing_offline_pct` | `u8` | 10 | 长时间离线惩罚百分比 |
| `reputation_window_rounds` | `u64` | 300 | 声誉评分滑动窗口大小 |
| `reputation_decay` | `f64` | 0.99 | 声誉指数衰减因子 |

**选举安全阈值（硬编码）：**

| 常量 | 值 | 说明 |
|------|-----|------|
| `MIN_COMMITTEE_SIZE` | 4 | 选举最小委员会人数 |
| `MIN_TOTAL_EFFECTIVE_STAKE` | 4 NXS (4×10⁹ voo) | 选举最小总有效 stake |
| `MIN_ELIGIBLE_STAKE` | 1 NXS (1×10⁹ voo) | 单个 validator 最小有效 stake |

---

## 3. 查询接口

### 3.1 查看最近一次选举结果

```bash
curl -s http://<node>:8080/v2/consensus/election/latest | jq .
```

返回：`for_epoch`、`snapshot_epoch`、`elected`（地址+有效stake+索引）、`total_effective_stake`、`is_fallback`。

### 3.2 查看当前轮换策略

```bash
curl -s http://<node>:8080/v2/consensus/rotation-policy | jq .
```

返回：`election_epoch_interval`、`min_committee_size`、`max_committee_size`、`min_total_effective_stake`、`exclude_slashed`、`min_reputation_score`。

### 3.3 查看 Staking 快照

```bash
curl -s http://<node>:8080/v2/staking/validators | jq .
```

返回所有 validator 的 staking 信息：`address`、`effective_stake`、`bonded`、`penalty_total`、`status`、`is_slashed`、`reputation`。

### 3.4 查看共识状态（含 epoch）

```bash
curl -s http://<node>:8080/v2/consensus/status | jq .
```

### 3.5 查看 epoch 历史

```bash
curl -s http://<node>:8080/v2/admin/epoch/history | jq '.transitions[] | {epoch, trigger, committee_size}'
```

---

## 4. 正常轮换流程

### 4.1 自动轮换（生产路径）

1. 共识引擎在 `epoch_length_rounds` 个 round 后推进 epoch。
2. Execution bridge 调用 snapshot provider 读取当前 committed state 中的 staking BCS。
3. `attempt_rotation(snapshot, policy, next_epoch)` 判断是否在选举边界：
   - `next_epoch % interval == 0` → 选举边界
   - epoch 0 永不选举（使用创世 committee）
4. 如果在选举边界，`elect_committee_with_policy()` 运行确定性选举：
   - 排除 slashed validator（若 `exclude_slashed = true`）
   - 排除声誉低于阈值的 validator
   - 按 effective stake DESC、address bytes ASC 排序
   - 截取 `max_committee_size` 个 validator
   - 检查 `min_committee_size` 和 `min_total_effective_stake` 安全阈值
5. 选举结果写入 epoch store。
6. 新 committee 在下一 epoch 生效。

### 4.2 验证轮换成功

```bash
# 检查最新选举
curl -s http://localhost:8080/v2/consensus/election/latest | jq '{for_epoch, elected_count: (.elected | length), is_fallback}'

# 跨节点一致性
for i in $(seq 0 6); do
  echo "Node $i: $(curl -s http://localhost:$((8080+i))/v2/consensus/election/latest | jq -r '.for_epoch')"
done
```

**确认条件：**
- `for_epoch` 等于当前 epoch。
- `is_fallback` 为 false。
- 所有节点返回相同的 `for_epoch` 和 `elected` 集合。
- `elected` 中的 validator 均为非 slashed、有效 stake ≥ 1 NXS。

---

## 5. 降级与 Fallback 处理

### 5.1 选举失败（Fallback）

**触发条件：**
- 可用 validator 不足 `MIN_COMMITTEE_SIZE`（4 个）
- 总有效 stake 不足 `MIN_TOTAL_EFFECTIVE_STAKE`（4 NXS）
- Snapshot 不可用（合约未部署 / state 损坏）

**系统行为：**
- `attempt_rotation` 返回 `Fallback { reason }`。
- 当前 committee 原样沿用到下一 epoch。
- 日志输出 `WARN` 级别降级原因。

**运维响应：**

```bash
# 确认 fallback 状态
curl -s http://localhost:8080/v2/consensus/election/latest | jq '.is_fallback'

# 检查原因：validator 不足
curl -s http://localhost:8080/v2/staking/validators | jq '[.validators[] | select(.status == 0)] | length'

# 检查原因：总 stake 不足
curl -s http://localhost:8080/v2/staking/validators | jq '[.validators[] | select(.status == 0)] | map(.effective_stake) | add'
```

**恢复步骤：**
1. 注册新 validator 或 bond 追加 stake 使其达到阈值。
2. 触发 manual epoch advance，使下一次选举重新运行。
3. 确认 `is_fallback` 变为 false。

### 5.2 非选举 Epoch（NotElectionEpoch）

当 `epoch % interval ≠ 0` 时，不触发选举，当前 committee 自动延续。这是正常行为，非异常。

---

## 6. Slash 对轮换的影响

### 6.1 Slash → Rotation 联动

1. 执行 slash（`POST /v2/admin/validator/slash`）。
2. Slashed validator 的 `is_slashed = true`。
3. 下一次选举时，`exclude_slashed = true`（默认）排除该 validator。
4. 若 slash 导致候选不足触发 fallback，当前 committee 沿用。

### 6.2 验证 Slash 效果

```bash
# Slash 后立即检查
curl -s http://localhost:8080/v2/staking/validators | jq '.validators[] | select(.is_slashed == true)'

# 触发 epoch advance
curl -X POST http://localhost:8080/v2/admin/epoch/advance

# 确认下一届 committee 不包含被 slash 的 validator
curl -s http://localhost:8080/v2/consensus/election/latest | jq '.elected[].address_hex'
```

---

## 7. 冷启动恢复

### 7.1 恢复流程

节点冷启动时：
1. 从 RocksDB 加载当前 epoch 和 committee（`EpochStore`）。
2. 加载已持久化的选举结果（`PersistedElectionResult`）。
3. 从 `cf_state` 扫描加载 validator identity registry。
4. 验证已加载的 committee 与持久化选举结果一致。
5. 重建 snapshot provider 和 rotation policy。

### 7.2 验证恢复正确性

```bash
# 重启节点
docker compose stop nexus-node-<N>
docker compose start nexus-node-<N>

# 等待 ready
until curl -sf http://localhost:<port>/ready > /dev/null; do sleep 1; done

# 验证 epoch 和 committee 未漂移
curl -s http://localhost:<port>/v2/consensus/epoch | jq .
curl -s http://localhost:<port>/v2/consensus/election/latest | jq '{for_epoch, elected_count: (.elected | length)}'
```

### 7.3 已知限制

- 创世（epoch 0）不运行选举 — 使用静态创世 committee。
- 选举结果依赖 committed state，若 state 损坏则 snapshot 不可用，触发 fallback。

---

## 8. 创世 Staking 初始化

### 8.1 创世流程

`genesis_boot::boot_from_genesis()` 执行以下步骤：

1. 加载 `genesis.json` 中的 validator 配置。
2. 为每个 validator 写入 41 字节 BCS `ValidatorStake` 记录到 `cf_state`。
3. 写入 genesis 标记和 token 分配——全部在同一原子 batch 中。
4. 构建初始 committee。

### 8.2 BCS 布局（41 字节）

| 偏移 | 长度 | 字段 |
|------|------|------|
| 0 | 8 | `bonded` (u64 LE) |
| 8 | 8 | `penalty_total` (u64 LE) |
| 16 | 1 | `status` (u8: 0=Active, 1=Unbonding, 2=Withdrawn) |
| 17 | 8 | `registered_epoch` (u64 LE) |
| 25 | 8 | `unbond_epoch` (u64 LE) |
| 33 | 8 | `metadata_tag` (u64 LE) |

### 8.3 Resource Key 格式

```
shard_prefix (4 bytes) + address (32 bytes) + resource_tag (0xCAFE::staking::ValidatorStake)
```

---

## 9. 监控检查清单

Staking + rotation 切换前后应检查：

| 检查项 | 命令/端点 | 预期 |
|--------|-----------|------|
| 最近选举非 fallback | `/v2/consensus/election/latest` → `is_fallback` | false |
| 选举 epoch 与当前 epoch 一致 | `/v2/consensus/election/latest` → `for_epoch` | 等于当前 epoch |
| 跨节点选举一致 | `/v2/consensus/election/latest` × N 节点 | 所有节点相同 |
| 有效 validator 数 ≥ 4 | `/v2/staking/validators` | `status=0` 且 `effective_stake ≥ 1 NXS` 的数量 ≥ 4 |
| 总有效 stake ≥ 4 NXS | `/v2/staking/validators` | 有效 stake 总和 ≥ 4×10⁹ |
| 轮换策略已生效 | `/v2/consensus/rotation-policy` → `election_epoch_interval` | 与配置一致 |
| Epoch 正在推进 | `/v2/consensus/status` → `epoch` | 持续增长 |
| 所有节点 ready | `/ready` × N | 200 |

---

## 10. 故障排查速查

| 症状 | 可能原因 | 处理 |
|------|----------|------|
| `is_fallback: true` | validator 不足或 stake 不足 | 检查 `/v2/staking/validators`，注册新 validator 或追加 stake |
| 跨节点 committee 不一致 | 非确定性 bug 或网络分区 | 对比所有节点的 `/v2/consensus/election/latest`，检查 state root 一致性 |
| 冷启动后 committee 与重启前不同 | 选举结果未持久化 | 检查 epoch store 写入日志，确认 `PersistedElectionResult` 存在 |
| 选举间隔行为与配置不符 | `validator_election_epoch_interval` 配置错误 | 查看 `/v2/consensus/rotation-policy`，对比配置文件 |
| Slashed validator 仍出现在新 committee | `exclude_slashed` 未启用或 slash 未在 epoch advance 前执行 | 确认策略中 `exclude_slashed = true`，slash 后触发 epoch advance |

---

## 11. 升级注意事项（v0.1.8 → v0.1.9）

1. **新增 RPC 端点**：`/v2/consensus/election/latest`、`/v2/consensus/rotation-policy`、`/v2/staking/validators`。
2. **创世变更**：`boot_from_genesis` 现会写入 staking BCS 记录。升级节点首次从新创世启动时自动生效。
3. **存储新增**：`cf_state` 中增加每个 validator 的 staking resource key。
4. **Identity registry**：validator 公钥映射持久化到 RocksDB，冷启动自动恢复。
5. **配置检查**：确认 `validator_election_epoch_interval` 已按需设置（默认值 1 = 每 epoch 选举）。
6. **向后兼容**：无创世数据的旧节点重启时，snapshot provider 返回 `None`，选举回退到 fallback。

---

## 12. 多分片环境补充（v0.1.10+）

### 12.1 Staking 与 Shard 的关系

- Staking 数据（`ValidatorStake` BCS）仍存储在主分片（shard 0）的 `cf_state` 中。
- 选举逻辑从主分片 committed state 读取 snapshot，不依赖其他分片状态。
- 各分片共享同一 committee — 当前不支持 per-shard committee。

### 12.2 多分片 devnet 中的轮换操作

```bash
# 查看各节点的分片信息
for i in $(seq 0 6); do
  echo "Node $i shards: $(curl -s http://localhost:$((8080+i))/v2/shards | jq -r '.shards | length')"
done

# 选举结果在所有分片上一致
for i in $(seq 0 6); do
  echo "Node $i election: $(curl -s http://localhost:$((8080+i))/v2/consensus/election/latest | jq -r '.for_epoch')"
done
```

### 12.3 注意事项

| 场景 | 注意 |
|------|------|
| **跨分片 HTLC** | HTLC lock 存储在 `cf_htlc_locks`，不影响 staking snapshot |
| **分片故障** | 非主分片故障不影响选举；主分片故障导致 snapshot 不可用，触发 fallback |
| **分片扩容** | 新增分片不需要重新选举；分片路由由 `ShardRouter` 管理，独立于 staking |
| **创世多分片** | `setup-devnet.sh -s <N>` 生成多分片创世，staking 记录仅写入 shard 0 |
