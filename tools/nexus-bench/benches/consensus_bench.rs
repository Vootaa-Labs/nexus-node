// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Consensus layer benchmarks.
//!
//! Measures DAG insertion, certificate construction/verification,
//! and BFT ordering throughput.
//! DEV-09 targets: round finality < 200ms P50, < 500ms P99.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use nexus_consensus::traits::CertificateDag;
use nexus_consensus::types::{NarwhalCertificate, CERT_DOMAIN};
use nexus_consensus::{CertificateBuilder, CertificateVerifier, ConsensusEngine, InMemoryDag};
use nexus_crypto::{FalconSigner, Signer};
use nexus_primitives::{Blake3Digest, EpochNumber, RoundNumber, ValidatorIndex};
use nexus_test_utils::fixtures::consensus::TestCommittee;

// ── Helpers ─────────────────────────────────────────────────────────────

/// Build a set of fully-signed certificates for `n` validators at a given round.
fn build_round_certs(
    tc: &TestCommittee,
    round: RoundNumber,
    parents: Vec<nexus_primitives::CertDigest>,
) -> Vec<NarwhalCertificate> {
    let n = tc.signing_keys.len();
    (0..n)
        .map(|i| {
            let origin = ValidatorIndex(i as u32);
            let batch_digest = Blake3Digest([(round.0 as u8).wrapping_add(i as u8); 32]);
            tc.build_cert(batch_digest, origin, round, parents.clone())
        })
        .collect()
}

// ── Certificate Construction ────────────────────────────────────────────

fn bench_cert_build(c: &mut Criterion) {
    let mut group = c.benchmark_group("consensus_cert_build");

    for &n_validators in &[4, 10, 30] {
        let tc = TestCommittee::new(n_validators, EpochNumber(1));
        let batch_digest = Blake3Digest([0xAA; 32]);
        let origin = ValidatorIndex(0);
        let round = RoundNumber(1);
        let parents = vec![];
        let quorum = &tc.committee;

        // Pre-compute signatures
        let payload = nexus_consensus::certificate::cert_signing_payload(
            tc.epoch,
            &batch_digest,
            origin,
            round,
            &parents,
        )
        .unwrap();
        let sigs: Vec<_> = tc
            .signing_keys
            .iter()
            .enumerate()
            .map(|(i, sk)| {
                (
                    ValidatorIndex(i as u32),
                    FalconSigner::sign(sk, CERT_DOMAIN, &payload),
                )
            })
            .collect();

        group.throughput(Throughput::Elements(1));
        group.bench_with_input(
            BenchmarkId::new("validators", n_validators),
            &n_validators,
            |b, _| {
                b.iter(|| {
                    let mut builder = CertificateBuilder::new(
                        tc.epoch,
                        batch_digest,
                        origin,
                        round,
                        parents.clone(),
                        n_validators as u32,
                    );
                    for (idx, sig) in &sigs {
                        builder.add_signature(*idx, sig.clone());
                    }
                    black_box(builder.build(quorum).unwrap());
                });
            },
        );
    }
    group.finish();
}

// ── Certificate Verification ────────────────────────────────────────────

fn bench_cert_verify(c: &mut Criterion) {
    let mut group = c.benchmark_group("consensus_cert_verify");

    for &n_validators in &[4, 10, 30] {
        let tc = TestCommittee::new(n_validators, EpochNumber(1));
        let cert = tc.build_cert(
            Blake3Digest([0xBB; 32]),
            ValidatorIndex(0),
            RoundNumber(1),
            vec![],
        );

        group.throughput(Throughput::Elements(1));
        group.bench_with_input(
            BenchmarkId::new("validators", n_validators),
            &n_validators,
            |b, _| {
                b.iter(|| {
                    CertificateVerifier::verify(black_box(&cert), &tc.committee, EpochNumber(1))
                        .unwrap();
                });
            },
        );
    }
    group.finish();
}

// ── DAG Insertion ───────────────────────────────────────────────────────

