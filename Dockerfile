# Copyright (c) The Nexus-Node Contributors
# SPDX-License-Identifier: Apache-2.0
# ─────────────────────────────────────────────────────────────────────────
# Nexus node — production Dockerfile
#
# Multi-stage build:
#   1. Builder: compile all Nexus binaries in a full Rust image.
#   2. Runtime-prebuilt: CI fast-path — copies host-built binaries directly.
#   3. Runtime (default): minimal Debian image with builder-compiled binaries.
#
# Container directory contract:
#   /nexus/config/   — read-only: node.toml, genesis.json
#   /nexus/keys/     — read-only: validator key files (0600)
#   /nexus/data/     — read-write: RocksDB, chain state, identity markers
#
# Build:
#   docker build -t nexus-node .
#
# CI fast-path (skip double compilation):
#   cargo build --release -p nexus-node -p nexus-keygen -p nexus-genesis -p nexus-wallet
#   docker build --target runtime-prebuilt -t nexus-node .
#
# Run:
#   docker run -v ./devnet/validator-0/config:/nexus/config:ro \
#              -v ./devnet/validator-0/keys:/nexus/keys:ro \
#              -v ./devnet/validator-0/data:/nexus/data \
#              -p 8080:8080 -p 9090:9090 -p 7000:7000 \
#              nexus-node
# ─────────────────────────────────────────────────────────────────────────

# ── Stage 1: Builder ─────────────────────────────────────────────────────
FROM rust:1.85.0-bookworm AS builder

# Install build dependencies for pqcrypto-falcon (C FFI / CMake).
RUN apt-get update && apt-get install -y --no-install-recommends \
    cmake \
    clang \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Cache dependency compilation by copying manifests first.
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates/nexus-primitives/Cargo.toml crates/nexus-primitives/Cargo.toml
COPY crates/nexus-crypto/Cargo.toml     crates/nexus-crypto/Cargo.toml
COPY crates/nexus-config/Cargo.toml     crates/nexus-config/Cargo.toml
COPY crates/nexus-storage/Cargo.toml    crates/nexus-storage/Cargo.toml
COPY crates/nexus-network/Cargo.toml    crates/nexus-network/Cargo.toml
COPY crates/nexus-consensus/Cargo.toml  crates/nexus-consensus/Cargo.toml
COPY crates/nexus-execution/Cargo.toml  crates/nexus-execution/Cargo.toml
COPY crates/nexus-intent/Cargo.toml     crates/nexus-intent/Cargo.toml
COPY crates/nexus-rpc/Cargo.toml        crates/nexus-rpc/Cargo.toml
COPY crates/nexus-node/Cargo.toml       crates/nexus-node/Cargo.toml
COPY tools/nexus-keygen/Cargo.toml      tools/nexus-keygen/Cargo.toml
COPY tools/nexus-genesis/Cargo.toml     tools/nexus-genesis/Cargo.toml
COPY tools/nexus-wallet/Cargo.toml      tools/nexus-wallet/Cargo.toml
COPY tools/nexus-bench/Cargo.toml       tools/nexus-bench/Cargo.toml
COPY tests/nexus-test-utils/Cargo.toml  tests/nexus-test-utils/Cargo.toml

# Create stub lib.rs for each crate so cargo can resolve the dependency graph.
RUN find crates tools tests -name Cargo.toml -exec sh -c \
    'dir=$(dirname "$1"); mkdir -p "$dir/src" && echo "" > "$dir/src/lib.rs"' _ {} \;

# Create stub main.rs where needed.
RUN mkdir -p crates/nexus-node/src && echo "fn main() {}" > crates/nexus-node/src/main.rs \
    && mkdir -p tools/nexus-keygen/src && echo "fn main() {}" > tools/nexus-keygen/src/main.rs \
    && mkdir -p tools/nexus-genesis/src && echo "fn main() {}" > tools/nexus-genesis/src/main.rs \
    && mkdir -p tools/nexus-wallet/src && echo "fn main() {}" > tools/nexus-wallet/src/main.rs \
    && mkdir -p tools/nexus-bench/src && echo "fn main() {}" > tools/nexus-bench/src/main.rs

