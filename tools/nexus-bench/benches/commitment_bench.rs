// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! State commitment benchmarks (Phase O-4).
//!
//! Measures incremental insert, root computation, proof generation,
//! proof verification, and batch proof at varying tree sizes.
//! Establishes performance baselines for the BLAKE3 Sorted Merkle Tree.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use nexus_storage::commitment::Blake3SmtCommitment;
use nexus_storage::traits::StateCommitment;

/// Build a commitment tree with `n` entries.
fn build_tree(n: usize) -> Blake3SmtCommitment {
    let mut tree = Blake3SmtCommitment::new();
    let pairs: Vec<(Vec<u8>, Vec<u8>)> = (0..n)
        .map(|i| {
            let key = format!("bench_key_{:08}", i).into_bytes();
            let value = format!("bench_val_{:08}", i).into_bytes();
            (key, value)
        })
        .collect();
    let refs: Vec<(&[u8], &[u8])> = pairs
        .iter()
        .map(|(k, v)| (k.as_slice(), v.as_slice()))
        .collect();
    tree.update(&refs);
    tree
}

// ── Incremental insert ──────────────────────────────────────────────────

fn bench_incremental_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("commitment_insert");

    for &base_size in &[0, 100, 1000] {
        group.throughput(Throughput::Elements(10));
        group.bench_with_input(
            BenchmarkId::new("batch_10_into", base_size),
            &base_size,
            |b, &base| {
                b.iter_batched(
                    || {
                        let tree = build_tree(base);
                        let new_pairs: Vec<(Vec<u8>, Vec<u8>)> = (base..base + 10)
                            .map(|i| {
                                let key = format!("bench_key_{:08}", i).into_bytes();
                                let value = format!("bench_val_{:08}", i).into_bytes();
                                (key, value)
                            })
                            .collect();
                        (tree, new_pairs)
                    },
                    |(mut tree, pairs)| {
                        let refs: Vec<(&[u8], &[u8])> = pairs
                            .iter()
                            .map(|(k, v)| (k.as_slice(), v.as_slice()))
                            .collect();
                        tree.update(black_box(&refs));
                        black_box(tree.root_commitment());
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();
}

// ── Root computation ────────────────────────────────────────────────────

fn bench_root_computation(c: &mut Criterion) {
    let mut group = c.benchmark_group("commitment_root");

    for &n in &[10, 100, 500, 1000] {
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::new("entries", n), &n, |b, &n| {
            let tree = build_tree(n);
            b.iter(|| {
                black_box(tree.root_commitment());
            });
        });
    }
    group.finish();
}

// ── Single-key proof generation ─────────────────────────────────────────

fn bench_prove_key(c: &mut Criterion) {
    let mut group = c.benchmark_group("commitment_prove_key");

    for &n in &[10, 100, 500, 1000] {
        group.bench_with_input(BenchmarkId::new("entries", n), &n, |b, &n| {
            let tree = build_tree(n);
            let key = format!("bench_key_{:08}", n / 2).into_bytes();
            b.iter(|| {
                let _ = black_box(tree.prove_key(black_box(&key)));
            });
        });
    }
    group.finish();
}

// ── Proof verification ──────────────────────────────────────────────────

fn bench_verify_proof(c: &mut Criterion) {
    let mut group = c.benchmark_group("commitment_verify_proof");

    for &n in &[10, 100, 500, 1000] {
        group.bench_with_input(BenchmarkId::new("entries", n), &n, |b, &n| {
            let tree = build_tree(n);
            let key = format!("bench_key_{:08}", n / 2).into_bytes();
            let (value, proof) = tree.prove_key(&key).unwrap();
            let root = tree.root_commitment();
            b.iter(|| {
                let result = Blake3SmtCommitment::verify_proof(
                    black_box(&root),
                    black_box(&key),
                    black_box(value.as_deref()),
                    black_box(&proof),
                );
                black_box(result)
            });
        });
    }
    group.finish();
}

// ── Batch proof generation ──────────────────────────────────────────────

fn bench_prove_keys_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("commitment_prove_batch");

    for &n in &[100, 500, 1000] {
        let batch_size = 10.min(n);
        group.throughput(Throughput::Elements(batch_size as u64));
        group.bench_with_input(BenchmarkId::new("entries", n), &n, |b, &n| {
            let tree = build_tree(n);
            let keys: Vec<Vec<u8>> = (0..batch_size)
                .map(|i| format!("bench_key_{:08}", i * (n / batch_size)).into_bytes())
                .collect();
            let key_refs: Vec<&[u8]> = keys.iter().map(|k| k.as_slice()).collect();
            b.iter(|| {
                let _ = black_box(tree.prove_keys(black_box(&key_refs)));
            });
        });
    }
    group.finish();
}

// ── Exclusion proof generation ──────────────────────────────────────────

fn bench_prove_exclusion(c: &mut Criterion) {
    let mut group = c.benchmark_group("commitment_prove_exclusion");

    for &n in &[10, 100, 500, 1000] {
        group.bench_with_input(BenchmarkId::new("entries", n), &n, |b, &n| {
            let tree = build_tree(n);
            // Key that doesn't exist in the tree
            let key = b"__nonexistent_key__".to_vec();
            b.iter(|| {
                let _ = black_box(tree.prove_key(black_box(&key)));
            });
        });
    }
    group.finish();
}

// ── Criterion entry point ───────────────────────────────────────────────

criterion_group!(
    benches,
    bench_incremental_insert,
    bench_root_computation,
    bench_prove_key,
    bench_verify_proof,
    bench_prove_keys_batch,
    bench_prove_exclusion,
);
criterion_main!(benches);
