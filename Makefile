# Copyright (c) The Nexus-Node Contributors
# SPDX-License-Identifier: Apache-2.0

# nexus-node development workflow
# Usage: make <target>

.PHONY: all check fmt clippy test test-vm test-all lint audit deny build release clean setup help
.PHONY: verify-test-fixtures compile-test-fixtures pre-push-fast pre-push

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
## Scoped to nexus-crypto to match CI (crypto-kat job).
## The workspace-wide --ignored includes scenario_tests which require a running devnet.
test-kat:
	cargo test -p nexus-crypto --lib --tests -- --ignored --test-threads=1

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
.PHONY: devnet-contracts devnet-full devnet-logs devnet-status devnet-tail devnet-json ci-local-smoke

NEXUS_IMAGE ?= nexus-node
NEXUS_NUM_VALIDATORS ?= 7
NEXUS_NUM_SHARDS ?= 1

## Build container image (reuses cached base images, does not force pull)
devnet-build:
	@echo "=== Building $(NEXUS_IMAGE) container image ==="
	DOCKER_BUILDKIT=1 docker build -t $(NEXUS_IMAGE) .

## Generate devnet keys, genesis, per-node configs, and docker-compose-n7s.yml
devnet-setup:
	@echo "=== Bootstrapping devnet layout ($(NEXUS_NUM_VALIDATORS) validators, $(NEXUS_NUM_SHARDS) shards) ==="
	./scripts/setup-devnet.sh -n $(NEXUS_NUM_VALIDATORS) -s $(NEXUS_NUM_SHARDS) -o devnet-n7s -f
	NEXUS_NUM_SHARDS=$(NEXUS_NUM_SHARDS) ./scripts/generate-compose.sh \
		-o docker-compose-n7s.yml \
		-d devnet-n7s \
		-n $(NEXUS_NUM_VALIDATORS)
	@echo "✓ devnet-n7s/ + docker-compose-n7s.yml generated ($(NEXUS_NUM_VALIDATORS) validators, $(NEXUS_NUM_SHARDS) shards)"

## Start devnet (builds image + setup if needed)
devnet-up: devnet-build devnet-setup
	@echo "=== Starting devnet ==="
	NEXUS_IMAGE=$(NEXUS_IMAGE) docker compose -f docker-compose-n7s.yml up -d

## Stop devnet containers
devnet-down:
	docker compose -f docker-compose-n7s.yml down

## Run smoke tests against running devnet (health checks only)
devnet-smoke:
	NEXUS_NUM_VALIDATORS=$(NEXUS_NUM_VALIDATORS) NEXUS_NUM_SHARDS=$(NEXUS_NUM_SHARDS) ./scripts/smoke-test.sh

## Run contract smoke tests against running devnet (deploy/call/query)
devnet-contracts:
	./scripts/contract-smoke-test.sh

## Full devnet lifecycle: build → setup → up → smoke → contracts → down
devnet-full: devnet-up devnet-smoke devnet-contracts devnet-down

## Full devnet lifecycle (legacy alias): build → setup → up → smoke
devnet: devnet-up devnet-smoke

## Remove all devnet state, containers, and compose files
devnet-clean:
	-docker compose -f docker-compose-n7s.yml down --remove-orphans 2>/dev/null
	-docker compose -f docker-compose-mvs-ci.yml down --remove-orphans 2>/dev/null
	-docker compose -f docker-compose-smoke-ci.yml down --remove-orphans 2>/dev/null
	rm -rf devnet-n7s devnet-mvs-ci devnet-smoke-ci
	rm -f docker-compose-n7s.yml docker-compose-mvs-ci.yml docker-compose-smoke-ci.yml
	@echo "✓ devnet artifacts cleaned"

## Follow devnet container logs
devnet-logs:
	docker compose -f docker-compose-n7s.yml logs --follow --tail=50

## Check health of all devnet validators
devnet-status:
	@echo "Checking node health..."
	@for i in $$(seq 0 $$(($(NEXUS_NUM_VALIDATORS)-1))); do \
		PORT=$$((8080 + $$i)); \
		STATUS=$$(curl -sf "http://127.0.0.1:$$PORT/v2/health" | jq -r '.status' 2>/dev/null || echo "UNREACHABLE"); \
		printf "  validator-$$i (:$$PORT): $$STATUS\n"; \
	done

## Tail logs for a specific validator (usage: make devnet-tail N=0)
N ?= 0
devnet-tail:
	docker compose -f docker-compose-n7s.yml logs nexus-node-$(N) --follow --tail=100

## Print genesis.json from current devnet setup
devnet-json:
	@cat devnet-n7s/genesis.json 2>/dev/null || echo "Run 'make devnet-setup' first"

