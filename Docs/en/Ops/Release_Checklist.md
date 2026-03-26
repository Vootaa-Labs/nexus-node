# Nexus Release Checklist

> Version: `v0.1.13`

## Usage

Copy this checklist for the target release and mark each item complete before
declaring GO.

## Phase 1: Code Preparation

- [ ] target PRs merged
- [ ] release identifier and image tag frozen
- [ ] workspace and toolchain assumptions still align with Rust `1.85.0`
- [ ] release branch or tag chosen and communicated

## Phase 2: CI And Evidence Gates

- [ ] lint passes
- [ ] security checks pass
- [ ] temporary cargo-audit exceptions reviewed against `Docs/Report/BACKLOG.md`
- [ ] workspace tests pass
- [ ] smoke and contract rehearsal pass
- [ ] required recovery and persistence checks pass
- [ ] required docs consistency checks pass

## Phase 3: Documentation Consistency

- [ ] `Docs/en/Ops/Testnet_Operations_Guide.md` reviewed
- [ ] `Docs/en/Ops/Testnet_Release_Runbook.md` reviewed
- [ ] `Docs/en/Ops/Testnet_Access_Policy.md` reviewed
- [ ] `Docs/en/Ops/Testnet_SLO.md` reviewed
- [ ] `Docs/en/Ops/Schema_Migration_Guide.md` reviewed
- [ ] `Docs/en/Ops/Staking_Rotation_Runbook.md` reviewed
- [ ] `Docs/en/Ops/Epoch_Operations_Runbook.md` reviewed
- [ ] `Docs/en/Report/Proof_Trust_Model.md` reviewed
- [ ] bilingual pairing and README path checks pass

## Phase 4: Staging Deployment Validation

- [ ] image builds successfully
- [ ] staging environment deploys successfully
- [ ] readiness validation passes
- [ ] smoke tests pass on staging
- [ ] representative proof, shard, and validator checks pass

## Phase 5: Go / No-Go Review

- [ ] release owner signs off
- [ ] on-call owner signs off
- [ ] rollback plan is confirmed in advance
- [ ] no unresolved blocker remains in core operator-facing paths

## Phase 6: Release Execution

- [ ] artifacts pushed to target registry or host
- [ ] rollout sequence executed as planned
- [ ] readiness restored on all required nodes
- [ ] consensus progress verified after rollout

## Phase 7: Post-Release Monitoring

- [ ] readiness monitoring active
- [ ] consensus monitoring active
- [ ] proof, shard, and faucet monitoring reviewed if relevant
- [ ] error budget burn checked against SLO posture

## Phase 8: Exception Rollback

- [ ] rollback trigger conditions are clearly identified
- [ ] previous known-good image or artifact is ready
- [ ] logs and evidence are preserved before rollback
- [ ] post-rollback validation plan is ready

## Sign-Off

- [ ] release owner
- [ ] operations owner
- [ ] on-call owner
