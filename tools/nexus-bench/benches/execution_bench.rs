//! Block-STM execution engine benchmarks.
//!
//! Measures throughput and latency baselines for the parallel execution
//! pipeline under varying transaction counts, conflict rates, and
//! parallelism levels.  Results establish the Phase 2 performance
//! baseline per DEV-09.
//!
//! Phase 12 additions: Move contract publish/call latency, throughput
//! scaling, and gas-cost baselines (T-11005).

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use nexus_crypto::{DilithiumSigner, Signer};
use nexus_execution::block_stm::BlockStmExecutor;
use nexus_execution::types::{
    compute_tx_digest, SignedTransaction, TransactionBody, TransactionPayload, TX_DOMAIN,
};
use nexus_primitives::{
    AccountAddress, CommitSequence, ContractAddress, EpochNumber, ShardId, TimestampMs,
};
use nexus_test_utils::fixtures::execution::{MemStateView, TxBuilder};

// ── Helpers ─────────────────────────────────────────────────────────────

/// Build N non-conflicting transfer transactions (each sender is unique).
fn build_non_conflicting_txs(n: usize) -> (Vec<SignedTransaction>, MemStateView) {
    let mut state = MemStateView::new();
    let recipient = TxBuilder::new(1).sender;
    let txs: Vec<_> = (0..n)
        .map(|i| {
            let builder = TxBuilder::new(1);
            state.set_balance(builder.sender, 1_000_000);
            builder.transfer(recipient, 100, i as u64)
        })
        .collect();
    (txs, state)
}

/// Build N conflicting transfer transactions (all from the SAME sender).
fn build_conflicting_txs(n: usize) -> (Vec<SignedTransaction>, MemStateView) {
    let mut state = MemStateView::new();
    let sender = TxBuilder::new(1);
    state.set_balance(sender.sender, n as u64 * 1_000_000);
    let recipient = TxBuilder::new(1).sender;
    let txs: Vec<_> = (0..n)
        .map(|i| sender.transfer(recipient, 100, i as u64))
        .collect();
    (txs, state)
}

fn make_executor(workers: usize) -> BlockStmExecutor {
    BlockStmExecutor::with_config(
        ShardId(0),
        CommitSequence(1),
        TimestampMs::now(),
        5,
        workers,
    )
}

// ── Benchmarks ──────────────────────────────────────────────────────────

/// Baseline: single transfer transaction.
fn bench_single_transfer(c: &mut Criterion) {
    let mut group = c.benchmark_group("block_stm_single");
    group.throughput(Throughput::Elements(1));

    let (txs, state) = build_non_conflicting_txs(1);
    let executor = make_executor(1);

    group.bench_function("single_transfer", |b| {
        b.iter(|| {
            let result = executor.execute(black_box(&txs), black_box(&state));
            black_box(result).unwrap();
        });
    });
    group.finish();
}

/// Throughput scaling with non-conflicting transactions.
fn bench_non_conflicting_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("block_stm_no_conflict");

    for &n in &[10, 100, 500, 1000] {
        group.throughput(Throughput::Elements(n as u64));
        let (txs, state) = build_non_conflicting_txs(n);
        let executor = make_executor(4);

        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                let result = executor.execute(black_box(&txs), black_box(&state));
                black_box(result).unwrap();
            });
        });
    }
    group.finish();
}

/// Overhead of conflicting transactions (single sender → Phase 2 re-execution).
fn bench_conflicting_overhead(c: &mut Criterion) {
    let mut group = c.benchmark_group("block_stm_conflict");

    for &n in &[10, 50, 100] {
        group.throughput(Throughput::Elements(n as u64));
        let (txs, state) = build_conflicting_txs(n);
        let executor = make_executor(4);

        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                let result = executor.execute(black_box(&txs), black_box(&state));
                // Conflicting batches may partially fail — that's expected.
                let _ = black_box(result);
            });
        });
    }
    group.finish();
}

/// Parallelism scaling: same workload, different worker counts.
fn bench_parallelism_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("block_stm_parallelism");

    let n = 200;
    group.throughput(Throughput::Elements(n as u64));
    let (txs, state) = build_non_conflicting_txs(n);

    for &workers in &[1, 2, 4, 8] {
        let executor = make_executor(workers);

        group.bench_with_input(BenchmarkId::new("workers", workers), &workers, |b, _| {
            b.iter(|| {
                let result = executor.execute(black_box(&txs), black_box(&state));
                black_box(result).unwrap();
            });
        });
    }
    group.finish();
}

