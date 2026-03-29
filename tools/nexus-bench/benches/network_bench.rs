// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Phase 8 network-layer benchmarks.
//!
//! Measures:
//! - Wire codec encode/decode throughput
//! - NetworkService build latency (incl. keypair generation)
//! - Config validation hot-path

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use nexus_network::config::NetworkConfig;
use nexus_network::types::MessageType;

// ── Wire codec encode/decode ─────────────────────────────────────────────

fn bench_wire_encode_decode(c: &mut Criterion) {
    let mut group = c.benchmark_group("wire_codec");

    for &size in &[256, 1024, 4096, 65_500] {
        // Use raw Vec<u8> as payload (BCS serializes it with length prefix)
        let payload: Vec<u8> = vec![0xABu8; size];
        let msg_type = MessageType::Transaction;

        group.throughput(Throughput::Bytes(size as u64));

        group.bench_with_input(BenchmarkId::new("encode", size), &size, |b, _| {
            b.iter(|| {
                black_box(nexus_network::codec::encode(
                    black_box(msg_type),
                    black_box(&payload),
                ))
            });
        });

        // Pre-encode for decode benchmark
        let encoded =
            nexus_network::codec::encode(msg_type, &payload).expect("encode should succeed");
        group.bench_with_input(BenchmarkId::new("decode", size), &size, |b, _| {
            b.iter(|| {
                black_box(nexus_network::codec::decode::<Vec<u8>>(black_box(&encoded))).unwrap();
            });
        });
    }
    group.finish();
}

// ── Config validation ────────────────────────────────────────────────────

fn bench_config_validate(c: &mut Criterion) {
    let config = NetworkConfig::for_testing();
    c.bench_function("config_validate", |b| {
        b.iter(|| {
            black_box(black_box(&config).validate()).unwrap();
        });
    });
}

// ── Service build (includes keypair generation) ──────────────────────────

fn bench_service_build(c: &mut Criterion) {
    c.bench_function("network_service_build", |b| {
        let config = NetworkConfig::for_testing();
        b.iter(|| {
            let _ = black_box(nexus_network::service::NetworkService::build(black_box(
                &config,
            )));
        });
    });
}

criterion_group!(
    network_benches,
    bench_wire_encode_decode,
    bench_config_validate,
    bench_service_build,
);
criterion_main!(network_benches);
