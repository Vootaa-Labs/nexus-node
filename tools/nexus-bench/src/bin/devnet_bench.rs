// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Multi-node devnet TPS and latency benchmark.
//!
//! Submits transfer transactions through worker threads, then polls
//! receipt visibility across all devnet nodes to compute local and
//! cluster-wide throughput and latency percentiles.
//!
//! Usage:
//!   cargo run -p nexus-bench --bin devnet_bench --release -- [OPTIONS]

use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use clap::Parser;
use nexus_execution::types::{TransactionBody, TransactionPayload};
use nexus_primitives::{AccountAddress, Amount, EpochNumber, TokenId};
use nexus_wallet::rpc_client::{
    ephemeral_identity, http_agent, query_balance, request_faucet, sign_transaction,
    submit_transaction, validate_rpc_url, Identity,
};
use serde::Serialize;

// ── CLI ─────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(
    name = "devnet-bench",
    about = "Multi-node devnet TPS and latency benchmark"
)]
struct Cli {
    /// Comma-separated list of devnet node URLs.
    #[arg(
        long,
        value_delimiter = ',',
        default_value = "http://127.0.0.1:8080,http://127.0.0.1:8081,http://127.0.0.1:8082,http://127.0.0.1:8083,http://127.0.0.1:8084,http://127.0.0.1:8085,http://127.0.0.1:8086"
    )]
    nodes: Vec<String>,

    /// Comma-separated worker counts for the sweep.
    #[arg(long, value_delimiter = ',', default_value = "1,2,4,8")]
    workers: Vec<usize>,

    /// Transactions per worker per tier.
    #[arg(long, default_value_t = 10)]
    txs_per_worker: u64,

    /// Transfer amount per transaction (smallest unit).
    #[arg(long, default_value_t = 1000)]
    amount: u64,

    /// Number of shards in the devnet.
    #[arg(long, default_value_t = 2)]
    num_shards: u16,

    /// Chain ID.
    #[arg(long, default_value_t = 1)]
    chain_id: u64,

    /// Confirmation poll timeout in milliseconds.
    #[arg(long, default_value_t = 30000)]
    confirm_timeout_ms: u64,

    /// Confirmation poll interval in milliseconds.
    #[arg(long, default_value_t = 200)]
    poll_interval_ms: u64,

    /// Path for JSON output.
    #[arg(
        long,
        default_value = "target/devnet-bench/devnet_benchmark_results.json"
    )]
    json_out: PathBuf,

    /// Path for English markdown report.
    #[arg(
        long,
        default_value = "Docs/en/Report/Benchmark/Devnet_Benchmark_Report_v0.1.13.md"
    )]
    report_en: PathBuf,

    /// Path for Chinese markdown report.
    #[arg(
        long,
        default_value = "Docs/zh/Report/Benchmark/Devnet_Benchmark_Report_v0.1.13.md"
    )]
    report_zh: PathBuf,
}

// ── Data types ──────────────────────────────────────────────────────────

#[derive(Debug)]
struct SubmittedTx {
    digest: String,
    submit_node: usize,
    submitted_at: Instant,
}

#[derive(Debug)]
struct ObservedTx {
    local_confirmed_at: Option<Instant>,
    cluster_confirmed_at: Option<Instant>,
    seen_nodes: Vec<bool>,
}

#[derive(Debug, Serialize)]
struct Percentiles {
    min_ms: f64,
    p50_ms: f64,
    p95_ms: f64,
    p99_ms: f64,
    max_ms: f64,
}

#[derive(Debug, Serialize)]
struct TierReport {
    workers: usize,
    planned_transactions: usize,
    confirmed_local: usize,
    confirmed_cluster: usize,
    local_tps: f64,
    cluster_visibility_tps: f64,
    local_latency_ms: Option<Percentiles>,
    cluster_visibility_latency_ms: Option<Percentiles>,
    unconfirmed_local: usize,
    unconfirmed_cluster: usize,
}

