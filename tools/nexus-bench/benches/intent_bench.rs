//! Intent layer compilation benchmarks.
//!
//! Measures intent compilation throughput and latency for the
//! Phase 3 performance baseline per DEV-09.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use nexus_crypto::{DilithiumSigner, Signer};
use nexus_intent::config::IntentConfig;
use nexus_intent::traits::IntentCompiler;
use nexus_intent::types::*;
use nexus_intent::{AccountResolverImpl, IntentCompilerImpl};
use nexus_primitives::{AccountAddress, Amount, ContractAddress, ShardId, TimestampMs, TokenId};

// ── Helpers ─────────────────────────────────────────────────────────────

fn sender() -> AccountAddress {
    AccountAddress([0xAA; 32])
}

fn recipient() -> AccountAddress {
    AccountAddress([0xBB; 32])
}

fn make_resolver(shard_count: u16) -> AccountResolverImpl {
    let r = AccountResolverImpl::new(shard_count);
    r.balances()
        .set_balance(sender(), TokenId::Native, Amount(u64::MAX / 2));
    r.balances()
        .set_balance(recipient(), TokenId::Native, Amount(1_000));
    r
}

fn sign(intent: &UserIntent) -> SignedUserIntent {
    let (sk, vk) = DilithiumSigner::generate_keypair();
    let nonce = 1u64;
    let digest = compute_intent_digest(intent, &sender(), nonce).unwrap();

    let intent_bytes = bcs::to_bytes(intent).unwrap();
    let sender_bytes = bcs::to_bytes(&sender()).unwrap();
    let nonce_bytes = bcs::to_bytes(&nonce).unwrap();
    let mut msg = Vec::new();
    msg.extend_from_slice(&intent_bytes);
    msg.extend_from_slice(&sender_bytes);
    msg.extend_from_slice(&nonce_bytes);
    let sig = DilithiumSigner::sign(&sk, INTENT_DOMAIN, &msg);

    SignedUserIntent {
        intent: intent.clone(),
        sender: sender(),
        signature: sig,
        sender_pk: vk,
        nonce,
        created_at: TimestampMs(1_000_000),
        digest,
    }
}

fn transfer_intent() -> UserIntent {
    UserIntent::Transfer {
        to: recipient(),
        token: TokenId::Native,
        amount: Amount(100),
    }
}

fn swap_intent() -> UserIntent {
    UserIntent::Swap {
        from_token: TokenId::Native,
        to_token: TokenId::Contract(ContractAddress([0xCC; 32])),
        amount: Amount(50),
        max_slippage_bps: 30,
    }
}

fn contract_call_intent(resolver: &AccountResolverImpl) -> UserIntent {
    let addr = ContractAddress([0xDD; 32]);
    resolver.contracts().register(
        addr,
        ContractLocation {
            shard_id: ShardId(0),
            contract_addr: addr,
            module_name: "bench_module".to_string(),
            verified: true,
        },
    );
    UserIntent::ContractCall {
        contract: addr,
        function: "run".to_string(),
        args: vec![],
        gas_budget: 50_000,
    }
}

// ── Benchmarks ──────────────────────────────────────────────────────────

/// Benchmark: single transfer compilation (same-shard, 1 shard).
fn bench_compile_transfer(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let resolver = make_resolver(1);
    let compiler = IntentCompilerImpl::<AccountResolverImpl>::new(IntentConfig::default());
    let signed = sign(&transfer_intent());

    let mut group = c.benchmark_group("intent_compile_transfer");
    group.throughput(Throughput::Elements(1));
    group.bench_function("single_shard", |b| {
        b.iter(|| rt.block_on(compiler.compile(black_box(&signed), black_box(&resolver))))
    });
    group.finish();
}

/// Benchmark: swap compilation.
fn bench_compile_swap(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let resolver = make_resolver(1);
    let compiler = IntentCompilerImpl::<AccountResolverImpl>::new(IntentConfig::default());
    let signed = sign(&swap_intent());

    let mut group = c.benchmark_group("intent_compile_swap");
    group.throughput(Throughput::Elements(1));
    group.bench_function("single_shard", |b| {
        b.iter(|| rt.block_on(compiler.compile(black_box(&signed), black_box(&resolver))))
    });
    group.finish();
}

/// Benchmark: contract call compilation.
fn bench_compile_contract_call(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let resolver = make_resolver(4);
    let compiler = IntentCompilerImpl::<AccountResolverImpl>::new(IntentConfig::default());
    let intent = contract_call_intent(&resolver);
    let signed = sign(&intent);

    let mut group = c.benchmark_group("intent_compile_contract_call");
    group.throughput(Throughput::Elements(1));
    group.bench_function("same_shard", |b| {
        b.iter(|| rt.block_on(compiler.compile(black_box(&signed), black_box(&resolver))))
    });
    group.finish();
}

/// Benchmark: gas estimation.
fn bench_gas_estimation(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let resolver = make_resolver(4);
    let compiler = IntentCompilerImpl::<AccountResolverImpl>::new(IntentConfig::default());
    let intent = transfer_intent();

    let mut group = c.benchmark_group("intent_gas_estimation");
    group.throughput(Throughput::Elements(1));
    group.bench_function("transfer", |b| {
        b.iter(|| rt.block_on(compiler.estimate_gas(black_box(&intent), black_box(&resolver))))
    });
    group.finish();
}

/// Benchmark: cross-shard transfer (many shards).
fn bench_cross_shard(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let mut group = c.benchmark_group("intent_cross_shard");
    for shard_count in [4u16, 16, 64, 256] {
        let resolver = make_resolver(shard_count);
        let compiler = IntentCompilerImpl::<AccountResolverImpl>::new(IntentConfig::default());
        let signed = sign(&transfer_intent());

        group.throughput(Throughput::Elements(1));
        group.bench_with_input(
            BenchmarkId::from_parameter(shard_count),
            &shard_count,
            |b, _| {
                b.iter(|| rt.block_on(compiler.compile(black_box(&signed), black_box(&resolver))))
            },
        );
    }
    group.finish();
}

/// Benchmark: validation-only (sign + validate, no planning).
fn bench_validation(c: &mut Criterion) {
    let config = IntentConfig::default();
    let signed = sign(&transfer_intent());

    let mut group = c.benchmark_group("intent_validation");
    group.throughput(Throughput::Elements(1));
    group.bench_function("dilithium3_verify", |b| {
        b.iter(|| {
            nexus_intent::compiler::validator::validate_signed_intent(
                black_box(&signed),
                black_box(&config),
            )
        })
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_compile_transfer,
    bench_compile_swap,
    bench_compile_contract_call,
    bench_gas_estimation,
    bench_cross_shard,
    bench_validation,
);
criterion_main!(benches);