# Copy Cargo config.
COPY .cargo/             .cargo/

# Pre-build dependencies (cached layer).
RUN cargo build --release --bin nexus-node 2>/dev/null || true

# Copy full source and build for real.
COPY crates/   crates/
COPY tools/    tools/
COPY tests/    tests/
COPY contracts/ contracts/
COPY scripts/  scripts/

# Touch all source files to invalidate cargo's cache for actual source.
RUN find crates tools tests -name "*.rs" -exec touch {} +

RUN CARGO_BUILD_JOBS=4 cargo build --release \
    --bin nexus-node \
    --bin nexus-keygen \
    --bin nexus-genesis \
    --bin nexus-wallet

# ── Stage 2: Runtime (pre-built, CI fast-path) ──────────────────────────
# Usage: docker build --target runtime-prebuilt -t nexus-node .
# Requires host-compiled Linux binaries in target/release/ (same arch).
# Eliminates double compilation in CI by reusing cargo build output.
FROM debian:bookworm-slim AS runtime-prebuilt

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

RUN groupadd --gid 1000 nexus && \
    useradd --uid 1000 --gid nexus --shell /bin/false --create-home nexus

RUN mkdir -p /nexus/config /nexus/keys /nexus/data && \
    chown -R nexus:nexus /nexus

COPY target/release/nexus-node    /usr/local/bin/nexus-node
COPY target/release/nexus-keygen  /usr/local/bin/nexus-keygen
COPY target/release/nexus-genesis /usr/local/bin/nexus-genesis
COPY target/release/nexus-wallet  /usr/local/bin/nexus-wallet

USER nexus:nexus
WORKDIR /nexus

VOLUME ["/nexus/config", "/nexus/keys", "/nexus/data"]
EXPOSE 8080 9090 7000

HEALTHCHECK --interval=10s --timeout=3s --start-period=30s --retries=3 \
    CMD curl -fsS http://127.0.0.1:8080/ready || exit 1

ENTRYPOINT ["nexus-node"]
CMD ["/nexus/config/node.toml"]

# ── Stage 3: Runtime (default) ───────────────────────────────────────────
# This is the LAST stage, so plain `docker build .` uses it by default.
FROM debian:bookworm-slim AS runtime

# Install minimal runtime dependencies.
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user.
RUN groupadd --gid 1000 nexus && \
    useradd --uid 1000 --gid nexus --shell /bin/false --create-home nexus

# Create container directory layout.
RUN mkdir -p /nexus/config /nexus/keys /nexus/data && \
    chown -R nexus:nexus /nexus

# Copy binaries from builder.
COPY --from=builder /build/target/release/nexus-node    /usr/local/bin/nexus-node
COPY --from=builder /build/target/release/nexus-keygen  /usr/local/bin/nexus-keygen
COPY --from=builder /build/target/release/nexus-genesis /usr/local/bin/nexus-genesis
COPY --from=builder /build/target/release/nexus-wallet  /usr/local/bin/nexus-wallet

# Switch to non-root user.
USER nexus:nexus
WORKDIR /nexus

# Volume mount points.
VOLUME ["/nexus/config", "/nexus/keys", "/nexus/data"]

# Expose ports: REST API, gRPC, P2P.
EXPOSE 8080 9090 7000

# Health check — uses the readiness endpoint exposed by the REST server.
HEALTHCHECK --interval=10s --timeout=3s --start-period=30s --retries=3 \
    CMD curl -fsS http://127.0.0.1:8080/ready || exit 1

# Default entry point — load config from standard location.
ENTRYPOINT ["nexus-node"]
CMD ["/nexus/config/node.toml"]
