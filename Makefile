# nexus-node development workflow
# Usage: make <target>

.PHONY: all check fmt clippy test test-vm test-all lint audit deny build release clean setup help

# Default: full lint + test
all: lint test

# ---------------------------------------------------------------------------
# Setup
# ---------------------------------------------------------------------------

## Install required cargo tools for local development
setup:
	@echo "=== Installing development tools ==="
	cargo install cargo-audit cargo-deny cargo-nextest cargo-llvm-cov \
		cargo-machete critcmp cargo-criterion --locked 2>/dev/null || \
	cargo binstall cargo-audit cargo-deny cargo-nextest cargo-llvm-cov \
		cargo-machete critcmp cargo-criterion --no-confirm
	@echo "=== Tools installed ==="

# ---------------------------------------------------------------------------
# Build & Check
# ---------------------------------------------------------------------------

## Workspace type-check (fast)
check:
	cargo check --workspace --all-targets

## Full release build
build:
	cargo build --workspace --release

# ---------------------------------------------------------------------------
# Formatting & Linting
# ---------------------------------------------------------------------------

## Format all code
fmt:
	cargo fmt --all

## Check formatting without modifying files
fmt-check:
	cargo fmt --all -- --check

## Run clippy with deny warnings
clippy:
	cargo clippy --workspace --all-targets -- -D warnings

## Check for unused dependencies (advisory — stub crates may report false positives)
machete:
	cargo machete --with-metadata || echo "  [warn] machete found potential unused deps (check for false positives in stub crates)"

## Full lint gate: fmt-check + clippy
lint: fmt-check clippy

# ---------------------------------------------------------------------------
# Testing
# ---------------------------------------------------------------------------

## Run workspace tests (standard cargo test)
## Uses default-members to skip vendored move crate test suites.
test:
	cargo test

## Run Move VM integration tests (nexus-execution with move-vm feature)
test-vm:
	cargo test -p nexus-execution --features move-vm

## Run workspace tests via nextest (parallel, CI profile)
nextest:
	cargo nextest run --profile ci

## Run doc-tests
doctest:
	cargo test --doc

## Run ignored tests (crypto KAT vectors)
test-kat:
	cargo test -- --ignored --test-threads=1

## All tests: nextest + doc-tests + KAT
test-all: nextest doctest test-kat

# ---------------------------------------------------------------------------
# Coverage
# ---------------------------------------------------------------------------

## Generate LCOV coverage report
coverage:
	cargo llvm-cov --workspace --lcov --output-path lcov.info
	@echo "Coverage report: lcov.info"

## Generate HTML coverage report
coverage-html:
	cargo llvm-cov --workspace --html
	@echo "Coverage report: target/llvm-cov/html/index.html"

# ---------------------------------------------------------------------------
# Security
# ---------------------------------------------------------------------------

## Run cargo-audit (CVE check)
## --ignore flags: temporary waivers for libp2p 0.54.x TLS chain (see Docs/Report/BACKLOG.md)
audit:
	cargo audit \
		--ignore RUSTSEC-2025-0009 \
		--ignore RUSTSEC-2026-0049 \
		--ignore RUSTSEC-2026-0009

## Run cargo-deny (licenses + bans + sources)
## Advisory check is handled by cargo-audit above; cargo-deny 0.18.x
## cannot parse CVSS 4.0 entries in the advisory DB.
deny:
	cargo deny check licenses bans sources

## Full security gate: audit + deny
security: audit deny

# ---------------------------------------------------------------------------
# Benchmarks
# ---------------------------------------------------------------------------

## Run all benchmarks
bench:
	cargo bench --workspace

# ---------------------------------------------------------------------------
# Release
# ---------------------------------------------------------------------------

## Build optimized release binary
release:
	cargo build --workspace --release

# ---------------------------------------------------------------------------
# Cleanup
# ---------------------------------------------------------------------------

## Remove build artifacts
clean:
	cargo clean

# ---------------------------------------------------------------------------
# Devnet (local Docker validator network)
# ---------------------------------------------------------------------------
.PHONY: devnet-build devnet-setup devnet-up devnet-down devnet-smoke devnet-clean devnet devnet-bench devnet-cold-bench

NEXUS_IMAGE ?= nexus-node
NEXUS_NUM_VALIDATORS ?= 7
NEXUS_NUM_SHARDS ?= 2

## Build container image (reuses cached base images, does not force pull)
devnet-build:
	@echo "=== Building $(NEXUS_IMAGE) container image ==="
	DOCKER_BUILDKIT=1 docker build -t $(NEXUS_IMAGE) .

