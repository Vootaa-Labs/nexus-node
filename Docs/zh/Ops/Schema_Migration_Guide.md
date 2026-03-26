# Nexus 数据库 Schema 迁移指南

> **版本:** v0.1.13  
> **受众:** 节点运维人员、发布工程师  
> **依据:** 当前 `v0.1.13` RocksDB 持久化基线

---

## 1. 概述

Nexus 使用 RocksDB 作为持久化存储，数据按 **Column Family（CF）** 隔离。
Schema 版本由 `__nexus_schema_version__` 键（存储在 `cf_state`）追踪，
节点每次启动时自动运行 `crates/nexus-storage/src/rocks/migration.rs` 中的迁移逻辑。

**当前 Schema 版本: v3**

---

## 2. Column Family 布局

### 2.1 完整列表（12 个 CF，FROZEN-3）

| CF 名称 | 引入版本 | 用途 | 压缩 | 键模式 |
|---------|----------|------|------|--------|
| `cf_blocks` | v1 | 区块头: `CommitSequence → BlockHeader (BCS)` | LZ4 | 顺序递增 |
| `cf_transactions` | v1 | 原始交易: `TxDigest → SignedTransaction (BCS)` | LZ4 | 随机分布 |
| `cf_receipts` | v1 | 收据: `TxDigest → TransactionReceipt (BCS)` | Zstd | 随机分布 |
| `cf_state` | v1 | 全局状态: `AccountKey(34B) \| ResourceKey(66B) → AccountState` | LZ4 | 分片前缀 |
| `cf_certificates` | v1 | 共识证书: `CertDigest → NarwhalCertificate (BCS)` | LZ4 | epoch 内临时 |
| `cf_batches` | v2 | 批次载荷: `BatchDigest → Vec<SignedTransaction> (BCS)` | LZ4 | 共识→执行桥接 |
| `cf_sessions` | v2 | Agent 会话: `SessionId → AgentSession (BCS)` | LZ4 | 读多写少 |
| `cf_provenance` | v2 | 审计日志: `ProvenanceId → ProvenanceRecord (BCS)` | Zstd | 写一次 |
| `cf_commitment_meta` | v3 | 承诺树元数据: 活跃版本、根、叶计数 | LZ4 | 单一标记键 |
| `cf_commitment_leaves` | v3 | 承诺树叶节点: 排序叶 + key→index 查找 | LZ4 | 版本化覆盖 |
| `cf_commitment_nodes` | v3 | Merkle 内部节点: `(level, index) → Blake3Digest` | LZ4 | O(log n) 每次变更 |
| `cf_htlc_locks` | v3† | HTLC 锁记录: `HtlcLockId → HtlcLock (BCS)` | LZ4 | 跨分片 claim/refund |

> † `cf_htlc_locks` 在 v0.1.10 中添加，未触发 schema 版本变更（仍为 v3），通过 `create_missing_column_families` 透明创建。

### 2.2 承诺树持久化 Schema（v3 新增）

承诺树使用三个 CF 协同工作：

```
cf_commitment_meta
  └─ ACTIVE_TREE_KEY → CommitmentMetaRecord {
       layout_version: u32,
       tree_version: u64,
       root: Blake3Digest,
       leaf_count: u64,
       base_tree_version: u64,
     }

cf_commitment_leaves
  ├─ <key_bytes> → PersistedLeafRecord {
  │    leaf_index: u64,
  │    key: Vec<u8>,
  │    value: Vec<u8>,
  │    leaf_hash: Blake3Digest,   // domain-separated
  │  }
  └─ (LRU cache: commitment_cache_size 控制)

cf_commitment_nodes
  └─ (level: u32, index: u64) → Blake3Digest
```

- **版本化覆盖**: `base_tree_version` 链可追溯，LRU 清理不需全量重建索引。
- **原子写入**: 所有变更通过 RocksDB `WriteBatch` 原子提交，`ACTIVE_TREE_KEY` 最后写入。

---

## 3. 迁移历史

### v1 → v2（v0.1.5）

- **新增 CF**: `cf_batches`、`cf_sessions`、`cf_provenance`
- **迁移方式**: 元数据级 — RocksDB `create_missing_column_families` 在打开时自动创建新 CF
- **数据迁移**: 无（新 CF 初始为空）
- **回滚**: 安全 — 旧版本忽略新 CF，但无法读取在新版本中写入这些 CF 的数据

### v2 → v3（v0.1.8）

- **新增 CF**: `cf_commitment_meta`、`cf_commitment_leaves`、`cf_commitment_nodes`
- **迁移方式**: 元数据级 — 同上
- **承诺树初始化**: 节点首次以 v3 schema 启动时，承诺树为空（`canonical_empty_root`）。执行桥在处理后续批次时自动填充叶和内部节点
- **回滚**: 安全降级 — 旧版本忽略承诺 CF，但降级后丢失承诺树状态，升级回来需从创世重放

### v3 透明扩展：Staking BCS（v0.1.9）

- **变更类型**: 数据格式扩展（非 CF 新增，非 schema 版本变更）
- **内容**: `cf_state` 中增加每个 validator 的 `ValidatorStake` BCS 记录（41 字节），使用 `resource_tag = 0xCAFE::staking::ValidatorStake`
- **创世变更**: `boot_from_genesis()` 现在同时写入 staking 记录与 token 分配
- **持久化新增**: `PersistedElectionResult` BCS 写入 epoch store（现有 `cf_state` 键空间）
- **影响**: 无 schema 版本变更，无需迁移；旧版本节点不读取 staking 键，无冲突

### v3 透明扩展：cf_htlc_locks（v0.1.10）

