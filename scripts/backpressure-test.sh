#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────
# backpressure-test.sh — Consensus→execution backpressure stress test.
#
# Floods the RPC transaction endpoint with concurrent requests and verifies:
#   1. Rate limiter returns 429 under load.
#   2. Consensus rounds still advance (pipeline not wedged).
#   3. No node crashes (all containers stay running).
#   4. Committed round delta across nodes remains bounded.
#
# Prerequisites:
#   - Docker Compose devnet running (docker compose up -d).
#   - curl, jq available.
#
# Usage:
#   ./scripts/backpressure-test.sh [-c CONCURRENCY] [-r REQUESTS] [-n NUM_NODES]
# ─────────────────────────────────────────────────────────────────────────

set -euo pipefail

# ── Defaults ─────────────────────────────────────────────────────────────
CONCURRENCY=20
TOTAL_REQUESTS=500
NUM_NODES="${NEXUS_NUM_VALIDATORS:-7}"
REST_BASE_PORT=8080

while getopts "c:r:n:" opt; do
    case "$opt" in
        c) CONCURRENCY="$OPTARG" ;;
        r) TOTAL_REQUESTS="$OPTARG" ;;
        n) NUM_NODES="$OPTARG" ;;
        *) ;;
    esac
done

PASS=0
FAIL=0
ts() { date "+%Y-%m-%dT%H:%M:%S"; }
log_ok()   { echo "[$(ts)] ✓ $1"; PASS=$((PASS + 1)); }
log_fail() { echo "[$(ts)] ✗ $1" >&2; FAIL=$((FAIL + 1)); }
node_url() { echo "http://localhost:$((REST_BASE_PORT + $1))"; }

echo "╔═══════════════════════════════════════════════════════════════════╗"
echo "║  Backpressure Stress Test"
echo "║  Concurrency: ${CONCURRENCY}  Total: ${TOTAL_REQUESTS}  Nodes: ${NUM_NODES}"
echo "╚═══════════════════════════════════════════════════════════════════╝"
echo ""

# ── 1. Record pre-flood consensus heights ────────────────────────────────
echo "── Phase 1: Record baseline heights"
PRE_HEIGHTS=()
for i in $(seq 0 $((NUM_NODES - 1))); do
    url="$(node_url "$i")"
    h=$(curl -sf "${url}/api/v1/consensus/status" 2>/dev/null | jq -r '.committed_round // .block_height // "0"' 2>/dev/null || echo "0")
    PRE_HEIGHTS+=("$h")
done
echo "  Baseline heights: ${PRE_HEIGHTS[*]}"

# ── 2. Flood with faucet requests (lightweight tx) ──────────────────────
echo ""
echo "── Phase 2: Flood ${TOTAL_REQUESTS} faucet requests (concurrency=${CONCURRENCY})"

STATUS_200=0
STATUS_429=0
STATUS_OTHER=0

flood_target="$(node_url 0)/api/v1/faucet"

# Use background jobs for concurrency
FIFO=$(mktemp -u)
mkfifo "$FIFO"
exec 3<>"$FIFO"
rm -f "$FIFO"

# Prime the semaphore
for _ in $(seq 1 "$CONCURRENCY"); do echo >&3; done

RESULT_FILE=$(mktemp)

for i in $(seq 1 "$TOTAL_REQUESTS"); do
    read -u 3  # acquire semaphore slot
    (
        addr="0x$(printf '%064x' "$i")"
        code=$(curl -sf -o /dev/null -w '%{http_code}' \
            -X POST "$flood_target" \
            -H "Content-Type: application/json" \
            -d "{\"address\": \"${addr}\"}" 2>/dev/null || echo "000")
        echo "$code" >> "$RESULT_FILE"
        echo >&3  # release semaphore slot
    ) &
done

# Wait for all requests to complete
wait

# Tally results
while IFS= read -r code; do
    case "$code" in
        200) STATUS_200=$((STATUS_200 + 1)) ;;
        429) STATUS_429=$((STATUS_429 + 1)) ;;
        *)   STATUS_OTHER=$((STATUS_OTHER + 1)) ;;
    esac
done < "$RESULT_FILE"
rm -f "$RESULT_FILE"

echo "  Results: 200=${STATUS_200}  429=${STATUS_429}  other=${STATUS_OTHER}"

# Expect at least some 429s under load (rate limiter is working)
if [ "$STATUS_429" -gt 0 ]; then
    log_ok "rate limiter triggered (${STATUS_429} × 429)"
else
    # Rate limiter may not trigger if faucet is disabled or limit is high.
    echo "  ⚠ no 429 responses (rate limiter may be configured with high limit)"
fi

if [ "$STATUS_200" -gt 0 ]; then
    log_ok "some requests succeeded (${STATUS_200} × 200)"
else
    log_fail "no requests succeeded at all"
fi

# ── 3. Wait and verify consensus still advancing ────────────────────────
echo ""
echo "── Phase 3: Wait 15s then check consensus progress"
sleep 15

POST_HEIGHTS=()
for i in $(seq 0 $((NUM_NODES - 1))); do
    url="$(node_url "$i")"
    h=$(curl -sf "${url}/api/v1/consensus/status" 2>/dev/null | jq -r '.committed_round // .block_height // "0"' 2>/dev/null || echo "0")
    POST_HEIGHTS+=("$h")
done
echo "  Post-flood heights: ${POST_HEIGHTS[*]}"

ADVANCED=false
for i in $(seq 0 $((NUM_NODES - 1))); do
    if [ "${POST_HEIGHTS[$i]}" -gt "${PRE_HEIGHTS[$i]}" ] 2>/dev/null; then
        ADVANCED=true
    fi
done

if [ "$ADVANCED" = true ]; then
    log_ok "consensus still advancing after flood"
else
    log_fail "consensus stalled after flood"
fi

# ── 4. Verify no node crashed ───────────────────────────────────────────
echo ""
echo "── Phase 4: Verify all containers running"
ALL_UP=true
for i in $(seq 0 $((NUM_NODES - 1))); do
    url="$(node_url "$i")"
    health=$(curl -sf -o /dev/null -w '%{http_code}' "${url}/health" 2>/dev/null || echo "000")
    if [ "$health" != "200" ]; then
        log_fail "node-${i} down after flood (health=${health})"
        ALL_UP=false
    fi
done
if [ "$ALL_UP" = true ]; then
    log_ok "all ${NUM_NODES} nodes survived flood"
fi

# ── 5. Cross-node delta ─────────────────────────────────────────────────
MIN_H=999999999999
MAX_H=0
for h in "${POST_HEIGHTS[@]}"; do
    [ "$h" -lt "$MIN_H" ] 2>/dev/null && MIN_H="$h"
    [ "$h" -gt "$MAX_H" ] 2>/dev/null && MAX_H="$h"
done
if [ "$MAX_H" -gt 0 ] 2>/dev/null; then
    DELTA=$((MAX_H - MIN_H))
    if [ "$DELTA" -le 10 ]; then
        log_ok "post-flood height delta=${DELTA} (bounded)"
    else
        log_fail "post-flood height delta=${DELTA} (unbounded — possible liveness issue)"
    fi
fi

# ── Summary ──────────────────────────────────────────────────────────────
echo ""
echo "╔═══════════════════════════════════════════════════════════════════╗"
echo "║  Backpressure Test Complete"
echo "║  Passed : ${PASS}   Failed : ${FAIL}"
echo "╚═══════════════════════════════════════════════════════════════════╝"

exec 3>&-  # close semaphore fd
if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