#[derive(Debug, Serialize)]
struct DevnetBenchmarkReport {
    generated_at_unix_ms: u128,
    nodes: Vec<String>,
    workers_sweep: Vec<usize>,
    txs_per_worker: u64,
    amount_smallest_unit: u64,
    num_shards: u16,
    chain_id: u64,
    confirm_timeout_ms: u64,
    poll_interval_ms: u64,
    tiers: Vec<TierReport>,
}

// ── Main ────────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.nodes.is_empty() {
        bail!("--nodes cannot be empty");
    }
    if cli.workers.is_empty() {
        bail!("--workers cannot be empty");
    }

    for node in &cli.nodes {
        validate_rpc_url(node)?;
    }

    wait_for_nodes(&cli.nodes)?;

    let mut tiers = Vec::new();
    for &worker_count in &cli.workers {
        let identities = fund_identities(&cli.nodes[0], worker_count, cli.chain_id)?;
        let tier = run_tier(&cli, &identities, worker_count)
            .with_context(|| format!("running devnet tier with {worker_count} workers"))?;
        tiers.push(tier);
    }

    let report = DevnetBenchmarkReport {
        generated_at_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
        nodes: cli.nodes.clone(),
        workers_sweep: cli.workers.clone(),
        txs_per_worker: cli.txs_per_worker,
        amount_smallest_unit: cli.amount,
        num_shards: cli.num_shards,
        chain_id: cli.chain_id,
        confirm_timeout_ms: cli.confirm_timeout_ms,
        poll_interval_ms: cli.poll_interval_ms,
        tiers,
    };

    write_outputs(&report, &cli.json_out, &cli.report_en, &cli.report_zh)?;
    println!("Devnet benchmark JSON:      {}", cli.json_out.display());
    println!("Devnet benchmark EN report: {}", cli.report_en.display());
    println!("Devnet benchmark ZH report: {}", cli.report_zh.display());

    Ok(())
}

// ── Node readiness ──────────────────────────────────────────────────────

fn wait_for_nodes(nodes: &[String]) -> Result<()> {
    println!("Waiting for {} node(s) to become ready...", nodes.len());
    for node in nodes {
        let ready_url = format!("{}/ready", node.trim_end_matches('/'));
        let start = Instant::now();
        loop {
            match http_agent().get(&ready_url).call() {
                Ok(response) if response.status() == 200 => break,
                Ok(_) | Err(_) => {}
            }
            if start.elapsed() > Duration::from_secs(60) {
                bail!("node not ready after 60 s: {node}");
            }
            thread::sleep(Duration::from_millis(500));
        }
    }
    println!("All nodes ready.");
    Ok(())
}

// ── Identity funding ────────────────────────────────────────────────────

fn fund_identities(rpc_url: &str, workers: usize, _chain_id: u64) -> Result<Vec<Identity>> {
    println!("Funding {workers} worker identities via faucet...");
    let mut identities = Vec::with_capacity(workers);
    for i in 0..workers {
        let identity = ephemeral_identity();
        request_faucet(rpc_url, &identity.address)
            .with_context(|| format!("requesting faucet for worker {i}"))?;
        let funded = wait_for_balance(rpc_url, &identity, 1, 60, 250).with_context(|| {
            format!(
                "waiting for faucet balance for {}",
                identity.address.to_hex()
            )
        })?;
        if !funded {
            bail!(
                "faucet funding did not settle for {}",
                identity.address.to_hex()
            );
        }
        identities.push(identity);
    }
    println!("All {workers} identities funded.");
    Ok(identities)
}

fn wait_for_balance(
    rpc_url: &str,
    identity: &Identity,
    minimum_balance: u64,
    max_attempts: u32,
    interval_ms: u64,
) -> Result<bool> {
    for _ in 0..max_attempts {
        if let Ok(balance) = query_balance(rpc_url, &identity.address) {
            if balance.balance >= minimum_balance {
                return Ok(true);
            }
        }
        thread::sleep(Duration::from_millis(interval_ms));
    }
    Ok(false)
}

