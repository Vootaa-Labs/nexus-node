#!/usr/bin/env zsh
# Copyright (c) The Nexus-Node Contributors
# SPDX-License-Identifier: Apache-2.0
# ─────────────────────────────────────────────────────────────────────────
# release-go-nogo.sh  (v0.1.10 — X-2)
#
# Automated go/no-go pre-release checklist.
#
# Maps each criterion from Docs/Ops/Testnet_Release_Runbook.md §5 to an
# automated check. Produces a machine-readable verdict (JSON) and a
# human-readable summary.
#
# Usage:
#   scripts/release-go-nogo.sh [--json]
#
# Options:
#   --json   Print only the JSON verdict (for CI consumption).
#
# Exit codes:
#   0 — GO (all checks pass)
#   1 — NO-GO (one or more checks failed)
# ─────────────────────────────────────────────────────────────────────────
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

JSON_ONLY=false
if [[ "${1:-}" == "--json" ]]; then
  JSON_ONLY=true
fi

# ── Result tracking ──────────────────────────────────────────────────────

typeset -a CHECK_NAMES
typeset -a CHECK_RESULTS
typeset -a CHECK_DETAILS
total_pass=0
total_fail=0
total_skip=0

record() {
  local name="$1" result="$2" detail="${3:-}"
  CHECK_NAMES+=("$name")
  CHECK_RESULTS+=("$result")
  CHECK_DETAILS+=("$detail")
  case "$result" in
    pass) (( total_pass++ )) || true ;;
    fail) (( total_fail++ )) || true ;;
    skip) (( total_skip++ )) || true ;;
  esac
}

log() {
  if ! $JSON_ONLY; then
    echo "$@"
  fi
}

# ── Check 1: Workspace compiles ──────────────────────────────────────────

log "▸ Check 1: Workspace compiles..."
if cargo check --workspace 2>/dev/null; then
  record "workspace_compiles" "pass"
  log "  ✅ PASS"
else
  record "workspace_compiles" "fail" "cargo check failed"
  log "  ❌ FAIL: cargo check failed"
fi

# ── Check 2: Lint (fmt + clippy) ─────────────────────────────────────────

log "▸ Check 2: Lint..."
fmt_ok=true
clippy_ok=true

if ! cargo fmt --all -- --check 2>/dev/null; then
  fmt_ok=false
fi

if ! cargo clippy --workspace --all-targets -- -D warnings 2>/dev/null; then
  clippy_ok=false
fi

if $fmt_ok && $clippy_ok; then
  record "lint" "pass"
  log "  ✅ PASS"
else
  detail=""
  $fmt_ok || detail="fmt failed"
  $clippy_ok || detail="${detail:+$detail; }clippy failed"
  record "lint" "fail" "$detail"
  log "  ❌ FAIL: $detail"
fi

# ── Check 3: Full test suite ─────────────────────────────────────────────

log "▸ Check 3: Full test suite..."
test_output=$(cargo test 2>&1)
test_exit=$?

if [[ $test_exit -eq 0 ]]; then
  # Count test results.
  pass_count=$(echo "$test_output" | grep "test result:" | grep -oE '[0-9]+ passed' | awk '{sum+=$1} END{print sum+0}')
  fail_count=$(echo "$test_output" | grep "test result:" | grep -oE '[0-9]+ failed' | awk '{sum+=$1} END{print sum+0}')
  record "test_suite" "pass" "${pass_count} passed, ${fail_count} failed"
  log "  ✅ PASS: ${pass_count} tests passed"
else
  fail_count=$(echo "$test_output" | grep "test result:" | grep -oE '[0-9]+ failed' | awk '{sum+=$1} END{print sum+0}')
  record "test_suite" "fail" "${fail_count} tests failed"
  log "  ❌ FAIL: ${fail_count} tests failed"
fi

# ── Check 4: Config-doc drift ────────────────────────────────────────────

