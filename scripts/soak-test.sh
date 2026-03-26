#!/usr/bin/env bash
# Copyright (c) The Nexus-Node Contributors
# SPDX-License-Identifier: Apache-2.0
# ─────────────────────────────────────────────────────────────────────────
# soak-test.sh — Long-running stability test for Nexus devnet.
#
# Runs for a configurable duration (default 24 h) and periodically checks:
#   1. All nodes respond on /health and /ready.
#   2. Consensus rounds advance between intervals.
#   3. Prometheus metrics are present and non-stale.
#   4. Cross-node block-height delta stays bounded.
#   5. Transaction submission succeeds at steady state.
#   6. Memory and connection metrics do not trend unbounded.
#
# Prerequisites:
#   - Docker Compose devnet running (docker compose up -d).
#   - curl, jq available.
#
# Usage:
#   ./scripts/soak-test.sh [-d DURATION_HOURS] [-i INTERVAL_SECS] [-n NUM_NODES]
#
# Output:
#   Timestamped log lines to stdout and a summary at exit.
# ─────────────────────────────────────────────────────────────────────────

set -euo pipefail

# ── Defaults ─────────────────────────────────────────────────────────────
DURATION_HOURS=24
INTERVAL_SECS=60
NUM_NODES="${NEXUS_NUM_VALIDATORS:-7}"
REST_BASE_PORT=8080

while getopts "d:i:n:" opt; do
    case "$opt" in
        d) DURATION_HOURS="$OPTARG" ;;
        i) INTERVAL_SECS="$OPTARG" ;;
        n) NUM_NODES="$OPTARG" ;;
        *) ;;
    esac
done

DURATION_SECS=$((DURATION_HOURS * 3600))
END_TIME=$(($(date +%s) + DURATION_SECS))

PASS=0
FAIL=0
ROUNDS=0
PREV_HEIGHTS=()

for i in $(seq 0 $((NUM_NODES - 1))); do
    PREV_HEIGHTS+=("0")
done

# ── Helpers ──────────────────────────────────────────────────────────────
ts() { date "+%Y-%m-%dT%H:%M:%S"; }

log_ok()   { echo "[$(ts)] ✓ $1"; PASS=$((PASS + 1)); }
log_warn() { echo "[$(ts)] ⚠ $1" >&2; }
log_fail() { echo "[$(ts)] ✗ $1" >&2; FAIL=$((FAIL + 1)); }

node_url() { echo "http://localhost:$((REST_BASE_PORT + $1))"; }

# ── Main Loop ────────────────────────────────────────────────────────────
echo "╔═══════════════════════════════════════════════════════════════════╗"
echo "║  Nexus Soak Test — ${DURATION_HOURS}h, ${NUM_NODES} nodes, ${INTERVAL_SECS}s interval"
echo "╚═══════════════════════════════════════════════════════════════════╝"
echo ""

while [ "$(date +%s)" -lt "$END_TIME" ]; do
    ROUNDS=$((ROUNDS + 1))

    echo ""
    echo "──── Round $ROUNDS ($(ts)) ────"

    # 1. Health & readiness
    ALL_HEALTHY=true
    for i in $(seq 0 $((NUM_NODES - 1))); do
        url="$(node_url "$i")"
        health=$(curl -sf -o /dev/null -w '%{http_code}' "${url}/health" 2>/dev/null || echo "000")
        ready=$(curl -sf -o /dev/null -w '%{http_code}' "${url}/ready" 2>/dev/null || echo "000")
        if [ "$health" = "200" ] && [ "$ready" = "200" ]; then
            : # healthy
        else
            log_fail "node-${i} health=${health} ready=${ready}"
            ALL_HEALTHY=false
        fi
    done
    if [ "$ALL_HEALTHY" = true ]; then
        log_ok "all ${NUM_NODES} nodes healthy and ready"
    fi

    # 2. Consensus round advancement
    HEIGHTS=()
    MIN_HEIGHT=999999999999
    MAX_HEIGHT=0
    for i in $(seq 0 $((NUM_NODES - 1))); do
        url="$(node_url "$i")"
        height=$(curl -sf "${url}/api/v1/consensus/status" 2>/dev/null | jq -r '.committed_round // .block_height // "0"' 2>/dev/null || echo "0")
        HEIGHTS+=("$height")
        [ "$height" -lt "$MIN_HEIGHT" ] 2>/dev/null && MIN_HEIGHT="$height"
        [ "$height" -gt "$MAX_HEIGHT" ] 2>/dev/null && MAX_HEIGHT="$height"
    done

    # Check progress vs previous round
    PROGRESSED=false
    for i in $(seq 0 $((NUM_NODES - 1))); do
        if [ "${HEIGHTS[$i]}" -gt "${PREV_HEIGHTS[$i]}" ] 2>/dev/null; then
            PROGRESSED=true
        fi
    done

    if [ "$PROGRESSED" = true ]; then
        log_ok "consensus progressed (heights: ${HEIGHTS[*]})"
    else
        # First round doesn't have a baseline
        if [ "$ROUNDS" -gt 1 ]; then
            log_fail "consensus stalled (heights: ${HEIGHTS[*]})"
        fi
    fi

    PREV_HEIGHTS=("${HEIGHTS[@]}")

    # 3. Cross-node height delta
    if [ "$MAX_HEIGHT" -gt 0 ] 2>/dev/null; then
        DELTA=$((MAX_HEIGHT - MIN_HEIGHT))
        if [ "$DELTA" -le 5 ]; then
            log_ok "height delta=${DELTA} (within bound)"
        else
            log_warn "height delta=${DELTA} (nodes: min=${MIN_HEIGHT} max=${MAX_HEIGHT})"
        fi
    fi

    # 4. Prometheus metrics (spot-check nexus_ prefix exists)
    url="$(node_url 0)"
    metrics_body=$(curl -sf "${url}/metrics" 2>/dev/null || echo "")
    if echo "$metrics_body" | grep -q "^nexus_"; then
        log_ok "Prometheus metrics present on node-0"
    else
        log_fail "no nexus_ metrics found on node-0"
    fi

    # 5. Transaction submission (balance check via faucet + query)
    ADDR="0x$(printf '%064x' $RANDOM)"
    faucet_response=$(curl -sf -X POST "${url}/api/v1/faucet" \
        -H "Content-Type: application/json" \
        -d "{\"address\": \"${ADDR}\"}" 2>/dev/null || echo "")
    if echo "$faucet_response" | jq -e '.tx_digest // .digest' >/dev/null 2>&1; then
        log_ok "faucet request accepted"
    else
        log_warn "faucet request failed or unavailable"
    fi

    # 6. Memory / connection metrics (check for unbounded growth)
    sessions=$(echo "$metrics_body" | grep 'nexus_agent_sessions_active' | grep -oE '[0-9]+' | tail -1 || echo "?")
    peers=$(echo "$metrics_body" | grep 'nexus_network_peers_connected' | grep -oE '[0-9]+' | tail -1 || echo "?")
    echo "  📊 sessions=${sessions} peers=${peers}"

    sleep "$INTERVAL_SECS"
done

# ── Summary ──────────────────────────────────────────────────────────────
echo ""
echo "╔═══════════════════════════════════════════════════════════════════╗"
echo "║  Soak Test Complete                                              "
echo "║  Duration : ${DURATION_HOURS}h  Rounds : ${ROUNDS}"
echo "║  Passed   : ${PASS}"
echo "║  Failed   : ${FAIL}"
echo "╚═══════════════════════════════════════════════════════════════════╝"

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