// ── Tier execution ──────────────────────────────────────────────────────

fn run_tier(cli: &Cli, identities: &[Identity], workers: usize) -> Result<TierReport> {
    println!("── Tier: {workers} workers × {} txs ──", cli.txs_per_worker);
    let tier_start = Instant::now();
    let mut handles = Vec::with_capacity(workers);

    for (worker_index, funded_identity) in identities.iter().take(workers).enumerate() {
        let node = cli.nodes[worker_index % cli.nodes.len()].clone();
        let identity = identity_clone(funded_identity)?;
        let txs_per_worker = cli.txs_per_worker;
        let amount = cli.amount;
        let chain_id = cli.chain_id;
        let num_shards = cli.num_shards;

        handles.push(thread::spawn(move || -> Result<Vec<SubmittedTx>> {
            let mut submissions = Vec::with_capacity(txs_per_worker as usize);
            for nonce in 0..txs_per_worker {
                let recipient = deterministic_recipient(worker_index as u64, nonce);
                let body = TransactionBody {
                    sender: identity.address,
                    sequence_number: nonce,
                    expiry_epoch: EpochNumber(u64::MAX),
                    gas_limit: 100_000,
                    gas_price: 1,
                    target_shard: Some(derive_target_shard(identity.address, num_shards)),
                    payload: TransactionPayload::Transfer {
                        recipient,
                        amount: Amount(amount),
                        token: TokenId::Native,
                    },
                    chain_id,
                };
                let signed = sign_transaction(&identity, body)?;
                let submitted_at = Instant::now();
                let response = submit_transaction(&node, &signed)
                    .with_context(|| format!("submitting tx to {node}"))?;
                if !response.accepted {
                    bail!("node rejected transaction {}", response.tx_digest);
                }
                submissions.push(SubmittedTx {
                    digest: response.tx_digest,
                    submit_node: worker_index,
                    submitted_at,
                });
            }
            Ok(submissions)
        }));
    }

    let mut submissions = Vec::new();
    for handle in handles {
        submissions.extend(
            handle
                .join()
                .map_err(|_| anyhow!("worker thread panicked"))??,
        );
    }

    let observed = observe_transactions(
        &cli.nodes,
        &submissions,
        Duration::from_millis(cli.confirm_timeout_ms),
        Duration::from_millis(cli.poll_interval_ms),
    )?;

    let mut local_latencies = Vec::new();
    let mut cluster_latencies = Vec::new();
    let mut local_end = tier_start;
    let mut cluster_end = tier_start;
    let mut confirmed_local = 0usize;
    let mut confirmed_cluster = 0usize;

    for (submission, observation) in submissions.iter().zip(observed.iter()) {
        if let Some(local_confirmed_at) = observation.local_confirmed_at {
            confirmed_local += 1;
            local_end = local_end.max(local_confirmed_at);
            local_latencies.push(duration_ms(
                local_confirmed_at.duration_since(submission.submitted_at),
            ));
        }
        if let Some(cluster_confirmed_at) = observation.cluster_confirmed_at {
            confirmed_cluster += 1;
            cluster_end = cluster_end.max(cluster_confirmed_at);
            cluster_latencies.push(duration_ms(
                cluster_confirmed_at.duration_since(submission.submitted_at),
            ));
        }
    }

    let total_elapsed_local = if confirmed_local > 0 {
        duration_s(local_end.duration_since(tier_start))
    } else {
        0.0
    };
    let total_elapsed_cluster = if confirmed_cluster > 0 {
        duration_s(cluster_end.duration_since(tier_start))
    } else {
        0.0
    };

    let tier = TierReport {
        workers,
        planned_transactions: submissions.len(),
        confirmed_local,
        confirmed_cluster,
        local_tps: rate(confirmed_local, total_elapsed_local),
        cluster_visibility_tps: rate(confirmed_cluster, total_elapsed_cluster),
        local_latency_ms: percentiles(&mut local_latencies),
        cluster_visibility_latency_ms: percentiles(&mut cluster_latencies),
        unconfirmed_local: submissions.len().saturating_sub(confirmed_local),
        unconfirmed_cluster: submissions.len().saturating_sub(confirmed_cluster),
    };

    println!(
        "   Local  TPS: {:.2}  ({}/{} confirmed)",
        tier.local_tps, tier.confirmed_local, tier.planned_transactions
    );
    println!(
        "   Cluster TPS: {:.2}  ({}/{} cluster-visible)",
        tier.cluster_visibility_tps, tier.confirmed_cluster, tier.planned_transactions
    );

    Ok(tier)
}

