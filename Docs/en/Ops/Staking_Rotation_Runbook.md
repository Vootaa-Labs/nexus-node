# Nexus Staking And Committee Rotation Runbook

> Version: `v0.1.13`
> Audience: node operators, release engineers, on-call engineers

## 0. Terms

| Term | Meaning |
| --- | --- |
| Staking snapshot | The committed-state view used as input to validator selection |
| Election | The deterministic process that derives the next committee |
| Rotation policy | Rules that decide when a committee change is attempted |
| Rotation outcome | The result of a rotation attempt, including normal and fallback paths |
| Persisted election result | The durable state needed for restart and continuity |

## 1. Architecture Overview

Rotation in the current baseline should be understood as a cross-cutting runtime
behavior involving committed state, snapshot derivation, election logic, and
durable recovery.

## 2. Configuration Parameters

Operators should review any configuration that affects:

- election interval
- minimum committee size assumptions
- minimum stake assumptions
- slash and exclusion behavior

## 3. Query Surfaces

### 3.1 Latest election result

Use the latest election-related endpoint or equivalent operator route to inspect
the most recent committee-selection result.

### 3.2 Current rotation policy

Validate that the running policy matches the intended rollout or testnet posture.

### 3.3 Staking snapshot view

If a staking snapshot is exposed or inspectable, verify that it reflects the
expected committed state at the relevant epoch boundary.

### 3.4 Consensus status

Always pair rotation validation with consensus status. A correct-looking election
surface is not enough if commit growth stops.

### 3.5 Epoch history

Rotation should be interpreted alongside epoch progression and history.

## 4. Normal Rotation Flow

### 4.1 Production path

In the healthy path, rotation should:

1. derive a snapshot from committed state
2. run deterministic election logic
3. persist the result
4. expose the same result across nodes

### 4.2 Validation checklist

After rotation, validate:

- readiness
- consensus progress
- validator-set consistency
- restart recovery of one representative node

## 5. Degradation And Fallback

### 5.1 Election failure fallback

If the election cannot safely produce a new committee, fallback should preserve
continuity instead of creating a broken or empty committee state.

### 5.2 Non-election epoch behavior

Not every epoch boundary must trigger a new election. Operators should be able
to distinguish healthy non-election behavior from silent failure.

## 6. Slash Impact On Rotation

### 6.1 Slash-to-rotation coupling

Slash can change candidate viability and therefore affect the next committee.

### 6.2 Validation after slash

After a slash event, recheck:

- election visibility
- validator-set consistency
- restart durability

## 7. Cold-Start Recovery

### 7.1 Recovery flow

On restart, the node should recover the active election and committee view from
durable state rather than reconstructing a conflicting in-memory guess.

### 7.2 Recovery validation

Cross-node agreement matters more than local success alone.

### 7.3 Known limits

Rotation documentation should not over-promise economic tuning or operational
policy maturity beyond what the current runtime actually supports.

## 8. Genesis Staking Initialization

Genesis-time staking data is part of the operator story because it influences
the initial committee and the first transition windows.

## 9. Monitoring Checklist

Track at least:

- current epoch
- latest election result
- readiness
- consensus progress
- validator-set consistency across nodes

## 10. Failure Triage Quick Reference

Typical buckets are:

- snapshot inconsistency
- election persistence failure
- cross-node disagreement
- post-rotation liveness degradation

## 11. Upgrade Notes

If a release touches staking, committee semantics, or slash behavior, review it
together with:

- `Docs/en/Ops/Epoch_Operations_Runbook.md`
- `Docs/en/Ops/Testnet_Release_Runbook.md`
- `Docs/en/Ops/Testnet_SLO.md`

## 12. Multi-Shard Notes

### 12.1 Shard relationship

Rotation is still a committee-level concern even when runtime execution becomes
multi-shard.

### 12.2 Multi-shard devnet operations

Validate shard-facing routes together with committee-facing routes when testing a
multi-shard release.

### 12.3 Caution points

Do not assume that successful shard startup alone proves committee continuity.
Rotation and epoch agreement still need explicit validation.
