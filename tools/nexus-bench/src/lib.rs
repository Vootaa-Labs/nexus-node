//! `nexus-bench` — Criterion micro-benchmark suite for Nexus hot paths.
//!
//! Benchmarks must be run with `cargo bench` from the workspace root.
//! Each benchmark file corresponds to a subsystem.
//!
//! # Benchmark files (stubs, to be populated after subsystem implementation)
//! - `benches/crypto_bench.rs`    — key generation, sign, verify throughput
//! - `benches/consensus_bench.rs` — DAG insertion, certificate aggregation