log "▸ Check 4: Config-doc drift..."
if zsh scripts/config-doc-drift-check.sh 2>/dev/null; then
  record "config_doc_drift" "pass"
  log "  ✅ PASS"
else
  record "config_doc_drift" "fail" "drift detected"
  log "  ❌ FAIL: drift detected"
fi

# ── Check 5: Access policy drift ─────────────────────────────────────────

log "▸ Check 5: Access policy drift..."
if [[ -f scripts/access-policy-drift-check.sh ]]; then
  if zsh scripts/access-policy-drift-check.sh 2>/dev/null; then
    record "access_policy_drift" "pass"
    log "  ✅ PASS"
  else
    record "access_policy_drift" "fail" "access policy drift"
    log "  ❌ FAIL: access policy drift"
  fi
else
  record "access_policy_drift" "skip" "script not found"
  log "  ⏭ SKIP: access-policy-drift-check.sh not found"
fi

# ── Check 6: Security audit (cargo-audit/deny) ──────────────────────────

log "▸ Check 6: Security audit..."
audit_ok=true
if command -v cargo-audit &>/dev/null; then
  if ! cargo audit 2>/dev/null; then
    audit_ok=false
  fi
else
  log "  ⚠️  cargo-audit not installed, checking with cargo-deny"
fi

deny_ok=true
if command -v cargo-deny &>/dev/null; then
  if ! cargo deny check advisories 2>/dev/null; then
    deny_ok=false
  fi
else
  log "  ⚠️  cargo-deny not installed"
fi

if $audit_ok && $deny_ok; then
  record "security_audit" "pass"
  log "  ✅ PASS"
elif ! command -v cargo-audit &>/dev/null && ! command -v cargo-deny &>/dev/null; then
  record "security_audit" "skip" "neither cargo-audit nor cargo-deny installed"
  log "  ⏭ SKIP: no audit tooling installed"
else
  record "security_audit" "fail" "vulnerabilities detected"
  log "  ❌ FAIL: vulnerabilities detected"
fi

# ── Check 7: B-1 epoch stress tests ─────────────────────────────────────

log "▸ Check 7: B-1 epoch stress tests..."
if cargo test -p nexus-test-utils --lib epoch_stress_tests 2>/dev/null; then
  record "epoch_stress_tests" "pass"
  log "  ✅ PASS"
else
  record "epoch_stress_tests" "fail" "epoch stress tests failed"
  log "  ❌ FAIL"
fi

# ── Check 8: B-2 governance recovery tests ───────────────────────────────

log "▸ Check 8: B-2 governance recovery tests..."
if cargo test -p nexus-test-utils --lib governance_recovery_tests 2>/dev/null; then
  record "governance_recovery_tests" "pass"
  log "  ✅ PASS"
else
  record "governance_recovery_tests" "fail" "governance tests failed"
  log "  ❌ FAIL"
fi

# ── Check 9: B-3 epoch e2e consistency tests ─────────────────────────────

log "▸ Check 9: B-3 epoch e2e consistency tests..."
if cargo test -p nexus-test-utils --lib epoch_e2e_consistency_tests 2>/dev/null; then
  record "epoch_e2e_consistency" "pass"
  log "  ✅ PASS"
else
  record "epoch_e2e_consistency" "fail" "e2e consistency tests failed"
  log "  ❌ FAIL"
fi

# ── Check 10: C-2 proof smoke tests ─────────────────────────────────────

log "▸ Check 10: C-2 proof smoke tests..."
if cargo test -p nexus-test-utils --lib proof_smoke_tests 2>/dev/null; then
  record "proof_smoke_tests" "pass"
  log "  ✅ PASS"
else
  record "proof_smoke_tests" "fail" "proof smoke tests failed"
  log "  ❌ FAIL"
fi

# ── Check 11: D-1 gas calibration tests ─────────────────────────────────

