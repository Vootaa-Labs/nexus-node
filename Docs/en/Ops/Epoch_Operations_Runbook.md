# Nexus Epoch Operations Runbook

> Version: `v0.1.13`
> Audience: node operators, release engineers, on-call engineers

## 0. Terms

| Term | Meaning |
| --- | --- |
| Epoch | A period during which committee composition and related consensus parameters are stable |
| Committee | The validator set active for the current epoch |
| Epoch transition | The durable move from one epoch to the next |
| Epoch store | The state required to recover epoch and committee information after restart |
| Rotation boundary | The point where committee election or rollover may take effect |

## 1. Normal Epoch Transition Flow

### 1.1 Time-driven transition

In the healthy path, epoch progression should:

1. detect a transition boundary
2. derive or load the next committee view
3. persist transition metadata
4. expose the new epoch consistently across nodes

### 1.2 Operator-driven transition

If epoch-related operations are triggered through an administrative surface, the
same invariants still matter:

- the transition must be durable
- the committee view must converge across nodes
- readiness and consensus progress must remain observable

### 1.3 How to validate success

After a transition, validate at least:

- `/ready`
- `/v2/consensus/status`
- validator-related endpoints on multiple nodes
- restart recovery of one representative node if the release window allows it

## 2. Governance Operations Around Epoch State

### 2.1 Slash-related handling

Slash and committee-affecting governance events should be treated as
epoch-sensitive changes. The operator concern is not only whether the event is
accepted, but whether the durable epoch view remains coherent after restart.

### 2.2 Persistence best practice

If an operation changes committee-relevant state, verify durable recovery rather
than assuming in-memory behavior is enough.

### 2.3 History inspection

Operators should keep a minimal record of:

- previous epoch number
- new epoch number
- trigger type
- observed validator set
- any rollback or recovery action taken

## 3. Restart And Recovery

### 3.1 Single-node restart

The baseline expectation is that a restarted node recovers epoch and committee
state from durable storage and rejoins a coherent cluster view.

### 3.2 Multi-node restart

Simultaneous restart of several nodes raises the bar. Validate:

- matching epoch values
- matching validator and committee surfaces
- resumed commit growth

### 3.3 Full network cold restart

For a cold restart or planned maintenance window, treat epoch recovery as a
primary acceptance criterion, not a secondary observation.

## 4. Common Failure Modes And Rollback Guidance

### 4.1 Epoch mismatch across nodes

Symptoms:

- different nodes report different epoch numbers
- committee or validator data differs without an ongoing rollout

Response:

1. stop further rollout activity
2. capture endpoint snapshots from several nodes
3. compare logs and durable state assumptions

### 4.2 Consensus stalls after transition

If a transition completes but commits stop advancing, treat the issue as both a
consensus and an epoch-coordination incident.

### 4.3 Post-slash state regression on restart

If governance-related state appears to roll back after restart, prioritize the
durable store and recovery path before changing live policy again.

### 4.4 Epoch-store corruption

This should be handled as a durability and recovery incident. Preserve evidence,
avoid ad hoc manual edits, and follow the schema and release runbooks.

### 4.5 Snapshot-based recovery

Where snapshot or backup recovery is used, confirm that the restored epoch state
matches the intended committee and transition window.

## 5. Monitoring Checklist

Track at least:

- readiness state
- commit advancement
- validator-set consistency
- restart recovery success
- disagreement windows between nodes

## 6. Operational Log Template

For each epoch-related event, record:

- date and time window
- triggering cause
- old and new epoch
- nodes checked
- validation endpoints used
- whether rollback or restart was required

## 7. Epoch Handling During Upgrades

During a release or protocol change, the safe sequence is:

1. stabilize deployment inputs
2. monitor readiness and consensus before the transition window
3. validate immediately after the transition
4. only then continue broader rollout or declare GO

## 8. Cold-Restart Recovery Scope

### 8.1 What recovery should cover

At minimum, recovery should preserve:

- current epoch number
- effective committee view
- transition-related durable metadata

### 8.2 Single-node cold restart flow

Use one restarted node as the canary for epoch recovery behavior before claiming
cluster-wide safety.

### 8.3 Validation after cold restart

Validate both local health and cross-node agreement.

### 8.4 Retention and cleanup

Retention policy should not destroy the evidence required to reconstruct the
active epoch window during release debugging.

### 8.5 Failure handling

If cold-restart recovery fails, treat that as a release blocker for any change
that touches epoch, committee, persistence, or migration semantics.

### 8.6 CI evidence

The repository already contains recovery-oriented checks. Operators should treat
those as part of the evidence chain, not as a substitute for release-window
validation.

## Appendix A: Staking-Driven Committee Rotation

Epoch operations and staking rotation are coupled. For detailed rotation flows,
see `Docs/en/Ops/Staking_Rotation_Runbook.md`.

## Appendix B: Key RPC Surfaces

Useful endpoints for epoch validation include consensus, validator, readiness,
and related operational routes exposed by the current REST surface.
