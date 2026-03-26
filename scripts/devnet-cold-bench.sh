#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────
# devnet-cold-bench.sh — Rebuild image, cold-start devnet, rerun lifecycle
# benchmark, and capture fresh metrics.
#
# Fixed flow:
#   1. Rebuild Docker image from current source
#   2. Regenerate devnet layout with empty data directories
#   3. Start devnet and validate startup
#   4. Rerun lifecycle benchmark
#   5. Capture fresh Prometheus metrics snapshots per node
#
# Usage:
#   ./scripts/devnet-cold-bench.sh [OPTIONS]
#
# Options:
#   -n NUM_VALIDATORS    Number of validators (default: 7)
#   -s NUM_SHARDS        Number of shards (default: 2)
#   -o OUTPUT_DIR        Devnet output directory (default: ./devnet-n7s)
#   -i IMAGE             Docker image tag (default: nexus-node)
#   -w WORKERS           Worker sweep, comma-separated (default: 10,12,16)
#   -t TXS_PER_WORKER    Transactions per worker (default: 10)
#   -c TIMEOUT_MS        Confirmation timeout ms (default: 60000)
#   -p POLL_MS           Poll interval ms (default: 1000)
#   -j JSON_OUT          Benchmark JSON output path
#   -e REPORT_EN         English report output path
#   -z REPORT_ZH         Chinese report output path
#   -h                   Show help
# ─────────────────────────────────────────────────────────────────────────

set -euo pipefail

NUM_VALIDATORS=7
NUM_SHARDS=2
OUTPUT_DIR="./devnet-n7s"
IMAGE="nexus-node"
WORKERS="10,12,16"
TXS_PER_WORKER=10
CONFIRM_TIMEOUT_MS=60000
POLL_INTERVAL_MS=1000
JSON_OUT="target/devnet-bench/devnet_benchmark_lifecycle_probe_results.json"
REPORT_EN="Docs/en/Report/Benchmark/Devnet_Benchmark_Report_Lifecycle_Probe_v0.1.13.md"
REPORT_ZH="Docs/zh/Report/Benchmark/Devnet_Benchmark_Report_Lifecycle_Probe_v0.1.13.md"
METRICS_DIR="target/devnet-bench/metrics"
REST_BASE_PORT=8080

usage() {
    sed -n '/^# Usage:/,/^# ─/p' "$0" | head -n -1 | sed 's/^# //'
    exit "${1:-0}"
}

while getopts "n:s:o:i:w:t:c:p:j:e:z:h" opt; do
    case "$opt" in
        n) NUM_VALIDATORS="$OPTARG" ;;
        s) NUM_SHARDS="$OPTARG" ;;
        o) OUTPUT_DIR="$OPTARG" ;;
        i) IMAGE="$OPTARG" ;;
        w) WORKERS="$OPTARG" ;;
        t) TXS_PER_WORKER="$OPTARG" ;;
        c) CONFIRM_TIMEOUT_MS="$OPTARG" ;;
        p) POLL_INTERVAL_MS="$OPTARG" ;;
        j) JSON_OUT="$OPTARG" ;;
        e) REPORT_EN="$OPTARG" ;;
        z) REPORT_ZH="$OPTARG" ;;
        h) usage 0 ;;
        *) usage 1 ;;
    esac
done

log_step() {
    echo ""
    echo "=== $1 ==="
}

mkdir -p "$(dirname "$JSON_OUT")" "$(dirname "$REPORT_EN")" "$(dirname "$REPORT_ZH")" "$METRICS_DIR"

log_step "Step 1: Stop existing devnet"
docker compose down

log_step "Step 2: Rebuild image from current source"
DOCKER_BUILDKIT=1 docker build -t "$IMAGE" .

log_step "Step 3: Regenerate cold devnet layout"
./scripts/setup-devnet.sh -n "$NUM_VALIDATORS" -s "$NUM_SHARDS" -o "$OUTPUT_DIR" -f

log_step "Step 4: Start fresh devnet"
NEXUS_IMAGE="$IMAGE" docker compose up -d

log_step "Step 5: Validate startup"
NEXUS_NUM_VALIDATORS="$NUM_VALIDATORS" ./scripts/validate-startup.sh -n "$NUM_VALIDATORS"

log_step "Step 6: Run lifecycle benchmark"
cargo run -p nexus-bench --bin devnet_bench --release -- \
    --workers "$WORKERS" \
    --txs-per-worker "$TXS_PER_WORKER" \
    --num-shards "$NUM_SHARDS" \
    --confirm-timeout-ms "$CONFIRM_TIMEOUT_MS" \
    --poll-interval-ms "$POLL_INTERVAL_MS" \
    --json-out "$JSON_OUT" \
    --report-en "$REPORT_EN" \
    --report-zh "$REPORT_ZH"

log_step "Step 7: Capture fresh metrics snapshots"
rm -f "$METRICS_DIR"/*.prom "$METRICS_DIR"/summary.txt
for i in $(seq 0 $((NUM_VALIDATORS - 1))); do
    port=$((REST_BASE_PORT + i))
    node_name="node-$i"
    metrics_path="$METRICS_DIR/${node_name}.prom"
    curl -fsS "http://127.0.0.1:${port}/metrics" > "$metrics_path"
    {
        echo "[${node_name}]"
        grep -E '^(nexus_batch_proposals_total|nexus_batch_proposal_txs_total|nexus_mempool_enqueue_total|nexus_mempool_dequeue_total|nexus_consensus_certificates_(accepted|committed|rejected)_total|nexus_bridge_batches_executed_total|nexus_network_rate_limit_exceeded_total)' "$metrics_path" || true
        grep -E '^nexus_consensus_certificates_rejected_total\{reason=' "$metrics_path" || true
        echo ""
    } >> "$METRICS_DIR/summary.txt"
done

log_step "Artifacts"
echo "Benchmark JSON:   $JSON_OUT"
echo "English report:   $REPORT_EN"
echo "Chinese report:   $REPORT_ZH"
echo "Metrics dir:      $METRICS_DIR"
echo "Metrics summary:  $METRICS_DIR/summary.txt"