log "▸ Check 11: D-1 gas calibration tests..."
if cargo test -p nexus-execution --lib gas_calibration 2>/dev/null; then
  record "gas_calibration" "pass"
  log "  ✅ PASS"
else
  record "gas_calibration" "fail" "gas calibration tests failed"
  log "  ❌ FAIL"
fi

# ── Check 12: H-1 cold restart recovery tests ───────────────────────────

log "▸ Check 12: H-1 cold restart recovery tests..."
if cargo test -p nexus-test-utils --lib cold_restart_tests 2>/dev/null; then
  record "cold_restart_recovery" "pass"
  log "  ✅ PASS"
else
  record "cold_restart_recovery" "fail" "cold restart recovery tests failed"
  log "  ❌ FAIL"
fi

# ── Check 13: F-4/G-4 persistence tests ─────────────────────────────────

log "▸ Check 13: F-4/G-4 persistence integration tests..."
if cargo test -p nexus-test-utils --lib persistence_tests 2>/dev/null; then
  record "persistence_tests" "pass"
  log "  ✅ PASS"
else
  record "persistence_tests" "fail" "persistence tests failed"
  log "  ❌ FAIL"
fi

# ── Check 14: I-phase voo precision tests ────────────────────────────────

log "▸ Check 14: I-phase voo precision tests..."
if cargo test -p nexus-test-utils --lib precision_tests 2>/dev/null; then
  record "voo_precision_tests" "pass"
  log "  ✅ PASS"
else
  record "voo_precision_tests" "fail" "voo precision tests failed"
  log "  ❌ FAIL"
fi

# ── Check 15: J-phase stake-weighted quorum tests ────────────────────────

log "▸ Check 15: J-phase stake-weighted quorum tests..."
if cargo test -p nexus-test-utils --lib stake_weighted_cert_tests 2>/dev/null; then
  record "stake_weighted_quorum_tests" "pass"
  log "  ✅ PASS"
else
  record "stake_weighted_quorum_tests" "fail" "stake-weighted quorum tests failed"
  log "  ❌ FAIL"
fi

# ── Check 16: K-1 economic foundation E2E tests ─────────────────────────

log "▸ Check 16: K-1 economic foundation E2E tests..."
if cargo test -p nexus-test-utils --lib economic_foundation_tests 2>/dev/null; then
  record "economic_foundation_tests" "pass"
  log "  ✅ PASS"
else
  record "economic_foundation_tests" "fail" "economic foundation tests failed"
  log "  ❌ FAIL"
fi

# ── Check 17: Key CI artifacts exist ─────────────────────────────────────

log "▸ Check 17: Key artifacts exist..."
missing_files=()
required_files=(
  "Docs/Ops/Testnet_Release_Runbook.md"
  "Docs/Ops/Testnet_Operations_Guide.md"
  "Docs/Ops/Testnet_Access_Policy.md"
  "Docs/Ops/Schema_Migration_Guide.md"
  "Docs/Proof_Trust_Model.md"
  "scripts/smoke-test.sh"
  "scripts/contract-smoke-test.sh"
  "scripts/validate-startup.sh"
  "scripts/config-doc-drift-check.sh"
  ".github/workflows/ci.yml"
  "Dockerfile"
  "docker-compose.yml"
  "Makefile"
)

for f in "${required_files[@]}"; do
  if [[ ! -f "$REPO_ROOT/$f" ]]; then
    missing_files+=("$f")
  fi
done

