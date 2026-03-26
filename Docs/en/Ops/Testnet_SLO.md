# Nexus Testnet SLO, Error Budget, And Rollback Thresholds

> Version: `v0.1.13`

## 1. Overview

This document defines operator-facing service objectives for the public testnet
baseline. The goal is not to claim production-grade guarantees. The goal is to
give release owners and on-call engineers a quantitative decision frame.

## 2. SLO Definitions

### 2.1 Availability SLO

| SLI | How to measure | Target |
| --- | --- | --- |
| API readiness | ratio of successful `/ready` checks | at least 99.0% over 7 days |
| Consensus liveness | `total_commits` continues to increase | at least 98.0% over 7 days |
| Validator participation | ready validators divided by total validators | at least 5 of 7 in the default topology |

### 2.2 Latency SLO

| SLI | Scope | Target |
| --- | --- | --- |
| Read query P50 | common account and contract reads | <= 100 ms |
| Read query P99 | common account and contract reads | <= 1000 ms |
| Proof request P50 | state proof endpoints | <= 50 ms |
| Proof request P99 | state proof endpoints | <= 500 ms |

### 2.3 Correctness SLO

The testnet should be treated as out of objective if any of the following holds:

- consensus stops making forward progress
- proof endpoints start returning structurally invalid responses
- shard topology differs across healthy nodes
- validator or network endpoints disagree on the same time window without an explained rollout event

## 3. Error Budget

### 3.1 Interpretation

Use the budget to decide whether to continue, pause, or roll back a release.

- budget largely intact: continue release with standard monitoring
- budget rapidly burning: pause rollout and investigate
- budget exhausted on a core route: treat as a go-or-no-go blocker

### 3.2 Burn rules

The practical question is not whether a single probe fails, but whether a core
surface is burning budget fast enough that the system is no longer operating as
an acceptable public testnet.

### 3.3 Exhaustion handling

When the error budget is exhausted on a critical route, operators should stop
normal rollout and switch to incident posture.

## 4. Rollback Thresholds

### 4.1 Hard rollback triggers

Rollback should be considered when one or more of the following persist:

- `/ready` fails broadly across the cluster
- commit counters stall
- public read paths return sustained 5xx spikes
- proof or shard endpoints are unavailable
- restart recovery fails on multiple nodes

### 4.2 Soft rollback triggers

Some cases may justify pause-before-rollback rather than immediate rollback, for
example tail-latency regression or elevated but not catastrophic proof failures.

### 4.3 Rollback flow

Rollback flow should remain aligned with the release runbook rather than being
redefined ad hoc during an incident.

## 5. Monitoring And Alerting

### 5.1 Metrics guidance

Use at least:

- external probing for `/ready`
- consensus status polling
- metrics scraping on `/metrics`
- representative account, contract, and proof queries

### 5.2 Alerting guidance

Alerting should distinguish between:

- single-node anomalies
- cluster-wide readiness failure
- consensus stagnation
- proof and shard-route failure

### 5.3 Monitor integration note

Any operator UI or terminal monitor should reinforce the same SLO story rather
than presenting a disconnected health model.

## 6. Review Cadence

SLO targets should be reviewed whenever one of the following changes materially:

- public route exposure
- shard configuration assumptions
- proof or faucet posture
- release frequency or operator expectations

## 7. Version Evolution

The `v0.1.13` SLO surface should reflect the routes that actually exist in code
today: health, readiness, consensus, network, account, contract, shard, proof,
faucet, and related node APIs.

### 7.1 Multi-shard extension

As multi-shard behavior becomes more operationally significant, SLO discussion
should include shard-specific correctness and visibility checks in addition to
cluster-level liveness.

## Appendix: Quick Reference

Keep the release runbook, access policy, and SLO definitions aligned. A mismatch
between those documents creates operational ambiguity even when the node itself
is healthy.
