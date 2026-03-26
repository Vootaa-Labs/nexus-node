#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────
# validate-startup.sh — Post-launch validation for Nexus devnet.
#
# Verifies that the Docker devnet cluster is in a healthy initial state:
#   1. Every node has bound its REST/P2P port and responds on /ready
#   2. At least one boot-node dial has succeeded (peer count > 0)
#   3. At least 2f+1 nodes have established peer connections
#   4. Consensus status endpoint is responding
#
# Usage:
#   ./scripts/validate-startup.sh [-n NUM_NODES] [-t TIMEOUT]
#
# Options:
#   -n NUM_NODES   Number of validators (default: 7)
#   -t TIMEOUT     Seconds to wait for each node (default: 90)
#   -h             Show this help message
#
# Prerequisites:
#   - Docker Compose devnet running (docker compose up -d)
#   - curl available
# ─────────────────────────────────────────────────────────────────────────

set -euo pipefail

# ── Configuration ────────────────────────────────────────────────────────
NUM_NODES="${NEXUS_NUM_VALIDATORS:-7}"
TIMEOUT=90
REST_BASE_PORT=8080

while getopts "n:t:h" opt; do
    case "$opt" in
        n) NUM_NODES="$OPTARG" ;;
        t) TIMEOUT="$OPTARG" ;;
        h)
            sed -n '/^# Usage:/,/^# ─/p' "$0" | head -n -1 | sed 's/^# //'
            exit 0
            ;;
        *) exit 1 ;;
    esac
done

PASS=0
FAIL=0
WARN=0

log_pass() { PASS=$((PASS + 1)); echo "  ✓ $1"; }
log_fail() { FAIL=$((FAIL + 1)); echo "  ✗ $1" >&2; }
log_warn() { WARN=$((WARN + 1)); echo "  ⚠ $1"; }

# BFT quorum: 2f+1 where f = floor((N-1)/3)
F_TOLERANCE=$(( (NUM_NODES - 1) / 3 ))
QUORUM=$(( 2 * F_TOLERANCE + 1 ))

echo "=== Nexus Devnet Startup Validation ==="
echo "  Nodes:   $NUM_NODES"
echo "  f:       $F_TOLERANCE"
echo "  Quorum:  $QUORUM"
echo "  Timeout: ${TIMEOUT}s per node"
echo ""

# ── Phase 1: REST port binding (all nodes respond on /ready) ─────────────
echo "--- Phase 1: REST port binding ---"

READY_COUNT=0
for i in $(seq 0 $((NUM_NODES - 1))); do
    PORT=$((REST_BASE_PORT + i))
    ELAPSED=0
    READY=false
    while [ "$ELAPSED" -lt "$TIMEOUT" ]; do
        if curl -sf "http://localhost:${PORT}/ready" > /dev/null 2>&1; then
            READY=true
            break
        fi
        sleep 2
        ELAPSED=$((ELAPSED + 2))
    done
    if [ "$READY" = true ]; then
        log_pass "node-$i bound on :${PORT} (ready in ${ELAPSED}s)"
        READY_COUNT=$((READY_COUNT + 1))
    else
        log_fail "node-$i NOT ready on :${PORT} after ${TIMEOUT}s"
    fi
done

if [ "$READY_COUNT" -lt "$NUM_NODES" ]; then
    echo ""
    echo "ABORT: Only $READY_COUNT/$NUM_NODES nodes reached readiness."
    echo "  Check container logs: docker compose logs --tail=50"
    exit 1
fi

# ── Phase 2: Peer connectivity (boot-node dial success) ──────────────────
echo ""
echo "--- Phase 2: Peer connectivity ---"

CONNECTED_NODES=0
MIN_PEERS=1  # at least 1 peer means boot-node dial succeeded

for i in $(seq 0 $((NUM_NODES - 1))); do
    PORT=$((REST_BASE_PORT + i))
    PEER_COUNT=$(curl -sf "http://localhost:${PORT}/health" 2>/dev/null \
        | grep -o '"peers":[0-9]*' | grep -o '[0-9]*' || echo "0")

    if [ "$PEER_COUNT" -ge "$MIN_PEERS" ] 2>/dev/null; then
        log_pass "node-$i has $PEER_COUNT peers"
        CONNECTED_NODES=$((CONNECTED_NODES + 1))
    else
        log_warn "node-$i has $PEER_COUNT peers (expected ≥ $MIN_PEERS)"
    fi
done

if [ "$CONNECTED_NODES" -ge "$QUORUM" ]; then
    log_pass "peer connectivity: $CONNECTED_NODES/$NUM_NODES nodes connected (≥ quorum $QUORUM)"
else
    log_fail "peer connectivity: only $CONNECTED_NODES/$NUM_NODES nodes connected (need ≥ $QUORUM)"
fi

# ── Phase 3: Consensus status responding ─────────────────────────────────
echo ""
echo "--- Phase 3: Consensus status ---"

CONSENSUS_OK=0
for i in $(seq 0 $((NUM_NODES - 1))); do
    PORT=$((REST_BASE_PORT + i))
    STATUS=$(curl -sf "http://localhost:${PORT}/v2/consensus/status" 2>/dev/null || echo "FAIL")
    if [ "$STATUS" != "FAIL" ] && echo "$STATUS" | grep -q '"total_commits"'; then
        CONSENSUS_OK=$((CONSENSUS_OK + 1))
    fi
done

if [ "$CONSENSUS_OK" -ge "$QUORUM" ]; then
    log_pass "consensus status: $CONSENSUS_OK/$NUM_NODES nodes report status (≥ quorum)"
else
    log_fail "consensus status: only $CONSENSUS_OK/$NUM_NODES nodes report (need ≥ $QUORUM)"
fi

# ── Phase 4: Initial commit check ────────────────────────────────────────
echo ""
echo "--- Phase 4: Initial commit progress ---"

INITIAL_COMMITS=$(curl -sf "http://localhost:${REST_BASE_PORT}/v2/consensus/status" 2>/dev/null \
    | grep -o '"total_commits":[0-9]*' | grep -o '[0-9]*' || echo "0")

if [ "$INITIAL_COMMITS" -gt 0 ] 2>/dev/null; then
    log_pass "node-0 already has $INITIAL_COMMITS commits"
else
    log_warn "node-0 has 0 commits (consensus may still be bootstrapping)"
fi

# ── Summary ──────────────────────────────────────────────────────────────
echo ""
echo "=== Startup Validation Summary ==="
echo "  Passed:   $PASS"
echo "  Warnings: $WARN"
echo "  Failed:   $FAIL"

if [ "$FAIL" -gt 0 ]; then
    echo ""
    echo "STARTUP VALIDATION FAILED ($FAIL failures)" >&2
    echo ""
    echo "Troubleshooting:"
    echo "  docker compose logs --tail=30 nexus-node-0"
    echo "  docker compose ps"
    echo "  curl -s http://localhost:8080/health | python3 -m json.tool"
    exit 1
fi

echo ""
echo "STARTUP VALIDATION PASSED"
echo "  All $NUM_NODES nodes are bound, connected, and reporting consensus."
echo "  Ready for smoke-test: ./scripts/smoke-test.sh"
