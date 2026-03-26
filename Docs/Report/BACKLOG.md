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

### 2026-03-25: devnet_bench binary not yet ported

Status: open, deferred

Scope:
- `make devnet-bench` and `make devnet-cold-bench` fail because the
  `devnet_bench` binary (a `[[bin]]` target in `nexus-bench`) has not been
  ported from the v0.1.12 development tree.

Why this is not yet resolved:
- The binary was present in `Nexus_Devnet_0.1.12_Pre` at
  `tools/nexus-bench/src/bin/devnet_bench.rs` but was not carried forward
  during the v0.1.13 construction phases because it depends on HTTP-based
  transaction submission and confirmation polling that changed structurally
  between versions.

Required follow-up:
- Port `devnet_bench.rs` from `0.1.12_Pre` to the current nexus-bench crate,
  updating RPC calls to the v0.1.13 REST API surface.
- Verify `make devnet-bench` and `make devnet-cold-bench` complete end-to-end.

Exit criteria:
- `make devnet-cold-bench` produces benchmark JSON and English/Chinese reports
- This backlog entry is removed