# Proof / Snapshot 信任模型与客户端接入边界

> **版本:** v0.1.13  
> **受众:** 客户端开发者、第三方集成方、安全审计人员  
> **依据:** 当前 `v0.1.13` proof 与状态承诺基线

---

## 1. 概述

Nexus 的 Proof Surface 提供三种能力：

1. **状态承诺（State Commitment）**——当前全局状态的密码学摘要。
2. **Merkle 证明（Inclusion / Exclusion Proof）**——单个键值对存在性或不存在性的密码学证明。
3. **快照签名（Snapshot Signing）**——状态快照的完整性与来源验证。

这三者构成客户端**不依赖节点诚实性**验证链上状态的基础。

---

## 2. 信任假设

### 2.1 客户端必须信任的

| 假设 | 说明 |
|------|------|
| **创世配置** | 客户端需要持有正确的创世状态根（genesis state root），作为信任锚点 |
| **验证人集合** | 客户端需要知道当前 epoch 的验证人公钥集合，以验证提交签名 |
| **密码学原语** | Blake3 哈希函数和 ML-DSA-65 后量子签名方案的安全性 |

### 2.2 客户端不需要信任的

| 不需要信任 | 原因 |
|-----------|------|
| **单个节点的诚实性** | 客户端可通过 Merkle proof 自行验证值是否包含在承诺根中 |
| **传输通道** | Proof 是自验证的——即使在无 TLS 环境中获取，篡改会被检测到 |
| **节点的状态完整性** | 节点无法伪造一个不存在于承诺树中的证明 |

### 2.3 Staking 驱动的委员会轮换与 Proof 信任（v0.1.9+）

自 v0.1.9 起，验证人集合不再静态配置，而是通过 staking snapshot 确定性选举产生。
这对 Proof 信任模型的影响：

| 项目 | 影响 |
|------|------|
| **承诺根签名者集合** | 随 epoch 变化 — 客户端必须跟踪当前 epoch 的 committee 公钥集 |
| **选举确定性** | 同一 staking snapshot + 同一 policy → 所有节点得出同一 committee，可独立验证 |
| **Fallback 安全性** | 选举失败时沿用当前 committee，不会产生空委员会，不影响已有 proof 的有效性 |
| **Slash 排除** | 被 slash 的 validator 不再参与下一届签名，发现不诚实签名者后自动清除 |

### 2.4 多分片环境下的 Proof 边界（v0.1.10+）

| 边界 | 说明 |
|------|------|
| **分片本地 proof** | 每个 shard 维护自己的承诺树，proof 仅对本 shard state 有效 |
| **全局状态根** | 跨分片一致性通过 global state root（各 shard root 的组合哈希）确保 |
| **HTLC 跨分片 proof** | HTLC lock/claim/refund 记录存储在 `cf_htlc_locks`，当前不在承诺树中（未来可扩展） |
| **客户端责任** | 客户端需指定 shard ID 获取分片级别的 proof，或查询全局根确认跨分片一致性 |

### 2.5 当前限制

| 限制 | 说明 | 影响 |
|------|------|------|
| **无轻客户端协议** | 当前没有标准化的轻客户端头部链（header chain）| 客户端需从全节点 RPC 获取最新承诺根 |
| **单节点查询** | 客户端通常查询单个节点 | 应比较多节点返回的承诺根以降低风险 |
| **epoch 边界一致性** | epoch 切换期间可能存在短暂的根变更 | 客户端应重试或等待 epoch 稳定 |
| **HTLC 跨分片 proof** | `cf_htlc_locks` 尚未纳入承诺树 | HTLC 锁的存在性依赖节点查询，不可自验证 |

---

## 3. API 端点

### 3.1 状态承诺查询

```
GET /v2/state/commitment
```

返回：

```json
{
  "commitment_root": "hex-encoded-32-byte-blake3-hash",
  "backup_root": "hex-encoded-32-byte-blake3-hash",
  "entry_count": 42,
  "updates_applied": 128,
  "epoch_checks_passed": 3
}
```

- `commitment_root` 是当前 BLAKE3 Sorted Merkle Tree 的 canonical 树根。
- `backup_root` 是备份树根（后量子双树校验用）。
- `entry_count` 是承诺树中的叶节点数量。
- `updates_applied` 是自节点启动以来应用的状态变更次数。
- `epoch_checks_passed` 是通过的 epoch 边界跨树一致性校验次数。
- 客户端应先获取此根，再用它验证单键证明。

### 3.2 单键证明