// ── Transaction observation ─────────────────────────────────────────────

fn observe_transactions(
    nodes: &[String],
    submissions: &[SubmittedTx],
    timeout: Duration,
    poll_interval: Duration,
) -> Result<Vec<ObservedTx>> {
    let start = Instant::now();
    let agent = http_agent();
    let mut observed: Vec<_> = submissions
        .iter()
        .map(|_| ObservedTx {
            local_confirmed_at: None,
            cluster_confirmed_at: None,
            seen_nodes: vec![false; nodes.len()],
        })
        .collect();

    while start.elapsed() <= timeout {
        let now = Instant::now();
        let mut pending = false;

        for (index, submission) in submissions.iter().enumerate() {
            let local_node_index = submission.submit_node % nodes.len();

            // Check local node receipt visibility first.
            if observed[index].local_confirmed_at.is_none()
                && tx_visible(&agent, &nodes[local_node_index], &submission.digest)?
            {
                observed[index].local_confirmed_at = Some(now);
                observed[index].seen_nodes[local_node_index] = true;
            }

            // Check cluster-wide receipt visibility.
            if observed[index].cluster_confirmed_at.is_none() {
                pending = true;
                for (node_index, node) in nodes.iter().enumerate() {
                    if !observed[index].seen_nodes[node_index]
                        && tx_visible(&agent, node, &submission.digest)?
                    {
                        observed[index].seen_nodes[node_index] = true;
                    }
                }

                if observed[index].seen_nodes.iter().all(|seen| *seen) {
                    observed[index].cluster_confirmed_at = Some(now);
                }
            }
        }

        if !pending
            || observed
                .iter()
                .all(|entry| entry.cluster_confirmed_at.is_some())
        {
            break;
        }
        thread::sleep(poll_interval);
    }

    Ok(observed)
}

