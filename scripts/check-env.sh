#!/usr/bin/env bash
# Copyright (c) The Nexus-Node Contributors
# SPDX-License-Identifier: Apache-2.0
# Nexus local development environment verification.
# Checks toolchain, components, and cargo tools without modifying anything.
# Exit code 0 = all checks pass, 1 = something missing.

set -euo pipefail

REQUIRED_RUST="1.85.0"
REQUIRED_TOOLS=(cargo-audit cargo-deny cargo-nextest cargo-llvm-cov cargo-machete critcmp cargo-criterion)
PASS=0
FAIL=0

check() {
    local label="$1"
    local ok="$2"
    if [ "$ok" = "true" ]; then
        echo "  [PASS] $label"
        PASS=$((PASS + 1))
    else
        echo "  [FAIL] $label"
        FAIL=$((FAIL + 1))
    fi
}

echo "=== Nexus Dev Environment Check ==="
echo ""

# 1. Rust version
if command -v rustc &>/dev/null; then
    RUST_VER=$(rustc --version 2>/dev/null || echo "")
    if [[ "$RUST_VER" == *"$REQUIRED_RUST"* ]]; then
        check "Rust $REQUIRED_RUST" "true"
    else
        check "Rust $REQUIRED_RUST (found: $RUST_VER)" "false"
    fi
else
    check "Rust installed" "false"
fi

# 2. rustfmt
if rustfmt --version &>/dev/null; then
    check "rustfmt" "true"
else
    check "rustfmt" "false"
fi

# 3. clippy
if cargo clippy --version &>/dev/null; then
    check "clippy" "true"
else
    check "clippy" "false"
fi

# 4. Required cargo tools
for tool in "${REQUIRED_TOOLS[@]}"; do
    subcmd=$(echo "$tool" | sed 's/^cargo-//')
    if command -v "$tool" &>/dev/null || cargo help "$subcmd" &>/dev/null 2>&1; then
        check "$tool" "true"
    else
        check "$tool" "false"
    fi
done

# 5. rust-toolchain.toml
SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
if [ -f "$SCRIPT_DIR/rust-toolchain.toml" ]; then
    check "rust-toolchain.toml present" "true"
else
    check "rust-toolchain.toml present" "false"
fi

# 6. deny.toml
if [ -f "$SCRIPT_DIR/deny.toml" ]; then
    check "deny.toml present" "true"
else
    check "deny.toml present" "false"
fi

# 7. Quick compile check
echo ""
echo "  Running: cargo check --workspace ..."
if cargo check --workspace --quiet 2>/dev/null; then
    check "cargo check --workspace" "true"
else
    check "cargo check --workspace" "false"
fi

echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="

if [ "$FAIL" -gt 0 ]; then
    echo ""
    echo "Run the setup script to fix missing tools:"
    echo "  cd ../Nexus_Docs && ./setup-nexus-dev-env.sh"
    exit 1
fi

echo "Environment is ready for Nexus development."
