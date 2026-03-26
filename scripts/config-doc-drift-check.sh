#!/usr/bin/env zsh
# ─────────────────────────────────────────────────────────────────────────
# config-doc-drift-check.sh  (v0.1.5 — E-1)
#
# Compares RpcConfig::default() values in code with the documented
# defaults in Docs/Ops/Testnet_Access_Policy.md.
#
# Usage:
#   scripts/config-doc-drift-check.sh
#
# Exit codes:
#   0 — no drift detected
#   1 — drift detected (details printed to stderr)
# ─────────────────────────────────────────────────────────────────────────
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
RPC_CONFIG="$REPO_ROOT/crates/nexus-config/src/rpc.rs"
ACCESS_POLICY="$REPO_ROOT/Docs/Ops/Testnet_Access_Policy.md"

if [[ ! -f "$RPC_CONFIG" ]]; then
  echo "ERROR: RPC config file not found: $RPC_CONFIG" >&2
  exit 1
fi

if [[ ! -f "$ACCESS_POLICY" ]]; then
  echo "ERROR: Access policy file not found: $ACCESS_POLICY" >&2
  exit 1
fi

# ── Extract defaults from RpcConfig::default() in Rust ───────────────────

typeset -A CODE_DEFAULTS

in_default=false
while IFS= read -r line; do
  if [[ "$line" =~ "fn default" ]]; then
    in_default=true
    continue
  fi
  if $in_default && [[ "$line" =~ '^[[:space:]]*\}$' ]]; then
    in_default=false
    continue
  fi
  if $in_default; then
    # Match field: value, (numeric with underscores)
    if [[ "$line" =~ '^ *([a-z_]+): *([0-9_]+)' ]]; then
      field="${match[1]}"
      value="${${match[2]}//_/}"
      CODE_DEFAULTS[$field]="$value"
    fi
  fi
done < "$RPC_CONFIG"

# ── Extract defaults from Testnet_Access_Policy.md ───────────────────────

typeset -A DOC_DEFAULTS

while IFS= read -r line; do
  if [[ "$line" =~ '^\| *`([a-z_]+)`' ]]; then
    field="${match[1]}"
    default_raw=$(echo "$line" | awk -F'|' '{print $4}' | xargs)

    # Normalise
    default_norm="${default_raw// /}"
    default_norm="${default_norm//,/}"

    # Handle "10¹⁸" → 1000000000000000000
    if [[ "$default_norm" == "10¹⁸" ]]; then
      default_norm="1000000000000000000"
    fi

    # Skip non-numeric values
    if [[ "$default_norm" =~ '^[0-9]+$' ]]; then
      DOC_DEFAULTS[$field]="$default_norm"
    fi
  fi
done < "$ACCESS_POLICY"

# ── Compare ──────────────────────────────────────────────────────────────

drift_count=0

CHECKED_FIELDS=(
  rate_limit_per_ip_rps
  query_rate_limit_anonymous_rpm
  query_rate_limit_authenticated_rpm
  query_rate_limit_whitelisted_rpm
  intent_rate_limit_anonymous_rpm
  intent_rate_limit_authenticated_rpm
  intent_rate_limit_whitelisted_rpm
  mcp_rate_limit_anonymous_rpm
  mcp_rate_limit_authenticated_rpm
  mcp_rate_limit_whitelisted_rpm
  query_gas_budget
  query_timeout_ms
  faucet_amount
  faucet_per_addr_limit_per_hour
)

for field in "${CHECKED_FIELDS[@]}"; do
  code_val="${CODE_DEFAULTS[$field]:-MISSING}"
  doc_val="${DOC_DEFAULTS[$field]:-MISSING}"

  if [[ "$code_val" == "MISSING" ]]; then
    echo "DRIFT: $field — not found in code defaults" >&2
    (( drift_count++ )) || true
    continue
  fi

  if [[ "$doc_val" == "MISSING" ]]; then
    echo "DRIFT: $field — not found in Testnet_Access_Policy.md" >&2
    (( drift_count++ )) || true
    continue
  fi

  if [[ "$code_val" != "$doc_val" ]]; then
    echo "DRIFT: $field — code=$code_val  doc=$doc_val" >&2
    (( drift_count++ )) || true
  fi
done

if [[ "$drift_count" -eq 0 ]]; then
  echo "OK: No config-doc drift detected (${#CHECKED_FIELDS[@]} fields checked)."
  exit 0
else
  echo "" >&2
  echo "FAILED: $drift_count field(s) drifted between code and documentation." >&2
  exit 1
fi
