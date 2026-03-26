// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! BLAKE3-256 hashing benchmarks.
//!
//! Measures one-shot digest, incremental hashing, and Merkle root
//! computation at varying payload sizes.
//! DEV-09 target: > 5 GB/s single-thread with AVX2.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use nexus_crypto::domains;
use nexus_crypto::{Blake3Hasher, CryptoHasher};
use nexus_primitives::Blake3Digest;

// ── One-shot digest ─────────────────────────────────────────────────────

fn bench_blake3_digest(c: &mut Criterion) {
    let mut group = c.benchmark_group("blake3_digest");

    for &size in &[32, 512, 1024, 10_240, 102_400] {
        let data = vec![0xFFu8; size];
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::new("bytes", size), &size, |b, _| {
            b.iter(|| {
                black_box(Blake3Hasher::digest(
                    black_box(domains::USER_TX),
                    black_box(&data),
                ));
            });
        });
    }
    group.finish();
}

// ── Incremental hashing ─────────────────────────────────────────────────

fn bench_blake3_incremental(c: &mut Criterion) {
    let mut group = c.benchmark_group("blake3_incremental");

    let total = 10_240;
    for &chunk_size in &[32, 256, 1024] {
        let chunks: Vec<Vec<u8>> = (0..(total / chunk_size))
            .map(|i| vec![(i & 0xFF) as u8; chunk_size])
            .collect();

        group.throughput(Throughput::Bytes(total as u64));
        group.bench_with_input(
            BenchmarkId::new("chunk_bytes", chunk_size),
            &chunk_size,
            |b, _| {
                b.iter(|| {
                    let mut hasher = Blake3Hasher::new_with_domain(domains::STATE_ROOT);
                    for chunk in &chunks {
                        hasher.update(black_box(chunk));
                    }
                    black_box(hasher.finalize());
                });
            },
        );
    }
    group.finish();
}

// ── Merkle root ─────────────────────────────────────────────────────────

fn bench_blake3_merkle_root(c: &mut Criterion) {
    let mut group = c.benchmark_group("blake3_merkle_root");

    for &n_leaves in &[4, 16, 64, 256, 1024] {
        let leaves: Vec<Blake3Digest> = (0..n_leaves)
            .map(|i| Blake3Hasher::digest(domains::VERKLE_LEAF, &(i as u64).to_le_bytes()))
            .collect();

        group.throughput(Throughput::Elements(n_leaves as u64));
        group.bench_with_input(BenchmarkId::new("leaves", n_leaves), &n_leaves, |b, _| {
            b.iter(|| {
                black_box(Blake3Hasher::merkle_root(black_box(&leaves)));
            });
        });
    }
    group.finish();
}

// ── Criterion entry point ───────────────────────────────────────────────

criterion_group!(
    benches,
    bench_blake3_digest,
    bench_blake3_incremental,
    bench_blake3_merkle_root,
);
criterion_main!(benches);