/// Check whether a transaction receipt is visible on a given node.
fn tx_visible(agent: &ureq::Agent, node: &str, digest: &str) -> Result<bool> {
    let url = format!("{}/v2/tx/{digest}/status", node.trim_end_matches('/'));
    match agent.get(&url).call() {
        Ok(response) => Ok(response.status() == 200),
        Err(ureq::Error::Status(404, _)) => Ok(false),
        Err(ureq::Error::Status(429, _)) => Ok(false),
        Err(ureq::Error::Transport(_)) => Ok(false),
        Err(error) => Err(anyhow!(error)).with_context(|| format!("querying tx status at {url}")),
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────

fn identity_clone(identity: &Identity) -> Result<Identity> {
    let sk = nexus_crypto::DilithiumSigningKey::from_bytes(identity.sk.as_bytes())
        .context("cloning signing key")?;
    let pk = nexus_crypto::DilithiumVerifyKey::from_bytes(identity.pk.as_bytes())
        .context("cloning verify key")?;
    Ok(Identity {
        sk,
        pk,
        address: identity.address,
    })
}

fn deterministic_recipient(worker: u64, nonce: u64) -> AccountAddress {
    let mut bytes = [0u8; 32];
    bytes[..8].copy_from_slice(&worker.to_le_bytes());
    bytes[8..16].copy_from_slice(&nonce.to_le_bytes());
    AccountAddress(bytes)
}

fn derive_target_shard(sender: AccountAddress, num_shards: u16) -> nexus_primitives::ShardId {
    nexus_intent::resolver::shard_lookup::jump_consistent_hash(&sender, num_shards)
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn duration_s(duration: Duration) -> f64 {
    duration.as_secs_f64()
}

fn rate(count: usize, seconds: f64) -> f64 {
    if count == 0 || seconds <= f64::EPSILON {
        0.0
    } else {
        count as f64 / seconds
    }
}

fn percentiles(values: &mut [f64]) -> Option<Percentiles> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|left, right| left.partial_cmp(right).unwrap_or(Ordering::Equal));
    Some(Percentiles {
        min_ms: values[0],
        p50_ms: percentile(values, 0.50),
        p95_ms: percentile(values, 0.95),
        p99_ms: percentile(values, 0.99),
        max_ms: values[values.len() - 1],
    })
}

fn percentile(values: &[f64], p: f64) -> f64 {
    let last = values.len().saturating_sub(1);
    let index = ((last as f64) * p).round() as usize;
    values[index.min(last)]
}

// ── Output ──────────────────────────────────────────────────────────────

fn write_outputs(
    report: &DevnetBenchmarkReport,
    json_out: &Path,
    report_en: &Path,
    report_zh: &Path,
) -> Result<()> {
    if let Some(parent) = json_out.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Some(parent) = report_en.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Some(parent) = report_zh.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::write(json_out, serde_json::to_vec_pretty(report)?)?;
    fs::write(report_en, render_markdown_en(report))?;
    fs::write(report_zh, render_markdown_zh(report))?;
    Ok(())
}

// ── Report rendering ────────────────────────────────────────────────────

fn render_markdown_en(report: &DevnetBenchmarkReport) -> String {
    let mut lines = vec![
        "# Devnet Benchmark Report v0.1.13".to_string(),
        String::new(),
        "This report is generated by `cargo run -p nexus-bench --bin devnet_bench --release -- ...`.".to_string(),
        "Numbers below are devnet and cluster-visibility proxies, not production chain claims.".to_string(),
        String::new(),
        "## Configuration".to_string(),
        String::new(),
        "- Environment: shared-host Docker devnet".to_string(),
        "- Interpretation boundary: results must be read against the shared-host constraint, not as isolated bare-metal capacity".to_string(),
        format!("- Nodes: {}", report.nodes.join(", ")),
        format!("- Worker sweep: {:?}", report.workers_sweep),
        format!("- Transactions per worker: {}", report.txs_per_worker),
        format!("- Transfer amount: {}", report.amount_smallest_unit),
        format!("- Shards: {}", report.num_shards),
        format!("- Confirmation timeout: {} ms", report.confirm_timeout_ms),
        format!("- Poll interval: {} ms", report.poll_interval_ms),
        String::new(),
        "## Interpretation".to_string(),
        String::new(),
        "- `local_tps`: confirmed transactions per second from submit start until receipts become visible on the submit node.".to_string(),
        "- `cluster_visibility_tps`: confirmed transactions per second until receipts become visible on every sampled node.".to_string(),
        "- `cluster_visibility_latency_ms`: a finality proxy based on cross-node receipt visibility, not a formal BFT finality proof.".to_string(),
        String::new(),
        "## Results".to_string(),
        String::new(),
        "| Workers | Planned TXs | Confirmed Local | Confirmed Cluster | Local TPS | Cluster TPS | Local P50/P95/P99 (ms) | Cluster P50/P95/P99 (ms) |".to_string(),
        "| --- | --- | --- | --- | --- | --- | --- | --- |".to_string(),
    ];

    for tier in &report.tiers {
        lines.push(format!(
            "| {} | {} | {} | {} | {:.2} | {:.2} | {} | {} |",
            tier.workers,
            tier.planned_transactions,
            tier.confirmed_local,
            tier.confirmed_cluster,
            tier.local_tps,
            tier.cluster_visibility_tps,
            format_percentile_triplet(tier.local_latency_ms.as_ref()),
            format_percentile_triplet(tier.cluster_visibility_latency_ms.as_ref()),
        ));
    }

    lines.push(String::new());
    lines.push("## Public Messaging Guidance".to_string());
    lines.push(String::new());
    lines.push("- Safe wording: \"7-node devnet sweep\", \"cluster visibility proxy\", \"receipt visibility latency\", \"testnet steady-state benchmark\".".to_string());
    lines.push("- Avoid wording: \"mainnet TPS\", \"formal finality latency\", or any production capacity claim unless matched by dedicated release-grade evidence.".to_string());
    lines.push(String::new());
    lines.join("\n")
}

fn render_markdown_zh(report: &DevnetBenchmarkReport) -> String {
    let mut lines = vec![
        "# Devnet Benchmark Report v0.1.13".to_string(),
        String::new(),
        "本报告由 `cargo run -p nexus-bench --bin devnet_bench --release -- ...` 自动生成。".to_string(),
        "以下数字属于 devnet 与集群可见性代理指标，不应直接等同于生产链路口径。".to_string(),
        String::new(),
        "## 配置".to_string(),
        String::new(),
        "- 环境: shared-host Docker devnet".to_string(),
        "- 解读边界: 所有结果都必须放在 shared-host 约束下理解，不能脱离该背景引用为独占裸机容量".to_string(),
        format!("- 节点: {}", report.nodes.join(", ")),
        format!("- 并发 sweep: {:?}", report.workers_sweep),
        format!("- 每个 worker 交易数: {}", report.txs_per_worker),
        format!("- 转账金额: {}", report.amount_smallest_unit),
        format!("- 分片数: {}", report.num_shards),
        format!("- 确认超时: {} ms", report.confirm_timeout_ms),
        format!("- 轮询间隔: {} ms", report.poll_interval_ms),
        String::new(),
        "## 指标解释".to_string(),
        String::new(),
        "- `local_tps`: 从开始提交到提交节点可见 receipt 为止的确认吞吐。".to_string(),
        "- `cluster_visibility_tps`: 从开始提交到所有采样节点都可见 receipt 为止的集群可见性吞吐。".to_string(),
        "- `cluster_visibility_latency_ms`: 基于跨节点 receipt 可见性的 finality 代理，不是形式化 BFT finality 证明。".to_string(),
        String::new(),
        "## 结果".to_string(),
        String::new(),
        "| Workers | 计划交易数 | 本地确认数 | 集群可见数 | Local TPS | Cluster TPS | Local P50/P95/P99 (ms) | Cluster P50/P95/P99 (ms) |".to_string(),
        "| --- | --- | --- | --- | --- | --- | --- | --- |".to_string(),
    ];

    for tier in &report.tiers {
        lines.push(format!(
            "| {} | {} | {} | {} | {:.2} | {:.2} | {} | {} |",
            tier.workers,
            tier.planned_transactions,
            tier.confirmed_local,
            tier.confirmed_cluster,
            tier.local_tps,
            tier.cluster_visibility_tps,
            format_percentile_triplet(tier.local_latency_ms.as_ref()),
            format_percentile_triplet(tier.cluster_visibility_latency_ms.as_ref()),
        ));
    }

    lines.push(String::new());
    lines.push("## 对外口径建议".to_string());
    lines.push(String::new());
    lines.push("- 可以使用 \"7 节点 devnet sweep\"、\"集群可见性代理指标\"、\"receipt 可见延迟\"、\"测试网稳态 benchmark\" 这类表述。".to_string());
    lines.push("- 不要直接写成 \"主网 TPS\"、\"正式 finality 延迟\" 或任何生产容量承诺，除非后续有专门的发布级证据链支撑。".to_string());
    lines.push(String::new());
    lines.join("\n")
}

fn format_percentile_triplet(percentiles: Option<&Percentiles>) -> String {
    match percentiles {
        Some(values) => format!(
            "{:.2}/{:.2}/{:.2}",
            values.p50_ms, values.p95_ms, values.p99_ms
        ),
        None => "n/a".to_string(),
    }
}
