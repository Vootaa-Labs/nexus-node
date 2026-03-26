//! Phase 5 + Phase 9 pipeline benchmarks — genesis boot, query backend,
//! Block-STM execution throughput, and Phase 9 pipeline component benchmarks.
//!
//! These establish the Phase 5 + Phase 9 performance baselines.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::sync::Arc;

use nexus_config::genesis::GenesisConfig;
use nexus_crypto::Signer;
use nexus_execution::types::{
    compute_tx_digest, SignedTransaction, TransactionBody, TransactionPayload, TX_DOMAIN,
};
use nexus_node::backends::{StorageQueryBackend, StorageStateView};
use nexus_node::batch_store::BatchStore;
use nexus_node::genesis_boot;
use nexus_node::mempool::{Mempool, MempoolConfig};
use nexus_primitives::{
    AccountAddress, Amount, Blake3Digest, CommitSequence, EpochNumber, ShardId, TimestampMs,
    TokenId,
};
use nexus_rpc::QueryBackend;
use nexus_storage::{ColumnFamily, MemoryStore, StateStorage, WriteBatchOps};

// ── Helpers ─────────────────────────────────────────────────────────────

/// Write a genesis file to a temp path and return it.
fn write_test_genesis() -> (std::path::PathBuf, GenesisConfig) {
    let genesis = GenesisConfig::for_testing();
    let dir = std::env::temp_dir().join("nexus-pipeline-bench");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("genesis.json");
    std::fs::write(&path, serde_json::to_string(&genesis).unwrap()).unwrap();
    (path, genesis)
}

/// Build a signed transfer transaction.
fn build_transfer(
    sender: AccountAddress,
    recipient: AccountAddress,
    seq: u64,
) -> SignedTransaction {
    let body = TransactionBody {
        sender,
        sequence_number: seq,
        expiry_epoch: EpochNumber(1000),
        gas_limit: 50_000,
        gas_price: 1,
        target_shard: None,
        payload: TransactionPayload::Transfer {
            recipient,
            amount: Amount(100),
            token: TokenId::Native,
        },
        chain_id: 1,
    };
    let digest = compute_tx_digest(&body).unwrap();
    let (sk, pk) = nexus_crypto::DilithiumSigner::generate_keypair();
    let sig = nexus_crypto::DilithiumSigner::sign(&sk, TX_DOMAIN, digest.as_bytes());
    SignedTransaction {
        body,
        signature: sig,
        sender_pk: pk,
        digest,
    }
}

// ── Benchmarks ──────────────────────────────────────────────────────────

/// Benchmark genesis boot (load + validate + seed storage).
fn bench_genesis_boot(c: &mut Criterion) {
    let (path, _genesis) = write_test_genesis();

    c.bench_function("pipeline/genesis_boot", |b| {
        b.iter(|| {
            let store = MemoryStore::new();
            let result =
                genesis_boot::boot_from_genesis(black_box(&path), &store, ShardId(0)).unwrap();
            black_box(result);
        })
    });
}

/// Benchmark balance queries through StorageQueryBackend.
fn bench_balance_query(c: &mut Criterion) {
    let store = MemoryStore::new();
    let shard_id = ShardId(0);

    // Pre-populate N accounts.
    let n = 1000u64;
    let rt = tokio::runtime::Runtime::new().unwrap();
    for i in 0..n {
        let mut addr = [0u8; 32];
        addr[..8].copy_from_slice(&i.to_le_bytes());
        let key = nexus_storage::AccountKey {
            shard_id,
            address: AccountAddress(addr),
        };
        let mut batch = store.new_batch();
        batch.put_cf(
            ColumnFamily::State.as_str(),
            key.to_bytes(),
            Amount(1_000_000).0.to_le_bytes().to_vec(),
        );
        rt.block_on(store.write_batch(batch)).unwrap();
    }

    let epoch = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let commit = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let query = StorageQueryBackend::new(store, shard_id, epoch, commit);

    let mut group = c.benchmark_group("pipeline/balance_query");
    group.throughput(Throughput::Elements(1));

    group.bench_function("hit", |b| {
        let mut addr = [0u8; 32];
        addr[..8].copy_from_slice(&42u64.to_le_bytes());
        let address = AccountAddress(addr);
        b.iter(|| {
            let r = query.account_balance(black_box(&address), &TokenId::Native);
            black_box(r).unwrap();
        })
    });

    group.bench_function("miss", |b| {
        let address = AccountAddress([0xFF; 32]);
        b.iter(|| {
            let r = query.account_balance(black_box(&address), &TokenId::Native);
            black_box(r).unwrap_err();
        })
    });

    group.finish();
}