if [[ ${#missing_files[@]} -eq 0 ]]; then
  record "artifacts_exist" "pass" "${#required_files[@]} files verified"
  log "  ✅ PASS: ${#required_files[@]} required files present"
else
  record "artifacts_exist" "fail" "missing: ${missing_files[*]}"
  log "  ❌ FAIL: missing ${missing_files[*]}"
fi

# ── Check 18: Dockerfile builds ──────────────────────────────────────

log "▸ Check 18: Dockerfile builds..."
if command -v docker &>/dev/null; then
  if docker build --quiet --target builder -t nexus-go-nogo-check . 2>/dev/null; then
    record "dockerfile_builds" "pass"
    log "  ✅ PASS"
    docker rmi nexus-go-nogo-check 2>/dev/null || true
  else
    record "dockerfile_builds" "fail" "docker build failed"
    log "  ❌ FAIL: docker build failed"
  fi
else
  record "dockerfile_builds" "skip" "docker not installed"
  log "  ⏭ SKIP: docker not available"
fi

# ── Check 19: L-phase exclusion proof tests ──────────────────────────────

log "▸ Check 19: L-phase exclusion proof tests..."
if cargo test -p nexus-test-utils --lib proof_tests 2>/dev/null; then
  record "exclusion_proof_tests" "pass"
  log "  ✅ PASS"
else
  record "exclusion_proof_tests" "fail" "exclusion proof tests failed"
  log "  ❌ FAIL"
fi

# ── Check 20: M-phase commitment recovery tests ─────────────────────────

log "▸ Check 20: M-phase commitment recovery tests..."
if cargo test -p nexus-test-utils --lib commitment_recovery_tests 2>/dev/null; then
  record "commitment_recovery_tests" "pass"
  log "  ✅ PASS"
else
  record "commitment_recovery_tests" "fail" "commitment recovery tests failed"
  log "  ❌ FAIL"
fi

# ── Check 21: N-phase canonical root unification tests ───────────────────

log "▸ Check 21: N-phase canonical root tests..."
if cargo test -p nexus-test-utils --lib canonical_root_tests 2>/dev/null; then
  record "canonical_root_tests" "pass"
  log "  ✅ PASS"
else
  record "canonical_root_tests" "fail" "canonical root tests failed"
  log "  ❌ FAIL"
fi

# ── Check 22: Backup tree & determinism regression ───────────────────────

log "▸ Check 22: Backup tree & determinism regression..."
backup_ok=true
determ_ok=true

if ! cargo test -p nexus-storage --lib backup_tree 2>/dev/null; then
  backup_ok=false
fi

if ! cargo test -p nexus-test-utils --lib determinism 2>/dev/null; then
  determ_ok=false
fi

if $backup_ok && $determ_ok; then
  record "backup_tree_determinism" "pass"
  log "  ✅ PASS"
else
  detail=""
  $backup_ok || detail="backup_tree failed"
  $determ_ok || detail="${detail:+$detail; }determinism failed"
  record "backup_tree_determinism" "fail" "$detail"
  log "  ❌ FAIL: $detail"
fi

# ── Check 23: Staking rotation pipeline tests ────────────────────────────

log "▸ Check 23: Staking rotation pipeline tests..."
if cargo test -p nexus-test-utils --lib staking_rotation_tests 2>/dev/null; then
  record "staking_rotation_tests" "pass"
  log "  ✅ PASS"
else
  record "staking_rotation_tests" "fail" "staking rotation tests failed"
  log "  ❌ FAIL"
fi

# ── Check 24: Cross-node election determinism tests ──────────────────────

log "▸ Check 24: Cross-node election determinism tests..."
if cargo test -p nexus-test-utils --lib cross_node_election_tests 2>/dev/null; then
  record "cross_node_election_tests" "pass"
  log "  ✅ PASS"
else
  record "cross_node_election_tests" "fail" "cross-node election determinism tests failed"
  log "  ❌ FAIL"
fi

# ── Check 25: Staking failure & release regression tests ─────────────────

log "▸ Check 25: Staking failure & regression tests..."
staking_fail_ok=true
staking_regr_ok=true

if ! cargo test -p nexus-test-utils --lib staking_failure_tests 2>/dev/null; then
  staking_fail_ok=false
fi

if ! cargo test -p nexus-test-utils --lib staking_regression_tests 2>/dev/null; then
  staking_regr_ok=false
fi

if $staking_fail_ok && $staking_regr_ok; then
  record "staking_failure_regression" "pass"
  log "  ✅ PASS"
else
  detail=""
  $staking_fail_ok || detail="staking failure tests failed"
  $staking_regr_ok || detail="${detail:+$detail; }staking regression tests failed"
  record "staking_failure_regression" "fail" "$detail"
  log "  ❌ FAIL: $detail"
fi

# ── Check 26: Multi-shard core tests ─────────────────────────────────────

log "▸ Check 26: Multi-shard core tests (W-phase)..."
if cargo test -p nexus-test-utils --lib multi_shard_tests 2>/dev/null; then
  record "multi_shard_tests" "pass"
  log "  ✅ PASS"
else
  record "multi_shard_tests" "fail" "multi shard core tests failed"
  log "  ❌ FAIL"
fi

# ── Check 27: HTLC lifecycle tests ──────────────────────────────────────

log "▸ Check 27: HTLC lifecycle tests (W-phase)..."
if cargo test -p nexus-test-utils --lib htlc_tests 2>/dev/null; then
  record "htlc_tests" "pass"
  log "  ✅ PASS"
else
  record "htlc_tests" "fail" "HTLC lifecycle tests failed"
  log "  ❌ FAIL"
fi

# ── Check 28: Cross-shard determinism tests ──────────────────────────────

log "▸ Check 28: Cross-shard determinism tests (X-3)..."
if cargo test -p nexus-test-utils --lib cross_shard_determinism_tests 2>/dev/null; then
  record "cross_shard_determinism" "pass"
  log "  ✅ PASS"
else
  record "cross_shard_determinism" "fail" "cross-shard determinism tests failed"
  log "  ❌ FAIL"
fi

# ── Check 29: Shard failure rollback tests ───────────────────────────────

log "▸ Check 29: Shard failure rollback tests (X-4)..."
if cargo test -p nexus-test-utils --lib shard_failure_tests 2>/dev/null; then
  record "shard_failure_tests" "pass"
  log "  ✅ PASS"
else
  record "shard_failure_tests" "fail" "shard failure rollback tests failed"
  log "  ❌ FAIL"
fi

# ── Check 30: Release regression tests ───────────────────────────────────

log "▸ Check 30: Release regression tests (X-5)..."
if cargo test -p nexus-test-utils --lib release_regression_tests 2>/dev/null; then
  record "release_regression" "pass"
  log "  ✅ PASS"
else
  record "release_regression" "fail" "release regression tests failed"
  log "  ❌ FAIL"
fi

# ── Verdict ──────────────────────────────────────────────────────────────

total_checks=$(( total_pass + total_fail + total_skip ))

if [[ $total_fail -eq 0 ]]; then
  verdict="GO"
  exit_code=0
else
  verdict="NO-GO"
  exit_code=1
fi

# ── JSON output ──────────────────────────────────────────────────────────

json_output="{\"verdict\":\"$verdict\",\"total\":$total_checks,\"pass\":$total_pass,\"fail\":$total_fail,\"skip\":$total_skip,\"checks\":["

for i in {1..${#CHECK_NAMES[@]}}; do
  [[ $i -gt 1 ]] && json_output+=","
  detail="${CHECK_DETAILS[$i]:-}"
  # Escape double quotes in detail (zsh-safe: avoid outer double-quote context).
  detail=${detail//\"/\\\"}
  json_output+="{\"name\":\"${CHECK_NAMES[$i]}\",\"result\":\"${CHECK_RESULTS[$i]}\",\"detail\":\"$detail\"}"
done

json_output+="]}"

if $JSON_ONLY; then
  echo "$json_output"
else
  log ""
  log "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  log "  VERDICT: $verdict"
  log "  Checks: $total_pass pass / $total_fail fail / $total_skip skip (${total_checks} total)"
  log "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  log ""
  log "JSON: $json_output"
fi

exit $exit_code
