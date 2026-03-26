# Nexus Database Schema Migration Guide

> Version: `v0.1.13`
> Audience: operators, release engineers, storage maintainers

## 1. Overview

Nexus uses RocksDB for durable node storage. Schema-sensitive changes should be
treated as release-critical because they affect restart continuity, rollback
safety, and what operators can reasonably recover after failure.

## 2. Column Family Layout

### 2.1 Current family set

The current baseline uses multiple column families to separate:

- blocks and transactions
- receipts and state
- certificates and batches
- session and provenance data
- commitment metadata, leaves, and nodes
- HTLC-related state

The exact storage implementation lives under `crates/nexus-storage/src/rocks/`.

### 2.2 Commitment-tree persistence

State commitment is now part of the real persistent model. That means schema
changes touching commitment metadata or commitment tree structure are not minor
documentation updates; they are compatibility events.

## 3. Migration History

### v1 -> v2

This stage moved more protocol state out of memory-only assumptions and into a
durable store boundary.

### v2 -> v3

This stage completed the persistent commitment story more fully.

### Transparent extensions

Later additions such as staking-related durable state and `cf_htlc_locks`
should still be discussed explicitly in migration planning even when the schema
version number does not change.

## 4. Cold Upgrade Procedure

### 4.1 Pre-checks

Before a schema-sensitive rollout:

- verify backups or snapshots
- verify the exact compatibility boundary
- verify the target image and target config as a matched pair

### 4.2 Upgrade steps

The safe operator posture is:

1. stop uncontrolled writes or rollout activity
2. preserve the current data state
3. deploy the new binary and config together
4. let migration logic run in a controlled restart window

### 4.3 Automatic migration behavior

Automatic migration is useful only if it is deterministic, observable, and easy
to validate afterward. Operators should never assume that an automatic path is
self-proving.

### 4.4 Post-upgrade validation

Validate:

- node readiness
- commit progress
- persistent state continuity
- proof and query route behavior
- restart behavior after the first successful migration boot

## 5. Hot-Upgrade Strategy

The current baseline should not promise true zero-downtime hot upgrade for any
schema-sensitive change unless that path has been explicitly validated. For most
storage compatibility changes, a cold or controlled restart posture remains the
safer claim.

## 6. Rollback Strategy

### 6.1 Safe rollback conditions

Rollback is safer when the migration is backward compatible or when a verified
backup or snapshot exists.

### 6.2 Rollback steps

1. stop further rollout
2. restore the previous known-good binary and compatible data state
3. revalidate readiness, consensus, and operator-facing routes

### 6.3 Rollback without backup

Without a safe backup boundary, operators must be explicit that rollback may
require full environment regeneration rather than true state continuity.

## 7. Developer Guidance For New Migrations

When adding a new migration:

- document the compatibility boundary
- document whether rollback is supported
- update release and schema guides together
- add or update recovery validation if the change affects restart behavior

## Appendix: Related Files

- `crates/nexus-storage/src/rocks/mod.rs`
- `crates/nexus-storage/src/rocks/schema.rs`
- `Docs/en/Ops/Testnet_Release_Runbook.md`
- `Docs/en/Ops/Testnet_Operations_Guide.md`