/// Empty block overhead baseline.
fn bench_empty_block(c: &mut Criterion) {
    let mut group = c.benchmark_group("block_stm_empty");
    group.throughput(Throughput::Elements(0));

    let state = MemStateView::new();
    let txs: Vec<SignedTransaction> = vec![];
    let executor = make_executor(1);

    group.bench_function("empty_block", |b| {
        b.iter(|| {
            let result = executor.execute(black_box(&txs), black_box(&state));
            black_box(result).unwrap();
        });
    });
    group.finish();
}

// ── Move Contract Helpers (Phase 12 / T-11005) ─────────────────────────

/// Helper: sign a transaction body using a fresh keypair.
fn sign_bench_tx(body: TransactionBody) -> SignedTransaction {
    let digest = compute_tx_digest(&body).expect("test tx serialization");
    let (sk, pk) = DilithiumSigner::generate_keypair();
    let sig = DilithiumSigner::sign(&sk, TX_DOMAIN, digest.as_bytes());
    SignedTransaction {
        body,
        signature: sig,
        sender_pk: pk,
        digest,
    }
}

/// Build valid Move bytecode (magic + version + padding).
fn make_move_bytecode(padding: usize) -> Vec<u8> {
    let mut module = vec![0xa1, 0x1c, 0xeb, 0x0b]; // Move magic
    module.extend_from_slice(&1u32.to_le_bytes()); // Version 1
    module.extend(vec![0u8; padding]);
    module
}

/// Build a MovePublish transaction.
fn build_publish_tx(sender_byte: u8, gas_limit: u64) -> SignedTransaction {
    let sender = AccountAddress([sender_byte; 32]);
    sign_bench_tx(TransactionBody {
        sender,
        sequence_number: 0,
        expiry_epoch: EpochNumber(1000),
        gas_limit,
        gas_price: 1,
        target_shard: None,
        payload: TransactionPayload::MovePublish {
            bytecode_modules: vec![make_move_bytecode(64)],
        },
        chain_id: 1,
    })
}

/// Build a MoveCall transaction.
fn build_call_tx(sender_byte: u8, contract_byte: u8, gas_limit: u64) -> SignedTransaction {
    let sender = AccountAddress([sender_byte; 32]);
    let contract = ContractAddress([contract_byte; 32]);
    sign_bench_tx(TransactionBody {
        sender,
        sequence_number: 0,
        expiry_epoch: EpochNumber(1000),
        gas_limit,
        gas_price: 1,
        target_shard: None,
        payload: TransactionPayload::MoveCall {
            contract,
            function: "increment".into(),
            type_args: vec![],
            args: vec![],
        },
        chain_id: 1,
    })
}

// ── Move Publish Benchmarks ─────────────────────────────────────────────

/// Baseline: single Move module publish.
fn bench_move_publish_single(c: &mut Criterion) {
    let mut group = c.benchmark_group("move_publish");
    group.throughput(Throughput::Elements(1));

    let tx = build_publish_tx(0xA0, 100_000);
    let state = MemStateView::new();
    let executor = make_executor(1);

    group.bench_function("single_module", |b| {
        b.iter(|| {
            let result = executor.execute(black_box(&[tx.clone()]), black_box(&state));
            black_box(result).unwrap();
        });
    });
    group.finish();
}

/// Publish throughput: varying number of unique publish transactions.
fn bench_move_publish_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("move_publish_throughput");

    for &n in &[10, 50, 100] {
        group.throughput(Throughput::Elements(n as u64));
        let txs: Vec<_> = (0..n).map(|i| build_publish_tx(i as u8, 100_000)).collect();
        let state = MemStateView::new();
        let executor = make_executor(4);

        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                let result = executor.execute(black_box(&txs), black_box(&state));
                black_box(result).unwrap();
            });
        });
    }
    group.finish();
}

// ── Move Call Benchmarks ────────────────────────────────────────────────

