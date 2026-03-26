# Nexus Formal Verification Guide

## 1. Audience

This guide is for two groups:

- engineers who need a practical entry into the verification assets in this repository
- reviewers who want to understand what evidence already exists in the `v0.1.13` baseline

The point is not to teach each formal method in full. The point is to answer:

1. what assets exist today
2. which assets are runnable now
3. which assets are skeletons or forward-looking placeholders
4. what to run first during the first week on the codebase

## 2. Verification Asset Map

The repository includes several layers of evidence under `proofs/`, crate-level tests, and shared test utilities.

### 2.1 Assets that are already part of engineering evidence

- property tests for consensus under `crates/nexus-consensus/tests/`
- property tests for execution under `crates/nexus-execution/tests/`
- property tests for intent and Agent Core under `crates/nexus-intent/tests/`
- differential and shared verification helpers under `tests/nexus-test-utils/`
- operational smoke and soak scripts under `scripts/`

### 2.2 Assets that require external tools

- TLA+ models under `proofs/tla+/`
- Move Prover assets under `proofs/move-prover/`
- differential reference material under `proofs/differential/`

### 2.3 Assets that should be treated as roadmap or scaffolding

- proof directories that exist but are not wired into a default developer flow
- deeper theorem-proving tracks under `proofs/agda/` and related areas

## 3. What The Current Baseline Proves Best

The `v0.1.13` baseline provides the strongest practical evidence in these areas:

- consensus safety and ordering invariants through crate tests
- execution determinism and multi-shard scenarios through execution tests
- intent and agent-session invariants through property tests
- end-to-end devnet behavior through smoke and contract rehearsal scripts

It is more accurate to describe the repository as having a layered verification surface than as having a single formal-verification pipeline.

## 4. Recommended First Week Path

### Day 1

- read `README.md`
- run `./scripts/smoke-test.sh`
- run `./scripts/contract-smoke-test.sh`

### Day 2

- inspect `tests/nexus-test-utils/`
- run one consensus and one execution property test target

### Day 3

- inspect `proofs/tla+/` and `proofs/move-prover/`
- map which artifacts are runnable in your environment and which are retained reference assets

## 5. Practical Commands

Representative commands for the current repo:

```bash
cargo test -p nexus-consensus --test fv_proptest
cargo test -p nexus-execution --test fv_proptest
cargo test -p nexus-intent --test fv_proptest
```

If you are validating the local developer path first, run:

```bash
./scripts/smoke-test.sh
./scripts/contract-smoke-test.sh
```

## 6. Interpretation Guidance

- Treat property tests and smoke scripts as current executable evidence.
- Treat proof directories as mixed: some are runnable now, some are reference material.
- Do not claim more than the repository actually wires into CI or local flows.

## 7. Current Position At v0.1.13

At `v0.1.13`, Nexus already has meaningful verification depth, but it should not be described as a finished theorem-proving program. The accurate claim is that the codebase combines:

- runnable property and differential testing
- operational evidence from reproducible scripts
- retained formal-method assets for deeper assurance work