fn bench_dag_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("consensus_dag_insert");

    for &n_validators in &[4, 10] {
        let tc = TestCommittee::new(n_validators, EpochNumber(1));

        // Build genesis round
        let genesis_certs = build_round_certs(&tc, RoundNumber(0), vec![]);
        let parent_digests: Vec<_> = genesis_certs.iter().map(|c| c.cert_digest).collect();

        // Pre-build round-1 certs for insertion
        let round1_certs = build_round_certs(&tc, RoundNumber(1), parent_digests);

        group.throughput(Throughput::Elements(n_validators as u64));
        group.bench_with_input(
            BenchmarkId::new("validators", n_validators),
            &n_validators,
            |b, _| {
                b.iter_with_setup(
                    || {
                        // Fresh DAG with genesis pre-loaded
                        let mut dag = InMemoryDag::new();
                        for cert in &genesis_certs {
                            dag.insert_certificate(cert.clone()).unwrap();
                        }
                        dag
                    },
                    |mut dag| {
                        for cert in &round1_certs {
                            dag.insert_certificate(black_box(cert.clone())).unwrap();
                        }
                        black_box(&dag);
                    },
                );
            },
        );
    }
    group.finish();
}

// ── DAG Causal History ──────────────────────────────────────────────────

fn bench_dag_causal_history(c: &mut Criterion) {
    let mut group = c.benchmark_group("consensus_dag_causal_history");

    let n_validators = 4;
    let tc = TestCommittee::new(n_validators, EpochNumber(1));

    // Build a multi-round DAG
    let genesis_certs = build_round_certs(&tc, RoundNumber(0), vec![]);
    let mut dag = InMemoryDag::new();
    for cert in &genesis_certs {
        dag.insert_certificate(cert.clone()).unwrap();
    }
    let mut parents: Vec<_> = genesis_certs.iter().map(|c| c.cert_digest).collect();

    for round in 1..=5 {
        let certs = build_round_certs(&tc, RoundNumber(round), parents.clone());
        parents = certs.iter().map(|c| c.cert_digest).collect();
        for cert in &certs {
            dag.insert_certificate(cert.clone()).unwrap();
        }
    }

    // Benchmark causal history from a round-5 cert
    let target_digest = parents[0];
    group.throughput(Throughput::Elements(1));
    group.bench_function("5_rounds_4_validators", |b| {
        b.iter(|| {
            black_box(dag.causal_history(black_box(&target_digest)));
        });
    });
    group.finish();
}

// ── Full Engine Pipeline ────────────────────────────────────────────────

fn bench_engine_process_certificate(c: &mut Criterion) {
    let mut group = c.benchmark_group("consensus_engine_process");

    let n_validators = 4;
    let tc = TestCommittee::new(n_validators, EpochNumber(1));

    // Build genesis + round 1 certs
    let genesis_certs = build_round_certs(&tc, RoundNumber(0), vec![]);
    let parent_digests: Vec<_> = genesis_certs.iter().map(|c| c.cert_digest).collect();
    let round1_certs = build_round_certs(&tc, RoundNumber(1), parent_digests);

    group.throughput(Throughput::Elements(n_validators as u64));
    group.bench_function("genesis_plus_round1", |b| {
        b.iter_with_setup(
            || ConsensusEngine::new(tc.epoch, tc.committee.clone()),
            |mut engine| {
                for cert in &genesis_certs {
                    engine.process_certificate(cert.clone()).unwrap();
                }
                for cert in &round1_certs {
                    engine.process_certificate(cert.clone()).unwrap();
                }
                black_box(engine.take_committed());
            },
        );
    });
    group.finish();
}

// ── Criterion entry point ───────────────────────────────────────────────

criterion_group!(
    benches,
    bench_cert_build,
    bench_cert_verify,
    bench_dag_insert,
    bench_dag_causal_history,
    bench_engine_process_certificate,
);
criterion_main!(benches);