## Generate devnet keys, genesis, per-node configs, and docker-compose.yml
devnet-setup:
	@echo "=== Bootstrapping devnet layout ($(NEXUS_NUM_VALIDATORS) validators, $(NEXUS_NUM_SHARDS) shards) ==="
	./scripts/setup-devnet.sh -n $(NEXUS_NUM_VALIDATORS) -s $(NEXUS_NUM_SHARDS) -o devnet-n7s -f

## Start devnet (builds image + setup if needed)
devnet-up: devnet-build devnet-setup
	@echo "=== Starting devnet ==="
	NEXUS_IMAGE=$(NEXUS_IMAGE) docker compose up -d

## Stop devnet containers
devnet-down:
	docker compose down

## Run all smoke tests against running devnet
devnet-smoke:
	NEXUS_NUM_VALIDATORS=$(NEXUS_NUM_VALIDATORS) NEXUS_NUM_SHARDS=$(NEXUS_NUM_SHARDS) ./scripts/smoke-test.sh
	./scripts/contract-smoke-test.sh

## Full devnet lifecycle: build → setup → up → smoke → down
devnet: devnet-up devnet-smoke

## Remove devnet state and containers
devnet-clean: devnet-down
	rm -rf devnet-n7s/

## Run multi-node devnet TPS and latency benchmark sweep
devnet-bench:
	cargo run -p nexus-bench --bin devnet_bench --release -- \
		--num-shards $(NEXUS_NUM_SHARDS) \
		--nodes http://127.0.0.1:8080,http://127.0.0.1:8081,http://127.0.0.1:8082,http://127.0.0.1:8083,http://127.0.0.1:8084,http://127.0.0.1:8085,http://127.0.0.1:8086

## Rebuild image, cold-start devnet, rerun lifecycle benchmark, capture fresh metrics
devnet-cold-bench:
	NEXUS_NUM_VALIDATORS=$(NEXUS_NUM_VALIDATORS) NEXUS_NUM_SHARDS=$(NEXUS_NUM_SHARDS) \
	./scripts/devnet-cold-bench.sh \
		-n $(NEXUS_NUM_VALIDATORS) \
		-s $(NEXUS_NUM_SHARDS) \
		-o devnet-n7s \
		-i $(NEXUS_IMAGE)

# ---------------------------------------------------------------------------
# Pre-commit: run everything CI would check
# ---------------------------------------------------------------------------

## Full pre-commit check (mirrors CI gates 1-3)
pre-commit: lint security test test-vm
	@echo "=== All pre-commit checks passed ==="

# ---------------------------------------------------------------------------
# Help
# ---------------------------------------------------------------------------

## Show available targets
help:
	@echo "nexus-node Development Commands:"
	@echo ""
	@echo "  make setup        Install required cargo tools"
	@echo "  make check        Fast workspace type-check"
	@echo "  make build        Full workspace build"
	@echo "  make fmt          Format all code"
	@echo "  make fmt-check    Check formatting (no changes)"
	@echo "  make clippy       Run clippy with -D warnings"
	@echo "  make machete      Check unused dependencies"
	@echo "  make lint         Full lint gate (fmt + clippy + machete)"
	@echo "  make test         Run workspace tests"
	@echo "  make test-vm      Run Move VM integration tests"
	@echo "  make nextest      Run tests via nextest (parallel)"
	@echo "  make doctest      Run doc-tests only"
	@echo "  make test-kat     Run crypto KAT vectors (ignored tests)"
	@echo "  make test-all     All tests (nextest + doctest + KAT)"
	@echo "  make coverage     Generate LCOV coverage report"
	@echo "  make coverage-html  Generate HTML coverage report"
	@echo "  make audit        CVE audit (cargo-audit)"
	@echo "  make deny         License/ban check (cargo-deny)"
	@echo "  make security     Full security gate (audit + deny)"
	@echo "  make bench        Run benchmarks"
	@echo "  make release      Build release binaries"
	@echo "  make clean        Remove build artifacts"
	@echo "  make pre-commit   Full pre-commit check (mirrors CI)"
	@echo ""
	@echo "Devnet Commands:"
	@echo ""
	@echo "  make devnet-build   Build node container (no remote pull)"
	@echo "  make devnet-setup   Generate devnet keys and configs"
	@echo "  make devnet-up      Build + setup + start devnet"
	@echo "  make devnet-down    Stop devnet containers"
	@echo "  make devnet-smoke   Run smoke tests against running devnet"
	@echo "  make devnet         Full lifecycle (up + smoke)"
	@echo "  make devnet-clean   Stop and remove devnet state"
	@echo "  make devnet-bench   Run multi-node devnet TPS/latency sweep"
	@echo "  make devnet-cold-bench  Rebuild image, cold-start devnet, full benchmark"
	@echo ""
	@echo "  make help         Show this help"
