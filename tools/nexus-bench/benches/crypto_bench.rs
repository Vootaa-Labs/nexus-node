//! Post-quantum cryptography benchmarks.
//!
//! Measures keygen, sign, and verify throughput for FALCON-512 (consensus)
//! and ML-DSA / Dilithium3 (user transactions), plus Kyber-768 KEM.
//! DEV-09 target: FALCON verify > 600 000 ops/s (32-thread EPYC).

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use nexus_crypto::domains;
use nexus_crypto::{DilithiumSigner, FalconSigner, KeyEncapsulationMechanism, KyberKem, Signer};

// ── FALCON-512 (consensus signatures) ───────────────────────────────────

fn bench_falcon_keygen(c: &mut Criterion) {
    let mut group = c.benchmark_group("falcon512_keygen");
    group.throughput(Throughput::Elements(1));
    group.bench_function("generate_keypair", |b| {
        b.iter(|| {
            black_box(FalconSigner::generate_keypair());
        });
    });
    group.finish();
}

fn bench_falcon_sign(c: &mut Criterion) {
    let (sk, _vk) = FalconSigner::generate_keypair();

    let mut group = c.benchmark_group("falcon512_sign");

    for &size in &[32, 256, 1024] {
        let msg = vec![0xABu8; size];
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::new("msg_bytes", size), &size, |b, _| {
            b.iter(|| {
                black_box(FalconSigner::sign(
                    &sk,
                    black_box(domains::NARWHAL_CERT),
                    black_box(&msg),
                ));
            });
        });
    }
    group.finish();
}

fn bench_falcon_verify(c: &mut Criterion) {
    let (sk, vk) = FalconSigner::generate_keypair();

    let mut group = c.benchmark_group("falcon512_verify");

    for &size in &[32, 256, 1024] {
        let msg = vec![0xABu8; size];
        let sig = FalconSigner::sign(&sk, domains::NARWHAL_CERT, &msg);
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::new("msg_bytes", size), &size, |b, _| {
            b.iter(|| {
                FalconSigner::verify(
                    black_box(&vk),
                    black_box(domains::NARWHAL_CERT),
                    black_box(&msg),
                    black_box(&sig),
                )
                .unwrap();
            });
        });
    }
    group.finish();
}

// ── Dilithium3 / ML-DSA (user transaction signatures) ───────────────────

fn bench_dilithium_keygen(c: &mut Criterion) {
    let mut group = c.benchmark_group("dilithium3_keygen");
    group.throughput(Throughput::Elements(1));
    group.bench_function("generate_keypair", |b| {
        b.iter(|| {
            black_box(DilithiumSigner::generate_keypair());
        });
    });
    group.finish();
}

fn bench_dilithium_sign(c: &mut Criterion) {
    let (sk, _vk) = DilithiumSigner::generate_keypair();

    let mut group = c.benchmark_group("dilithium3_sign");

    for &size in &[32, 256, 1024] {
        let msg = vec![0xCDu8; size];
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::new("msg_bytes", size), &size, |b, _| {
            b.iter(|| {
                black_box(DilithiumSigner::sign(
                    &sk,
                    black_box(domains::USER_TX),
                    black_box(&msg),
                ));
            });
        });
    }
    group.finish();
}

fn bench_dilithium_verify(c: &mut Criterion) {
    let (sk, vk) = DilithiumSigner::generate_keypair();

    let mut group = c.benchmark_group("dilithium3_verify");

    for &size in &[32, 256, 1024] {
        let msg = vec![0xCDu8; size];
        let sig = DilithiumSigner::sign(&sk, domains::USER_TX, &msg);
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::new("msg_bytes", size), &size, |b, _| {
            b.iter(|| {
                DilithiumSigner::verify(
                    black_box(&vk),
                    black_box(domains::USER_TX),
                    black_box(&msg),
                    black_box(&sig),
                )
                .unwrap();
            });
        });
    }
    group.finish();
}

// ── Kyber-768 KEM ───────────────────────────────────────────────────────

fn bench_kyber_keygen(c: &mut Criterion) {
    let mut group = c.benchmark_group("kyber768_keygen");
    group.throughput(Throughput::Elements(1));
    group.bench_function("generate_keypair", |b| {
        b.iter(|| {
            black_box(KyberKem::generate_keypair());
        });
    });
    group.finish();
}

fn bench_kyber_encaps(c: &mut Criterion) {
    let (ek, _dk) = KyberKem::generate_keypair();

    let mut group = c.benchmark_group("kyber768_encaps");
    group.throughput(Throughput::Elements(1));
    group.bench_function("encapsulate", |b| {
        b.iter(|| {
            black_box(KyberKem::encapsulate(black_box(&ek)));
        });
    });
    group.finish();
}

fn bench_kyber_decaps(c: &mut Criterion) {
    let (ek, dk) = KyberKem::generate_keypair();
    let (_ss, ct) = KyberKem::encapsulate(&ek);

    let mut group = c.benchmark_group("kyber768_decaps");
    group.throughput(Throughput::Elements(1));
    group.bench_function("decapsulate", |b| {
        b.iter(|| {
            black_box(KyberKem::decapsulate(black_box(&dk), black_box(&ct))).unwrap();
        });
    });
    group.finish();
}

// ── Criterion entry point ───────────────────────────────────────────────

criterion_group!(
    benches,
    bench_falcon_keygen,
    bench_falcon_sign,
    bench_falcon_verify,
    bench_dilithium_keygen,
    bench_dilithium_sign,
    bench_dilithium_verify,
    bench_kyber_keygen,
    bench_kyber_encaps,
    bench_kyber_decaps,
);
criterion_main!(benches);