/// Benchmark Block-STM executor throughput (synchronous, no actor overhead).
fn bench_execution(c: &mut Criterion) {
    let shard_id = ShardId(0);
    let rt = tokio::runtime::Runtime::new().unwrap();

    let mut group = c.benchmark_group("pipeline/execution");

    for batch_size in [1, 10, 50] {
        group.throughput(Throughput::Elements(batch_size as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(batch_size),
            &batch_size,
            |b, &n| {
                let store = MemoryStore::new();
                let sender = AccountAddress([0xAA; 32]);
                let recipient = AccountAddress([0xBB; 32]);

                // Seed balance.
                let key = nexus_storage::AccountKey {
                    shard_id,
                    address: sender,
                };
                let mut batch = store.new_batch();
                batch.put_cf(
                    ColumnFamily::State.as_str(),
                    key.to_bytes(),
                    Amount(u64::MAX).0.to_le_bytes().to_vec(),
                );
                rt.block_on(store.write_batch(batch)).unwrap();

                let state_view = Arc::new(StorageStateView::new(store, shard_id));

                let mut seq = 0u64;
                b.iter(|| {
                    seq += 1;
                    let txs: Vec<_> = (0..n)
                        .map(|i| build_transfer(sender, recipient, seq * 1000 + i as u64))
                        .collect();

                    let executor = nexus_execution::block_stm::BlockStmExecutor::with_config(
                        shard_id,
                        CommitSequence(seq),
                        TimestampMs(1_000_000),
                        3, // max_retries
                        4, // max_workers
                    );
                    let result = executor.execute(&txs, state_view.as_ref()).unwrap();
                    black_box(result);
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_genesis_boot,
    bench_balance_query,
    bench_execution,
    bench_mempool_throughput,
    bench_batch_store,
);
criterion_main!(benches);

// ── Phase 9: Pipeline component benchmarks ─────────────────────────────

/// Benchmark mempool insert + drain throughput.
fn bench_mempool_throughput(c: &mut Criterion) {
    let sender = AccountAddress([0xAA; 32]);
    let recipient = AccountAddress([0xBB; 32]);

    let mut group = c.benchmark_group("pipeline/mempool");

    for batch_size in [1, 10, 100, 500] {
        group.throughput(Throughput::Elements(batch_size as u64));
        group.bench_with_input(
            BenchmarkId::new("insert_drain", batch_size),
            &batch_size,
            |b, &n| {
                b.iter(|| {
                    let mempool = Mempool::new(&MempoolConfig::default());
                    for i in 0..n {
                        let tx = build_transfer(sender, recipient, i as u64);
                        mempool.insert(tx);
                    }
                    let drained = mempool.drain_batch(n);
                    black_box(drained);
                })
            },
        );
    }

    group.finish();
}

/// Benchmark batch store insert + lookup throughput.
fn bench_batch_store(c: &mut Criterion) {
    let sender = AccountAddress([0xCC; 32]);
    let recipient = AccountAddress([0xDD; 32]);

    let mut group = c.benchmark_group("pipeline/batch_store");

    // Benchmark insert throughput.
    group.throughput(Throughput::Elements(100));
    group.bench_function("insert_100", |b| {
        let tx = build_transfer(sender, recipient, 1);
        b.iter(|| {
            let store = BatchStore::new();
            for i in 0u32..100 {
                let digest = Blake3Digest([i as u8; 32]);
                store.insert(digest, vec![tx.clone()]);
            }
            black_box(&store);
        })
    });

    // Benchmark lookup throughput.
    group.throughput(Throughput::Elements(1));
    group.bench_function("get_hit", |b| {
        let store = BatchStore::new();
        let tx = build_transfer(sender, recipient, 1);
        let digest = Blake3Digest([42u8; 32]);
        store.insert(digest, vec![tx]);

        b.iter(|| {
            let result = store.get(black_box(&digest));
            black_box(result);
        })
    });

    group.finish();
}