/// Baseline: single Move contract call.
fn bench_move_call_single(c: &mut Criterion) {
    let mut group = c.benchmark_group("move_call");
    group.throughput(Throughput::Elements(1));

    let tx = build_call_tx(0xB0, 0xCC, 100_000);
    let state = MemStateView::new();
    let executor = make_executor(1);

    group.bench_function("single_call", |b| {
        b.iter(|| {
            let result = executor.execute(black_box(&[tx.clone()]), black_box(&state));
            black_box(result).unwrap();
        });
    });
    group.finish();
}

/// Call throughput: varying number of calls from different senders.
fn bench_move_call_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("move_call_throughput");

    for &n in &[10, 50, 100] {
        group.throughput(Throughput::Elements(n as u64));
        let txs: Vec<_> = (0..n)
            .map(|i| build_call_tx(i as u8, 0xCC, 100_000))
            .collect();
        let state = MemStateView::new();
        let executor = make_executor(4);

        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                let result = executor.execute(black_box(&txs), black_box(&state));
                black_box(result).unwrap();
            });
        });
    }
    group.finish();
}

// ── Gas Cost Baseline ───────────────────────────────────────────────────

/// Measure gas consumed by publish vs call vs transfer for gas baseline data.
fn bench_gas_cost_comparison(c: &mut Criterion) {
    let mut group = c.benchmark_group("gas_cost_baseline");
    group.throughput(Throughput::Elements(1));

    let state = {
        let mut s = MemStateView::new();
        let sender = AccountAddress([0xD0; 32]);
        s.set_balance(sender, 10_000_000);
        s
    };
    let executor = make_executor(1);

    // Publish gas
    let publish_tx = build_publish_tx(0xD0, 200_000);
    group.bench_function("publish_gas", |b| {
        b.iter(|| {
            let result = executor.execute(black_box(&[publish_tx.clone()]), black_box(&state));
            let r = result.unwrap();
            black_box(r.receipts[0].gas_used);
        });
    });

    // Call gas
    let call_tx = build_call_tx(0xD0, 0xCC, 200_000);
    group.bench_function("call_gas", |b| {
        b.iter(|| {
            let result = executor.execute(black_box(&[call_tx.clone()]), black_box(&state));
            let r = result.unwrap();
            black_box(r.receipts[0].gas_used);
        });
    });

    // Transfer gas (for comparison)
    let xfer_builder = TxBuilder::new(1);
    let mut xfer_state = MemStateView::new();
    xfer_state.set_balance(xfer_builder.sender, 10_000_000);
    let xfer_tx = xfer_builder.transfer(AccountAddress([0xEE; 32]), 100, 0);
    group.bench_function("transfer_gas", |b| {
        b.iter(|| {
            let result = executor.execute(black_box(&[xfer_tx.clone()]), black_box(&xfer_state));
            let r = result.unwrap();
            black_box(r.receipts[0].gas_used);
        });
    });

    group.finish();
}

// ── Bytecode Size Scaling (T-11005) ─────────────────────────────────────

/// Publish latency as a function of bytecode size.
fn bench_publish_bytecode_size(c: &mut Criterion) {
    let mut group = c.benchmark_group("move_publish_bytecode_size");

    for &kb in &[1, 8, 64, 256] {
        let size = kb * 1024;
        group.throughput(Throughput::Bytes(size as u64));

        let sender = AccountAddress([0xA0; 32]);
        let bytecode = make_move_bytecode(size);
        let tx = sign_bench_tx(TransactionBody {
            sender,
            sequence_number: 0,
            expiry_epoch: EpochNumber(1000),
            gas_limit: 1_000_000,
            gas_price: 1,
            target_shard: None,
            payload: TransactionPayload::MovePublish {
                bytecode_modules: vec![bytecode],
            },
            chain_id: 1,
        });
        let state = MemStateView::new();
        let executor = make_executor(1);

        group.bench_with_input(BenchmarkId::new("kib", kb), &kb, |b, _| {
            b.iter(|| {
                let result = executor.execute(black_box(&[tx.clone()]), black_box(&state));
                black_box(result).unwrap();
            });
        });
    }
    group.finish();
}

// ── Mixed Workload (T-11005) ────────────────────────────────────────────