## Local equivalent of ci.yml move-vm-smoke job (isolated context)
ci-local-smoke: devnet-build
	@echo "==> ci-local-smoke: generating devnet (devnet-mvs-ci/)..."
	@./scripts/setup-devnet.sh -o devnet-mvs-ci -n $(NEXUS_NUM_VALIDATORS) -s $(NEXUS_NUM_SHARDS) -f
	@NEXUS_NUM_SHARDS=$(NEXUS_NUM_SHARDS) ./scripts/generate-compose.sh -o docker-compose-mvs-ci.yml -d devnet-mvs-ci -n $(NEXUS_NUM_VALIDATORS)
	@echo "==> ci-local-smoke: starting containers..."
	@docker compose -f docker-compose-mvs-ci.yml up -d
	@echo "==> ci-local-smoke: waiting for readiness..."
	@sleep 10
	@echo "==> ci-local-smoke: running smoke test..."
	@NEXUS_NUM_SHARDS=$(NEXUS_NUM_SHARDS) ./scripts/smoke-test.sh; \
		RESULT=$$?; \
		docker compose -f docker-compose-mvs-ci.yml down --remove-orphans; \
		rm -rf devnet-mvs-ci docker-compose-mvs-ci.yml; \
		exit $$RESULT

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
# Test Fixture Management
# ---------------------------------------------------------------------------

# All git-tracked bytecodes that MUST be compiled with dev-address.
# Format: path:expected_hex_address_suffix (last N non-zero hex chars of the 32-byte address)
FIXTURE_MVS := \
	contracts/examples/counter/nexus-artifact/bytecode/counter.mv:cafe \
	contracts/examples/token/nexus-artifact/bytecode/token.mv:cafe \
	contracts/examples/escrow/nexus-artifact/bytecode/escrow.mv:cafe \
	contracts/examples/voting/nexus-artifact/bytecode/voting.mv:cafe \
	contracts/examples/registry/nexus-artifact/bytecode/registry.mv:cafe \
	contracts/examples/multisig/nexus-artifact/bytecode/multisig.mv:cafe \
	contracts/staking/nexus-artifact/bytecode/staking.mv:beef

## Verify that all git-tracked .mv fixtures contain their expected dev-address
## (i.e. they were NOT overwritten by a smoke-test build with a real deployer address)
verify-test-fixtures:
	@echo "=== Verifying test fixture bytecodes contain expected dev-address ==="
	@fail=0; \
	for entry in $(FIXTURE_MVS); do \
		mv_path=$${entry%%:*}; \
		addr_suffix=$${entry##*:}; \
		full=0000000000000000000000000000000000000000000000000000000000000000$${addr_suffix}; \
		pattern=$$(echo "$$full" | grep -oE '.{64}$$'); \
		if [ ! -f "$$mv_path" ]; then \
			echo "  MISS  $$mv_path (file not found)"; \
			fail=1; \
		elif xxd -p "$$mv_path" | tr -d '\n' | grep -q "$$pattern"; then \
			echo "  OK    $$mv_path (0x$$addr_suffix)"; \
		else \
			echo "  FAIL  $$mv_path (expected 0x$$addr_suffix — fixture polluted?)"; \
			fail=1; \
		fi; \
	done; \
	if [ "$$fail" -ne 0 ]; then \
		echo ""; \
		echo "FAILED: One or more fixtures are missing or polluted."; \
		echo "Run 'make compile-test-fixtures' to rebuild with dev-address."; \
		exit 1; \
	fi
	@echo "=== All fixtures verified ==="

## Recompile all contract fixtures with dev-address (0xCAFE) into git-tracked paths.
## Requires nexus-wallet to be built first.
compile-test-fixtures: build
	@echo "=== Compiling test fixtures (dev-address) ==="
	@WALLET=$$(cargo metadata --format-version 1 --no-deps 2>/dev/null \
		| python3 -c "import sys,json; print(json.load(sys.stdin)['target_directory'])" 2>/dev/null)/release/nexus-wallet; \
	if [ ! -x "$$WALLET" ]; then \
		WALLET=target/release/nexus-wallet; \
	fi; \
	if [ ! -x "$$WALLET" ]; then \
		echo "ERROR: nexus-wallet not found. Run 'make build' first."; \
		exit 1; \
	fi; \
	for pkg_dir in contracts/examples/counter contracts/examples/token \
		contracts/examples/escrow contracts/examples/voting \
		contracts/examples/registry contracts/examples/multisig \
		contracts/staking; do \
		echo "  BUILD $$pkg_dir"; \
		"$$WALLET" move build --package-dir "$$pkg_dir" --skip-fetch || exit 1; \
	done
	@echo "=== Done — verify with 'make verify-test-fixtures' ==="

# ---------------------------------------------------------------------------
# Pre-commit / Pre-push gates
# ---------------------------------------------------------------------------

## Full pre-commit check (mirrors CI gates 1-3)
pre-commit: lint security verify-test-fixtures test test-vm
	@echo "=== All pre-commit checks passed ==="

## Fast pre-push gate: fmt + clippy + fixture integrity + unit tests
pre-push-fast: fmt-check clippy verify-test-fixtures test
	@echo "=== Pre-push (fast) passed ==="

## Full pre-push gate: everything CI will check + fixture safety + KAT
pre-push: lint security verify-test-fixtures test test-vm test-kat
	@echo "=== Pre-push (full) passed ==="

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
	@echo "  make pre-push-fast  Fast pre-push: fmt + clippy + fixtures + test"
	@echo "  make pre-push     Full pre-push: lint + security + fixtures + test + VM"
	@echo "  make verify-test-fixtures  Check .mv fixtures have dev-address 0xCAFE"
	@echo "  make compile-test-fixtures  Recompile all .mv fixtures with dev-address"
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