```
POST /v2/state/proof
Content-Type: application/json

{ "key": "hex-encoded-key" }
```

返回：

```json
{
  "commitment_root": "hex-encoded-root",
  "value": "hex-encoded-value-or-null",
  "proof": {
    "proof_type": "inclusion",
    "leaf_index": 3,
    "leaf_count": 8,
    "siblings": ["hex1", "hex2", "hex3"],
    "left_neighbor": null,
    "right_neighbor": null
  }
}
```

- `value` 为 `null` 表示排除证明（键不存在）。
- `proof.proof_type` 为 `"inclusion"` 或 `"exclusion"`。
- `proof.siblings` 是验证路径上的兄弟节点哈希（从叶到根的底向上顺序）。
- `proof.leaf_index` 对包含证明为被证明叶的索引，对排除证明为 `null`。

#### 排除证明额外字段

对于 `proof_type: "exclusion"`，返回邻叶 witness：

```json
{
  "commitment_root": "hex-encoded-root",
  "value": null,
  "proof": {
    "proof_type": "exclusion",
    "leaf_index": null,
    "leaf_count": 8,
    "siblings": [],
    "left_neighbor": {
      "key": "hex-key-of-left-neighbor",
      "value": "hex-value",
      "leaf_index": 2,
      "siblings": ["hex1", "hex2", "hex3"]
    },
    "right_neighbor": {
      "key": "hex-key-of-right-neighbor",
      "value": "hex-value",
      "leaf_index": 3,
      "siblings": ["hex1", "hex2", "hex3"]
    }
  }
}
```

- 邻叶证明包含排序上紧邻目标键的左右两个已有叶。
- 验证者确认目标键排序在 `left_neighbor.key` 和 `right_neighbor.key` 之间，且两个邻叶都能独立过 Merkle 验证，即可确认目标键不存在。
- 边界情况：最左排除时 `left_neighbor` 为 `null`；最右排除时 `right_neighbor` 为 `null`。

### 3.3 批量证明

```
POST /v2/state/proofs
Content-Type: application/json

{ "keys": ["hex-key-1", "hex-key-2", ...] }
```

- 最多 100 个键。
- 返回相同的 `commitment_root` 和每个键的独立证明。

---

## 4. 客户端验证流程

### 4.1 验证包含证明

```
输入: key, value, proof, commitment_root

1. 确认 proof.proof_type == "inclusion"。

2. 计算叶子哈希:
   leaf = Blake3("nexus::storage::state::leaf::v1" || key || value)

3. 从 proof.leaf_index 开始，沿兄弟节点逐层计算:
   for sibling in proof.siblings:
     if 当前索引为偶数:
       current = Blake3("nexus::storage::state::node::v1" || current || sibling)
     else:
       current = Blake3("nexus::storage::state::node::v1" || sibling || current)
     索引 >>= 1

4. 比较 current == commitment_root

5. 若相等: 证明有效
   若不等: 证明无效——节点可能篡改或状态不一致
```

### 4.2 验证排除证明

```
输入: target_key, proof (含 left_neighbor/right_neighbor), commitment_root

1. 确认 proof.proof_type == "exclusion"。

2. 对 left_neighbor（若存在）:
   a. 确认 left_neighbor.key < target_key（排序比较）。
   b. 使用 §4.1 步骤 2-4 验证 left_neighbor 的 Merkle 路径。

3. 对 right_neighbor（若存在）:
   a. 确认 right_neighbor.key > target_key（排序比较）。
   b. 使用 §4.1 步骤 2-4 验证 right_neighbor 的 Merkle 路径。

4. 确认相邻性: left_neighbor.leaf_index + 1 == right_neighbor.leaf_index
   （或处于边界情况: 左邻为 null 表示目标在所有键之前，右邻为 null 表示在所有键之后）。

5. 以上全部通过: 目标键不存在于承诺树中。
```

### 4.3 最佳实践

1. **获取根**：先从 `GET /v2/state/commitment` 获取 `primary_root`。
2. **多节点比较**：从至少 2 个不同节点获取 commitment，确认一致。
3. **验证证明**：使用上述算法在客户端本地验证。
4. **检查时效性**：commitment 应来自最新 epoch；对比 `/v2/consensus/epoch` 确认。
5. **处理 epoch 边界**：如果获取证明时跨 epoch，重试一次以获得稳定状态。

---

## 5. 快照完整性验证

### 5.1 快照结构

```
SnapshotManifest:
  schema_version: u32
  store_root_hash: Vec<u8>    # Blake3 哈希
  key_count: u64
  byte_size: u64
  created_at: i64             # Unix 时间戳
  metadata: HashMap<String, String>
```