- **新增 CF**: `cf_htlc_locks` — 存储跨分片 HTLC 锁的 BCS 记录
- **迁移方式**: 无显式迁移 — RocksDB `create_missing_column_families` 在 `RocksStore::open` 时自动创建
- **Schema 版本**: 仍为 v3（未触发版本号递增）
- **回滚**: 安全 — 旧版本忽略 `cf_htlc_locks`，无 HTLC 功能但不影响其他状态

---

## 4. 冷升级流程

适用于 devnet / testnet 版本升级（允许短暂停机）。

### 4.1 前置检查

```bash
# 确认当前版本可编译、所有测试通过
make test-all

# 运行 go/no-go 门禁
scripts/release-go-nogo.sh
```

### 4.2 升级步骤

```bash
# 1. 等待最近一个 epoch 结束，确保状态稳定
#    观察日志中 "epoch boundary check passed" 消息

# 2. 停止所有验证节点（先停非领导者，最后停领导者）
docker compose stop

# 3. 备份数据目录（可选但推荐）
for i in $(seq 0 6); do
  cp -r devnet-n7s/validator-$i/db devnet-n7s/validator-$i/db.bak
done

# 4. 更新镜像
docker compose pull
# 或重新构建
make devnet-build

# 5. 启动节点（先启动领导者，再启动其他节点）
docker compose up -d

# 6. 等待所有节点就绪
scripts/validate-startup.sh -n 7 -t 120

# 7. 运行冒烟测试验证功能完整
scripts/smoke-test.sh
scripts/contract-smoke-test.sh
```

### 4.3 迁移自动执行

节点启动时 `RocksStore::open` 会：
1. 读取 `__nexus_schema_version__`  
2. 逐步运行缺失的迁移（v1→v2→v3）  
3. 写入新版本号  
4. 全部在同一进程内完成，无需手动干预

### 4.4 验证迁移成功

```bash
# 检查节点日志中的迁移记录
docker logs nexus-validator-0 2>&1 | grep "schema migration"
# 预期输出（仅从旧版升级时出现）:
#   running schema migration from=2 to=3

# 确认承诺端点正常
curl -s http://localhost:8080/v2/state/commitment | jq .
# 预期: commitment_root 非空，entry_count ≥ 0
```

---

## 5. 热升级策略

当前 Nexus devnet **不支持真正的热升级**（zero-downtime rolling），原因：

1. 共识协议依赖 2f+1 验证人联合签名，滚动重启期间可能短暂低于法定人数。
2. Schema 迁移在节点启动时同步执行，新旧版本节点混合运行可能导致 CF 读写不一致。

**推荐的最小中断策略：**

1. 一次升级不超过 f 个节点（f = (N-1)/3），确保法定人数不中断。
2. 等待升级的节点完全追上链头（日志 "caught up to commit sequence"）后再升级下一批。
3. 7 节点集群可分 3 批：(0,1) → (2,3) → (4,5,6)，每批之间间隔至少 30 秒。

---

## 6. 回滚策略

### 6.1 安全回滚条件

- **元数据级迁移**（v1→v2, v2→v3）: 回滚安全。旧版本打开数据库时忽略未知 CF。
- **数据级迁移**（未来版本可能出现）: 需要预先备份，参考 §4.2 步骤 3。

### 6.2 回滚步骤

```bash
# 1. 停止所有节点
docker compose stop

# 2. 如果有备份，恢复数据目录
for i in $(seq 0 6); do
  rm -rf devnet-n7s/validator-$i/db
  cp -r devnet-n7s/validator-$i/db.bak devnet-n7s/validator-$i/db
done

# 3. 切换回旧版本镜像
docker compose up -d
```

### 6.3 无备份回滚

若没有提前备份，v3→v2 降级后：
- `cf_state`、`cf_blocks`、`cf_transactions` 等原始 CF 数据完整
- `cf_commitment_*` 三个 CF 的数据会被旧版本忽略
- 再次升级回 v3 后，承诺树需从当前状态重建（首批执行后自动完成）

---

## 7. 添加新迁移的开发指南

参考 `crates/nexus-storage/src/rocks/migration.rs` 头部注释：

```rust
// 1. 修改 CURRENT_SCHEMA_VERSION 递增
pub const CURRENT_SCHEMA_VERSION: SchemaVersion = SchemaVersion(4);

// 2. 在 run_migration() 中添加新分支
fn run_migration(store: &RocksStore, from: SchemaVersion) -> Result<(), StorageError> {
    match from.0 {
        1 => Ok(()),  // v1→v2
        2 => Ok(()),  // v2→v3
        3 => {        // v3→v4: 新迁移逻辑
            // 执行数据转换...
            Ok(())
        }
        other => Err(StorageError::Snapshot(format!(
            "no migration path from schema version {other}"
        ))),
    }
}
```

**原则：**
- 每个迁移必须**幂等**（崩溃后可安全重跑）
- 新增 CF → 元数据级迁移（RocksDB 自动创建），迁移函数返回 `Ok(())`
- 修改已有 CF 的数据 → 数据级迁移，需要实际读写操作
- 迁移完成后框架自动写入新版本号

---

## 附录：相关文件

| 文件 | 作用 |
|------|------|
| `crates/nexus-storage/src/rocks/migration.rs` | 迁移框架与步骤定义 |
| `crates/nexus-storage/src/types.rs` | ColumnFamily 枚举 (FROZEN-3, 12 个 CF) |
| `crates/nexus-storage/src/commitment_persist.rs` | 承诺树持久化记录结构 |
| `crates/nexus-storage/src/rocks/mod.rs` | RocksStore 打开与 CF 创建 |
| `crates/nexus-config/src/storage.rs` | StorageConfig 包含 `commitment_cache_size` |
