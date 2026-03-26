# Coverage Report v0.1.13

## Scope

This report records the current Rust test coverage baseline for the Nexus `v0.1.13` workspace.

- Scope includes first-party workspace packages only.
- Vendored sources under `vendor-src/` are excluded from both LCOV and HTML coverage output.
- Generated files under `target/` are excluded from report rendering.
- Coverage was collected from the repository root on 2026-03-24.

## Commands

From the repository root:

```bash
make coverage
make coverage-html
make coverage-docs
```

`make coverage-docs` runs the coverage test pass once, exports LCOV and HTML artifacts without rerunning tests, then refreshes this report from `target/llvm-cov/coverage-summary.json`.

## Measured Results

The following totals were generated from `cargo llvm-cov --json --summary-only` using the same first-party package allowlist and filename exclusion rule:

| Metric | Covered | Total | Percent |
| --- | ---: | ---: | ---: |
| Lines | 38,872 | 51,229 | 75.88% |
| Functions | 3,538 | 4,751 | 74.47% |
| Regions | 12,763 | 19,332 | 66.02% |
| Instantiations | 4,529 | 7,890 | 57.40% |

Additional scope checks for this run:

- Reported source files: 216
- `vendor-src` files present in summary: 0

## Artifacts

- LCOV output: `lcov.info`
- HTML output: `target/llvm-cov/html/index.html`
- Machine-readable summary used for this report: `target/llvm-cov/coverage-summary.json`

## Notes

- This is a point-in-time baseline, not a permanent quality gate.
- If the first-party package list changes, update the Makefile allowlist first and then rerun `make coverage-docs`.
- CI coverage calls `make coverage-docs` and retains the refreshed bilingual coverage reports as a workflow artifact.
