# Nexus Capacity Calibration Reference

> Version: `v0.1.13`
> Audience: operators, release engineers, RPC owners

## 1. Gas Budget Calibration Results

### 1.1 Test method

Capacity discussion for the current baseline should be grounded in the routes and
traffic classes that actually exist in the repository. In practice that means
testing and calibrating against:

- account and contract queries
- proof and commitment requests
- intent-facing submission and estimation paths
- MCP-exposed query or tool-style flows

The repository already contains middleware tests, smoke scripts, and release
checks that together form the engineering evidence for these defaults.

### 1.2 Current conclusion

For the `v0.1.13` baseline, the documented operational posture remains
deliberately conservative:

- a global per-IP admission limit
- separate quota classes for query, intent, and MCP traffic
- a bounded read-only query gas budget
- a bounded read-only query timeout

The goal is not to maximize public throughput by default. The goal is to keep
testnet behavior predictable under mixed traffic and under abuse pressure.

### 1.3 Timeout calibration

Timeout must be interpreted together with gas budget rather than as an isolated
number. A useful default should:

- allow normal query paths to complete comfortably
- terminate pathological or adversarial requests quickly
- remain simple enough for external integrators to reason about

## 2. Quota Matrix

### 2.1 Current default classes

The current access policy separates traffic into at least three quota classes:

- query
- intent
- MCP

Each class is independently limited so that exhaustion of one class does not
silently consume the others.

### 2.2 Global limit

The global per-IP limit remains the broadest protection layer. It is intended to
absorb:

- anonymous burst traffic
- enumeration attempts
- accidental client retry storms

Higher-level class quotas then refine behavior for more expensive paths.

### 2.3 Tier hierarchy invariants

The documented hierarchy should remain monotonic:

1. anonymous <= authenticated <= whitelisted
2. expensive paths stay tighter than lightweight metadata paths
3. faucet and proof routes never inherit permissive defaults by accident

### 2.4 Cross-class independence

Cross-class independence matters because query-heavy clients, intent clients,
and MCP clients have different traffic shapes. A single shared bucket would hide
failure modes and create noisy-neighbor behavior.

### 2.5 Fail-closed behavior

When quota accounting reaches an unsafe state, the safer operator posture is to
fail closed rather than allow uncontrolled admission. This is especially true on
public surfaces where traffic identity is only partially trusted.

## 3. Faucet Quotas

The faucet is not a general-purpose distribution channel. It is a bounded
developer bootstrap service. Its limits should be reviewed together with:

- per-address hourly caps
- address-tracking capacity
- public release posture
- abuse policy and temporary block rules

## 4. Configuration Synchronization Checklist

When quota or gas-related defaults change, review all of the following together:

- `crates/nexus-config/src/rpc.rs`
- `Docs/en/Ops/Testnet_Access_Policy.md`
- `Docs/en/Ops/Testnet_SLO.md`
- `Docs/en/Ops/Testnet_Release_Runbook.md`
- `scripts/config-doc-drift-check.sh`

If only one of these moves, the operator surface becomes ambiguous.

## 5. Multi-Shard And HTLC Calibration

### 5.1 Multi-shard gas consumption

Multi-shard execution changes the cost profile of query, observation, and
cross-shard coordination paths. Even when the public route is read-only, the
runtime may need to traverse more state or more topology metadata than the
single-shard case.

### 5.2 Capacity impact

Operators should expect at least three capacity differences once multi-shard
paths matter operationally:

- more topology-sensitive reads
- more proof and shard-head observation
- more care around cross-shard settlement visibility

### 5.3 HTLC timeout configuration

HTLC-related timing and quota choices should not be treated as purely economic
settings. They also affect operational observability, retry behavior, and how
quickly operators can distinguish healthy delay from real failure.

## 6. Calibration Recommendations For Public Testnet

1. keep defaults conservative until measured evidence says otherwise
2. calibrate on representative traffic, not only isolated unit tests
3. treat quota changes as release-scoped changes that require doc review
4. validate single-node and multi-node behavior separately
5. include proof and MCP-facing flows in the same capacity story as contract query

## 7. Related Automation

The repository already contains direct or indirect automation for this area:

- middleware tests for quota behavior
- CI gates for config and docs consistency
- smoke and release scripts that validate operator-facing routes

That means capacity documentation should be maintained as an engineering input,
not as an after-the-fact narrative.