### 5.2 签名与验签

快照使用节点的 ML-DSA-65 密钥签名：

```
签名过程:
1. 序列化 SnapshotManifest → BCS 字节流
2. signature = ML_DSA_65_Sign(private_key, manifest_bytes)

验签过程:
1. 获取签名者的公钥（从创世或 committee 获取）
2. 序列化 manifest → BCS 字节流
3. ok = ML_DSA_65_Verify(public_key, manifest_bytes, signature)
```

### 5.3 离线验签步骤

1. 从节点导出快照包（含 manifest + 签名 + 数据）。
2. 使用 `nexus-wallet` 工具验证：
   ```bash
   nexus-wallet snapshot verify --manifest snapshot.manifest --signature snapshot.sig --pubkey <hex>
   ```
3. 比较 `store_root_hash` 与链上 commitment root。
4. 检查 `key_count` 和 `byte_size` 与预期一致。

### 5.4 篡改检测

已覆盖的篡改场景（见 `proof_smoke_tests.rs`）：

| 篡改类型 | 检测方式 |
|----------|---------|
| 翻转 manifest 中的一个字节 | 签名验证失败 |
| 替换 `store_root_hash` | 签名验证失败 |
| 修改 `key_count` | 签名验证失败 |
| 使用错误公钥验证 | 签名验证失败 |
| 截断签名 | 签名解析失败 |

---

## 6. 安全边界总结

```
┌───────────────────────────────────────────────┐
│                 信任区域                       │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐     │
│  │  创世     │  │ 验证人    │  │ 密码学    │     │
│  │  状态根   │  │ 公钥集合   │  │ 原语安全  │     │
│  └──────────┘  └──────────┘  └──────────┘     │
└──────────────────────┬────────────────────────┘
                       │
                       ▼
┌───────────────────────────────────────────────┐
│              自验证区域                         │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐     │
│  │  Merkle  │  │ 快照      │  │ 状态     │     │
│  │  证明     │  │ 签名     │   │ 承诺根   │     │
│  └──────────┘  └──────────┘  └──────────┘     │
└──────────────────────┬────────────────────────┘
                       │
                       ▼
┌───────────────────────────────────────────────┐
│              不可信区域                         │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐     │
│  │  节点     │  │ 传输     │   │ 网络     │     │
│  │  诚实性   │  │ 通道      │  │ 中间人    │     │
│  └──────────┘  └──────────┘  └──────────┘     │
└───────────────────────────────────────────────┘
```

---

## 7. 当前版本 vs 主网目标

| 特性 | v0.1.11 (Testnet) | 主网目标 |
|------|-------------------|--------|
| Merkle 包含证明 | ✅ 默认启用 | ✅ |
| Merkle 排除证明 | ✅ 邻叶 witness 验证 | ✅ |
| 承诺树持久化 | ✅ RocksDB 增量持久化 | ✅ |
| Canonical state root | ✅ 统一 commitment root | ✅ |
| 快照签名 | ✅ ML-DSA-65 | ✅ |
| Staking 驱动委员会 | ✅ 确定性选举 + fallback (v0.1.9) | ✅ |
| 多分片状态根 | ✅ 分片 local proof + global root (v0.1.10) | ✅ |
| HTLC 跨分片 proof | ⚠️ cf_htlc_locks 未纳入承诺树 | 🎯 纳入 |
| 轻客户端头部链 | ❌ 不可用 | 🎯 需设计 |
| Proof relay 网络 | ❌ 不可用 | 🎯 需设计 |
| 跨 epoch 证明续接 | ⚠️ 需客户端重试 | 🎯 自动化 |
| Monitoring 指标 | ✅ Prometheus | ✅ |

---

## 附录：相关文件

| 文件 | 作用 |
|------|------|
| `crates/nexus-rpc/src/rest/proof.rs` | Proof REST 端点实现 |
| `crates/nexus-rpc/src/metrics.rs` | Proof 指标收集 |
| `crates/nexus-storage/src/commitment.rs` | 状态承诺追踪器 |
| `crates/nexus-storage/src/snapshot.rs` | 快照签名与校验 |
| `tests/nexus-test-utils/src/proof_smoke_tests.rs` | Proof 冒烟测试 |
| `tests/nexus-test-utils/src/proof_tests.rs` | Proof 单元测试 |
| `Docs/zh/Ops/Testnet_Release_Runbook.md` | 发布门禁中的 Proof 检查项 |
