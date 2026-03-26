# Nexus Remediation Backlog

This file tracks temporary exceptions and deferred remediation items that are
intentionally kept out of the current green path.

## Open Security Exceptions

### 2026-03-24: Temporary cargo-audit waivers for libp2p 0.54.x TLS chain

Status: open, temporary exception

Scope:
- `RUSTSEC-2025-0009` on `ring 0.16.20`
- `RUSTSEC-2026-0049` on `rustls-webpki 0.101.7`
- `RUSTSEC-2026-0009` on `time 0.3.36`

Why this is temporarily ignored:
- The remaining `cargo audit` failures are all pulled in through the same
  upstream dependency chain: `libp2p-tls 0.5.0 -> rcgen 0.11.3` in the current
  `libp2p 0.54.x` line.
- `rustls-webpki 0.103.9` was already upgraded to `0.103.10`, but the older
  `libp2p-tls` path still locks `ring 0.16.20`, `rustls-webpki 0.101.7`, and
  `time 0.3.36`.
- Direct `cargo update` attempts confirmed these three advisories cannot be
  cleared without moving the libp2p TLS dependency chain forward.

Required follow-up:
- Revisit this exception when a compatible `libp2p` / `libp2p-tls` / `rcgen`
  upgrade path is available for the workspace.
- Remove the three `cargo audit --ignore` entries from `Makefile` as soon as
  the upstream chain can be upgraded without breaking the network stack.
- Re-run `make audit`, `make security`, and the full network-facing test set
  after the dependency upgrade.

Suggested trigger points:
- Any planned upgrade of `libp2p` beyond `0.54.x`
- Any network transport refactor touching QUIC or TLS
- Any release preparation where dependency refresh is already in scope

Exit criteria:
- `cargo audit` passes with no ignore flags for these three advisories
- This backlog entry is removed or marked resolved with the fixing versions

---

### 2026-03-26: devnet_bench binary ported (RESOLVED)

Status: resolved

Resolution:
- `devnet_bench.rs` ported from v0.1.12_Pre to current nexus-bench crate,
  updated for v0.1.13 REST API surface (`/v2` prefix, no lifecycle endpoint).
- `make devnet-bench` verified end-to-end on 7-node Docker devnet.
- Benchmark produces JSON (`target/devnet-bench/devnet_benchmark_results.json`)
  and EN/ZH markdown reports under `Docs/{en,zh}/Report/Benchmark/`.