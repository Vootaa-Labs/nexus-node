# Coverage Report v0.1.15

## Scope

This report records the current Rust test coverage baseline for the Nexus `v0.1.15` workspace.

- Scope includes first-party workspace packages only.
- Vendored sources under `vendor-src/` are excluded from both LCOV and HTML coverage output.
- Generated files under `target/` are excluded from report rendering.
- Coverage was collected from the repository root on 2026-03-31.

## Commands

From the repository root:

```bash
make coverage
make coverage-html
make coverage-json
make coverage-scorecard
make coverage-docs
```

`make coverage-docs` executes the coverage sampling pass once, exports LCOV, HTML, and JSON artifacts without rerunning tests, then refreshes this report and the crate-level scorecard.

## Measured Results

| Metric | Covered | Total | Percent |
| --- | ---: | ---: | ---: |
| Lines | 47,867 | 54,477 | 87.87% |
| Functions | 4,379 | 5,315 | 82.39% |
| Regions | 15,495 | 20,254 | 76.50% |
| Instantiations | 5,446 | 8,883 | 61.31% |

Additional scope checks for this run:

- Reported source files: 197
- `vendor-src` files present in summary: 0
- Package scorecard rows: 15

## Package Scorecard

| Package | Priority | Target | Lines | Gap | Functions | Regions | Files | Status |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| nexus-consensus | P0 | >= 90% | 96.99% | 0.00% | 91.53% | 89.27% | 9 | meets-target |
| nexus-crypto | P0 | >= 95% | 98.17% | 0.00% | 99.29% | 94.24% | 5 | meets-target |
| nexus-execution | P0 | >= 85% | 94.18% | 0.00% | 92.46% | 87.13% | 29 | meets-target |
| nexus-intent | P1 | >= 85% | 94.20% | 0.00% | 83.94% | 85.30% | 30 | meets-target |
| nexus-network | P1 | >= 75% | 81.73% | 0.00% | 82.03% | 72.70% | 15 | meets-target |
| nexus-storage | P1 | >= 85% | 92.61% | 0.00% | 79.16% | 83.13% | 11 | meets-target |
| nexus-config | P2 | >= 80% | 97.96% | 0.00% | 97.35% | 93.44% | 8 | meets-target |
| nexus-node | P2 | >= 70% | 79.58% | 0.00% | 78.13% | 65.03% | 29 | meets-target |
| nexus-primitives | P2 | >= 80% | 93.27% | 0.00% | 93.18% | 83.41% | 4 | meets-target |
| nexus-rpc | P2 | >= 80% | 90.96% | 0.00% | 79.83% | 80.62% | 33 | meets-target |
| nexus-bench | Support | tracked | 0.00% | n/a | 0.00% | 0.00% | 1 | tracked |
| nexus-genesis | Support | tracked | 90.60% | n/a | 63.04% | 79.56% | 1 | tracked |
| nexus-keygen | Support | tracked | 91.39% | n/a | 65.38% | 72.04% | 1 | tracked |
| nexus-test-utils | Support | tracked | 100.00% | n/a | 100.00% | 100.00% | 0 | tracked |
| nexus-wallet | Support | tracked | 74.55% | n/a | 75.49% | 59.99% | 21 | tracked |

## Top 10 Coverage Hotspots

| Package | Priority | File | Lines | Gap | Uncovered Lines |
| --- | --- | --- | ---: | ---: | ---: |
| nexus-crypto | P0 | crates/nexus-crypto/src/mlkem.rs | 97.35% | 0.00% | 7 |
| nexus-crypto | P0 | crates/nexus-crypto/src/falcon.rs | 97.99% | 0.00% | 6 |
| nexus-execution | P0 | crates/nexus-execution/src/block_stm/executor.rs | 88.14% | 0.00% | 179 |
| nexus-consensus | P0 | crates/nexus-consensus/src/dag.rs | 93.23% | 0.00% | 22 |
| nexus-crypto | P0 | crates/nexus-crypto/src/mldsa.rs | 98.31% | 0.00% | 4 |
| nexus-execution | P0 | crates/nexus-execution/src/move_adapter/move_runtime.rs | 89.40% | 0.00% | 85 |
| nexus-crypto | P0 | crates/nexus-crypto/src/csprng.rs | 100.00% | 0.00% | 0 |
| nexus-crypto | P0 | crates/nexus-crypto/src/hasher.rs | 100.00% | 0.00% | 0 |
| nexus-execution | P0 | crates/nexus-execution/src/move_adapter/builtin_vm.rs | 90.04% | 0.00% | 25 |
| nexus-consensus | P0 | crates/nexus-consensus/src/engine.rs | 95.32% | 0.00% | 26 |

## Artifacts

- LCOV output: `lcov.info`
- HTML output: `target/llvm-cov/html/index.html`
- Machine-readable summary used for this report: `target/llvm-cov/coverage-summary.json`
- Crate-level scorecard: `target/llvm-cov/package-scorecard.md`

## Notes

- This is a point-in-time baseline, not a permanent quality gate.
- Core crate targets in the scorecard reflect the v0.1.15 coverage governance plan.
- CI coverage now calls `make coverage-docs`, publishes a package summary in the GitHub Actions step summary, and uploads the refreshed report and scorecard as workflow artifacts.