/// Mixed block: transfers + publishes + calls in the same batch.
fn bench_mixed_workload(c: &mut Criterion) {
    let mut group = c.benchmark_group("mixed_workload");

    for &n in &[10, 50, 100] {
        // n transactions: 1/3 transfers, 1/3 publishes, 1/3 calls.
        let third = n / 3;
        group.throughput(Throughput::Elements(n as u64));

        let mut state = MemStateView::new();
        let mut txs = Vec::with_capacity(n);

        // Transfers.
        for i in 0..third {
            let builder = TxBuilder::new(1);
            state.set_balance(builder.sender, 1_000_000);
            txs.push(builder.transfer(AccountAddress([0xEE; 32]), 100, i as u64));
        }
        // Publishes.
        for i in 0..third {
            txs.push(build_publish_tx((0xA0u8).wrapping_add(i as u8), 100_000));
        }
        // Calls.
        for i in 0..third {
            txs.push(build_call_tx((0xB0u8).wrapping_add(i as u8), 0xCC, 100_000));
        }

        let executor = make_executor(4);
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                let result = executor.execute(black_box(&txs), black_box(&state));
                black_box(result).unwrap();
            });
        });
    }
    group.finish();
}

// ── Move Call Contention (T-11005) ──────────────────────────────────────

/// Multiple callers invoking the same contract — measures Block-STM contention.
fn bench_move_call_contention(c: &mut Criterion) {
    let mut group = c.benchmark_group("move_call_contention");

    for &n in &[10, 50, 100] {
        group.throughput(Throughput::Elements(n as u64));

        // All callers target the same contract address.
        let txs: Vec<_> = (0..n)
            .map(|i| build_call_tx(i as u8, 0xCC, 100_000)) // same contract 0xCC
            .collect();
        let state = MemStateView::new();
        let executor = make_executor(4);

        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                let result = executor.execute(black_box(&txs), black_box(&state));
                let _ = black_box(result);
            });
        });
    }
    group.finish();
}

// ── Move Parallelism Scaling (T-11005) ──────────────────────────────────

/// Move publish/call throughput at different worker counts.
fn bench_move_parallelism(c: &mut Criterion) {
    let mut group = c.benchmark_group("move_parallelism");

    let n = 100;
    group.throughput(Throughput::Elements(n as u64));

    // Build a mixed batch of publish + call transactions.
    let txs: Vec<_> = (0..n)
        .map(|i| {
            if i % 2 == 0 {
                build_publish_tx(i as u8, 100_000)
            } else {
                build_call_tx(i as u8, 0xCC, 100_000)
            }
        })
        .collect();
    let state = MemStateView::new();

    for &workers in &[1, 2, 4, 8] {
        let executor = make_executor(workers);

        group.bench_with_input(BenchmarkId::new("workers", workers), &workers, |b, _| {
            b.iter(|| {
                let result = executor.execute(black_box(&txs), black_box(&state));
                black_box(result).unwrap();
            });
        });
    }
    group.finish();
}

// ── Query View Benchmark (T-11005) ──────────────────────────────────────

/// Read-only query_view latency with pre-populated ABI + resource state.
fn bench_query_view_latency(c: &mut Criterion) {
    use nexus_test_utils::fixtures::execution::setup_query_view_state;

    let mut group = c.benchmark_group("query_view");
    group.throughput(Throughput::Elements(1));

    let (state, contract_addr) = setup_query_view_state(0xCC);
    let executor = make_executor(1);

    group.bench_function("single_query", |b| {
        b.iter(|| {
            let result = executor.query_view(
                black_box(&state),
                black_box(contract_addr),
                black_box("get_count"),
                black_box(&[]),
                black_box(&[]),
            );
            black_box(result).unwrap();
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_single_transfer,
    bench_non_conflicting_throughput,
    bench_conflicting_overhead,
    bench_parallelism_scaling,
    bench_empty_block,
    // Phase 12: Move execution benchmarks (T-11005)
    bench_move_publish_single,
    bench_move_publish_throughput,
    bench_move_call_single,
    bench_move_call_throughput,
    bench_gas_cost_comparison,
    // T-11005 expansion: bytecode scaling, mixed workloads, contention
    bench_publish_bytecode_size,
    bench_mixed_workload,
    bench_move_call_contention,
    bench_move_parallelism,
    // T-11005: query_view read-only path
    bench_query_view_latency,
);
criterion_main!(benches);
