# Nexus Proof Workspace

**Version**: 0.1.11  
**Purpose**: Centralized directory for formal verification artifacts.

**Corpus stats**: 18 files / 60 scenarios across 5 layers (consensus, execution, storage, agent, crypto).

## Quick-Start: Run Everything

```bash
# 1. Differential corpus — 19 harnesses covering all 18 corpus files (Rust)
cargo test -p nexus-test-utils --lib fv_diff_

# 2. Execution property tests — multi-shard, HTLC, cross-shard (Rust)
cargo test -p nexus-execution --test fv_proptest

# 3. Consensus property tests (Rust)
cargo test -p nexus-consensus --test fv_proptest

# 4. Intent / Agent Core property tests (Rust)
cargo test -p nexus-intent --test fv_proptest

# 5. Haskell commit sequence reference spec
runghc proofs/haskell/consensus/CommitSequence.hs

# 6. TLA+ session state machine (requires TLC or Apalache)
#    TLC:      tlc proofs/tla+/agent/FV-AG-002_session_forward.tla -config proofs/tla+/agent/AgentSession.cfg
#    Apalache: apalache-mc check --config=AgentSession.cfg proofs/tla+/agent/FV-AG-002_session_forward.tla

# 7. Move Prover staking invariants (requires move-prover)
#    move prove --path contracts/staking --named-addresses staking_addr=0xBEEF
```

## Directory Structure

```text
proofs/
├── agda/                          # Agda machine-checked proofs (reserved)
│   └── consensus/                 # Consensus safety & liveness (FV-CO-*)
├── haskell/                       # Haskell executable specifications
│   ├── consensus/                 # CommitSequence.hs — monotonicity spec ✅
│   └── execution/                 # Execution reference model (reserved)
├── tla+/                          # TLA+ temporal logic models
│   ├── agent/                     # AgentSession — forward-only FSM ✅
│   └── execution/                 # HTLC atomicity, Block-STM (reserved)
├── move-prover/                   # Move Prover specs
│   └── capabilities/              # staking_spec.move — lifecycle invariants ✅
├── differential/                  # Differential testing framework
│   ├── corpus/                    # 18 JSON corpus files, 60 scenarios ✅
│   └── reports/                   # Generated diff reports (FV_GENERATE_REPORTS=1)
└── property-tests/                # Rust property-based test harnesses
```

## Verification Asset Index

| Asset | Location | Status | Runner |
| --- | --- | --- | --- |
| Differential corpus runner | `tests/nexus-test-utils/src/fv_differential_runner.rs` | ✅ CI-integrated | `cargo test -p nexus-test-utils --lib fv_diff_` |
| Differential report gen | (same file, `FV_GENERATE_REPORTS=1`) | ✅ Complete | Set env var before test run |
| Execution property tests | `crates/nexus-execution/tests/fv_proptest.rs` | ✅ CI-integrated | `cargo test -p nexus-execution --test fv_proptest` |
| Consensus property tests | `crates/nexus-consensus/tests/fv_proptest.rs` | ✅ CI-integrated | `cargo test -p nexus-consensus --test fv_proptest` |
| Intent property tests | `crates/nexus-intent/tests/fv_proptest.rs` | ✅ CI-integrated | `cargo test -p nexus-intent --test fv_proptest` |
| TLA+ session FSM | `proofs/tla+/agent/FV-AG-002_session_forward.tla` | ✅ TLC-checkable | TLC / Apalache (see cfg) |
| Haskell commit sequence | `proofs/haskell/consensus/CommitSequence.hs` | ✅ Executable | `runghc CommitSequence.hs` |
| Move Prover staking specs | `proofs/move-prover/capabilities/staking_spec.move` | ✅ Spec complete | `move prove` (see above) |
| Differential corpus | `proofs/differential/corpus/` (18 JSON) | ✅ Complete | Consumed by runner |
| Report template | `proofs/differential/reports/TEMPLATE.md` | ✅ Template | Reference format |
| CI formal-verification job | `.github/workflows/ci.yml` | ✅ Gate | Runs on every PR |

## Naming Convention

### Proof Files

Pattern: `{invariant_id}_{short_name}.{ext}`

Examples:

- `agda/consensus/FV-CO-001_quorum_validity.agda`
- `tla+/agent/FV-AG-002_session_forward.tla`
- `haskell/consensus/FV-CO-004_commit_monotonic.hs`

### Corpus Files

Pattern: `{object_id}_{scenario}.json`

Examples:

- `differential/corpus/VO-CO-001_dag_causality.json`
- `differential/corpus/VO-EX-001_block_stm_determinism.json`

### Current Corpus Inventory (18 files / 60 scenarios)

| Layer | Files | Scenarios | Objects Covered |
| --- | --- | --- | --- |
| Consensus | 6 | 18 | VO-CO-001/002/003/006/007/008 |
| Execution | 4 | 13 | VO-EX-001/003/004/007 |
| Storage | 3 | 11 | VO-ST-001/004/007 |
| Agent | 4 | 15 | VO-AG-001/002/003/004 |
| Crypto | 1 | 3 | VO-CR-001 |
| **Total** | **18** | **60** | **17 VOs** |

### Report Files

Pattern: `{date}_{invariant_id}_diff_report.md`

Example:

- `differential/reports/2026-03-10_FV-EX-001_diff_report.md`

## Ownership

| Directory | Primary Tool | Responsibility |
| --- | --- | --- |
| agda/ | Agda compiler | Protocol-level safety/liveness proofs |
| haskell/ | GHC / runghc | Executable reference specifications |
| tla+/ | TLC / Apalache | Temporal logic & state machine models |
| move-prover/ | Move Prover | On-chain resource & permission constraints |
| differential/ | Rust test harness | Rust ↔ spec differential comparison |
| property-tests/ | proptest | Property-based Rust testing |

## Relationship to Source Code

- All proof artifacts reference invariants from `Solutions/21-Formal-Verification-Object-Register.md`
- Implementation anchors point into `crates/` source files
- Differential tests use JSON/BCS corpus from `differential/corpus/` (18 files, 60 scenarios)

## CI Integration

The `formal-verification` job in `.github/workflows/ci.yml` runs automatically on every PR:

1. **Differential corpus** — 19 harnesses across all 18 corpus files
2. **Consensus property tests** — fv_proptest + property_tests modules
3. **Intent property tests** — Agent Core invariants
4. **Execution property tests** — multi-shard, HTLC, cross-shard state root

TLA+ and Haskell checks require external tooling (TLC/Apalache, GHC) and
are run manually or in a dedicated verification environment.

## Evidence Export

Closed proof artifacts are exported to `Nexus_Docs/Audit_Reports/` as part of
phase gate review evidence. See `Solutions/22` §二 for evidence format.
