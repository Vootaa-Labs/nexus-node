//! Epoch persistence layer.
//!
//! Stores epoch metadata, committee snapshots, and transition history
//! in the existing `cf_state` column family. All keys use the
//! `__epoch_` prefix to avoid collisions with other state data.
//!
//! # Key layout
//!
//! | Key | Value (BCS) |
//! |-----|-------------|
//! | `__epoch_current__` | `EpochNumber` |
//! | `__epoch_started_at__` | `TimestampMs` |
//! | `__epoch_committee__:{epoch_be8}` | `PersistentCommittee` |
//! | `__epoch_transition__:{epoch_be8}` | `EpochTransition` |

#![forbid(unsafe_code)]

use anyhow::Context;
use nexus_consensus::types::{EpochTransition, PersistentCommittee};
use nexus_consensus::Committee;
use nexus_primitives::{EpochNumber, TimestampMs};
use nexus_storage::{StateStorage, WriteBatchOps};

// ── Key constants ────────────────────────────────────────────────────────────

const KEY_CURRENT_EPOCH: &[u8] = b"__epoch_current__";
const KEY_EPOCH_STARTED_AT: &[u8] = b"__epoch_started_at__";
const PREFIX_COMMITTEE: &[u8] = b"__epoch_committee__:";
const PREFIX_TRANSITION: &[u8] = b"__epoch_transition__:";
const PREFIX_ELECTION: &[u8] = b"__epoch_election__:";

fn committee_key(epoch: EpochNumber) -> Vec<u8> {
    let mut key = PREFIX_COMMITTEE.to_vec();
    key.extend_from_slice(&epoch.0.to_be_bytes());
    key
}

fn transition_key(from_epoch: EpochNumber) -> Vec<u8> {
    let mut key = PREFIX_TRANSITION.to_vec();
    key.extend_from_slice(&from_epoch.0.to_be_bytes());
    key
}

fn election_key(for_epoch: EpochNumber) -> Vec<u8> {
    let mut key = PREFIX_ELECTION.to_vec();
    key.extend_from_slice(&for_epoch.0.to_be_bytes());
    key
}

/// Public accessor for the election key — used by backends for
/// direct epoch-specific election result lookups.
pub fn election_key_for(epoch: EpochNumber) -> Vec<u8> {
    election_key(epoch)
}

/// Column family used for all epoch metadata.
const CF: &str = "cf_state";

// ── Persisted epoch state ────────────────────────────────────────────────────

/// Snapshot of epoch state loaded from storage on cold restart.
#[derive(Debug)]
pub struct PersistedEpochState {
    /// The current epoch number.
    pub epoch: EpochNumber,
    /// Wall-clock time when the current epoch started.
    pub epoch_started_at: TimestampMs,
    /// Committee for the current epoch.
    pub committee: Committee,
    /// Full transition history (may be empty on first boot).
    pub transitions: Vec<EpochTransition>,
    /// Election result for the current epoch (if the committee was
    /// derived from an election rather than genesis or carry-forward).
    pub election_result: Option<crate::staking_snapshot::PersistedElectionResult>,
}

// ── Write operations ─────────────────────────────────────────────────────────

/// Persist the initial epoch state during genesis boot.
///
/// Writes the current epoch number, the committee snapshot, and the
/// epoch start time in a single atomic write batch.
pub fn persist_initial_epoch<S: StateStorage>(
    store: &S,
    committee: &Committee,
) -> anyhow::Result<()> {
    let epoch = committee.epoch();
    let now = TimestampMs::now();
    let snap = committee.to_persistent();

    let mut batch = store.new_batch();
    batch.put_cf(
        CF,
        KEY_CURRENT_EPOCH.to_vec(),
        bcs::to_bytes(&epoch).context("BCS encode epoch")?,
    );
    batch.put_cf(
        CF,
        KEY_EPOCH_STARTED_AT.to_vec(),
        bcs::to_bytes(&now).context("BCS encode epoch_started_at")?,
    );
    batch.put_cf(
        CF,
        committee_key(epoch),
        bcs::to_bytes(&snap).context("BCS encode committee")?,
    );

    futures::executor::block_on(store.write_batch(batch))
        .map_err(|e| anyhow::anyhow!("epoch store write failed: {e}"))?;

    Ok(())
}

/// Persist an epoch transition atomically.
///
/// Writes the new committee, the transition record, and updates the
/// current epoch pointer in a single write batch.  If an election
/// result is provided, it is also persisted.
pub fn persist_epoch_transition<S: StateStorage>(
    store: &S,
    new_committee: &Committee,
    transition: &EpochTransition,
) -> anyhow::Result<()> {
    persist_epoch_transition_with_election(store, new_committee, transition, None)
}

/// Persist an epoch transition atomically, including an optional
/// election result record.
pub fn persist_epoch_transition_with_election<S: StateStorage>(
    store: &S,
    new_committee: &Committee,
    transition: &EpochTransition,
    election: Option<&crate::staking_snapshot::PersistedElectionResult>,
) -> anyhow::Result<()> {
    let new_epoch = transition.to_epoch;
    let snap = new_committee.to_persistent();

    let mut batch = store.new_batch();
    batch.put_cf(
        CF,
        KEY_CURRENT_EPOCH.to_vec(),
        bcs::to_bytes(&new_epoch).context("BCS encode new epoch")?,
    );
    batch.put_cf(
        CF,
        KEY_EPOCH_STARTED_AT.to_vec(),
        bcs::to_bytes(&transition.transitioned_at).context("BCS encode epoch_started_at")?,
    );
    batch.put_cf(
        CF,
        committee_key(new_epoch),
        bcs::to_bytes(&snap).context("BCS encode new committee")?,
    );
    batch.put_cf(
        CF,
        transition_key(transition.from_epoch),
        bcs::to_bytes(transition).context("BCS encode transition")?,
    );

    if let Some(er) = election {
        batch.put_cf(
            CF,
            election_key(new_epoch),
            bcs::to_bytes(er).context("BCS encode election result")?,
        );
    }

    futures::executor::block_on(store.write_batch(batch))
        .map_err(|e| anyhow::anyhow!("epoch transition write failed: {e}"))?;

    Ok(())
}

// ── Read operations ──────────────────────────────────────────────────────────

/// Load epoch state from storage.
///
/// Returns `None` if no epoch data has been persisted yet (first boot).
pub fn load_epoch_state<S: StateStorage>(store: &S) -> anyhow::Result<Option<PersistedEpochState>> {
    // 1. Read current epoch.
    let epoch_bytes = match store.get_sync(CF, KEY_CURRENT_EPOCH) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return Ok(None),
        Err(e) => anyhow::bail!("failed to read current epoch: {e}"),
    };
    let epoch: EpochNumber = bcs::from_bytes(&epoch_bytes).context("BCS decode epoch")?;

    // 2. Read epoch start time.
    let started_bytes = store
        .get_sync(CF, KEY_EPOCH_STARTED_AT)
        .map_err(|e| anyhow::anyhow!("failed to read epoch_started_at: {e}"))?
        .context("epoch_started_at missing when current epoch is present")?;
    let epoch_started_at: TimestampMs =
        bcs::from_bytes(&started_bytes).context("BCS decode epoch_started_at")?;

    // 3. Read committee for current epoch.
    let committee_bytes = store
        .get_sync(CF, &committee_key(epoch))
        .map_err(|e| anyhow::anyhow!("failed to read committee for epoch {}: {e}", epoch.0))?
        .with_context(|| format!("committee missing for epoch {}", epoch.0))?;
    let snap: PersistentCommittee =
        bcs::from_bytes(&committee_bytes).context("BCS decode committee")?;
    let committee = Committee::from_persistent(snap)
        .map_err(|e| anyhow::anyhow!("failed to reconstruct committee: {e}"))?;

    // 4. Scan transition history.
    let transitions = load_transition_history(store, epoch)?;

    // 5. Load election result for current epoch (optional).
    let election_result = match store.get_sync(CF, &election_key(epoch)) {
        Ok(Some(bytes)) => {
            let er: crate::staking_snapshot::PersistedElectionResult =
                bcs::from_bytes(&bytes).context("BCS decode election result")?;
            Some(er)
        }
        Ok(None) => None,
        Err(e) => {
            // Non-fatal: election result is supplementary.
            tracing::warn!(error = %e, "failed to read election result for epoch {}", epoch.0);
            None
        }
    };

    Ok(Some(PersistedEpochState {
        epoch,
        epoch_started_at,
        committee,
        transitions,
        election_result,
    }))
}

/// Load all epoch transitions up to `current_epoch`.
fn load_transition_history<S: StateStorage>(
    store: &S,
    current_epoch: EpochNumber,
) -> anyhow::Result<Vec<EpochTransition>> {
    let mut transitions = Vec::new();

    // Scan epochs 0..current_epoch for transition records.
    for e in 0..current_epoch.0 {
        let key = transition_key(EpochNumber(e));
        match store.get_sync(CF, &key) {
            Ok(Some(bytes)) => {
                let t: EpochTransition =
                    bcs::from_bytes(&bytes).context("BCS decode transition")?;
                transitions.push(t);
            }
            Ok(None) => {
                // Gap — transition record was not persisted (shouldn't happen,
                // but tolerate for forward compat).
            }
            Err(e) => anyhow::bail!("failed to read transition for epoch {}: {e}", e),
        }
    }

    Ok(transitions)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_consensus::types::{EpochTransitionTrigger, ReputationScore, ValidatorInfo};
    use nexus_crypto::Signer;
    use nexus_primitives::{Amount, ValidatorIndex};
    use nexus_storage::MemoryStore;

    fn make_test_committee(epoch: EpochNumber, n: u32) -> Committee {
        let validators: Vec<ValidatorInfo> = (0..n)
            .map(|i| {
                let (_, vk) = nexus_crypto::FalconSigner::generate_keypair();
                ValidatorInfo {
                    index: ValidatorIndex(i),
                    falcon_pub_key: vk,
                    stake: Amount(100),
                    reputation: ReputationScore::MAX,
                    is_slashed: false,
                    shard_id: None,
                }
            })
            .collect();
        Committee::new(epoch, validators).expect("test committee")
    }

    #[tokio::test]
    async fn round_trip_initial_epoch() {
        let store = MemoryStore::new();

        let committee = make_test_committee(EpochNumber(0), 4);
        persist_initial_epoch(&store, &committee).unwrap();

        let state = load_epoch_state(&store)
            .unwrap()
            .expect("should be present");
        assert_eq!(state.epoch, EpochNumber(0));
        assert_eq!(state.committee.active_count(), 4);
        assert!(state.transitions.is_empty());
    }

    #[tokio::test]
    async fn round_trip_epoch_transition() {
        let store = MemoryStore::new();

        let c0 = make_test_committee(EpochNumber(0), 4);
        persist_initial_epoch(&store, &c0).unwrap();

        let c1 = make_test_committee(EpochNumber(1), 5);
        let transition = EpochTransition {
            from_epoch: EpochNumber(0),
            to_epoch: EpochNumber(1),
            trigger: EpochTransitionTrigger::CommitThreshold,
            final_commit_count: 100,
            transitioned_at: TimestampMs::now(),
        };
        persist_epoch_transition(&store, &c1, &transition).unwrap();

        let state = load_epoch_state(&store)
            .unwrap()
            .expect("should be present");
        assert_eq!(state.epoch, EpochNumber(1));
        assert_eq!(state.committee.active_count(), 5);
        assert_eq!(state.transitions.len(), 1);
        assert_eq!(
            state.transitions[0].trigger,
            EpochTransitionTrigger::CommitThreshold,
        );
    }

    #[tokio::test]
    async fn load_returns_none_on_empty_store() {
        let store = MemoryStore::new();

        let state = load_epoch_state(&store).unwrap();
        assert!(state.is_none());
    }

    #[tokio::test]
    async fn round_trip_epoch_transition_with_election() {
        use crate::staking_snapshot::{ElectedValidator, PersistedElectionResult};
        use nexus_primitives::AccountAddress;

        let store = MemoryStore::new();

        let c0 = make_test_committee(EpochNumber(0), 4);
        persist_initial_epoch(&store, &c0).unwrap();

        let c1 = make_test_committee(EpochNumber(1), 4);
        let transition = EpochTransition {
            from_epoch: EpochNumber(0),
            to_epoch: EpochNumber(1),
            trigger: EpochTransitionTrigger::CommitThreshold,
            final_commit_count: 200,
            transitioned_at: TimestampMs::now(),
        };
        let election = PersistedElectionResult {
            for_epoch: EpochNumber(1),
            snapshot_epoch: EpochNumber(0),
            elected: vec![
                ElectedValidator {
                    address: AccountAddress([1; 32]),
                    effective_stake: 3_000_000_000,
                    committee_index: 0,
                },
                ElectedValidator {
                    address: AccountAddress([2; 32]),
                    effective_stake: 2_000_000_000,
                    committee_index: 1,
                },
            ],
            total_effective_stake: 5_000_000_000,
            is_fallback: false,
        };
        persist_epoch_transition_with_election(&store, &c1, &transition, Some(&election)).unwrap();

        let state = load_epoch_state(&store)
            .unwrap()
            .expect("should be present");
        assert_eq!(state.epoch, EpochNumber(1));
        let er = state
            .election_result
            .expect("election result should be loaded");
        assert_eq!(er.for_epoch, EpochNumber(1));
        assert_eq!(er.elected.len(), 2);
        assert_eq!(er.total_effective_stake, 5_000_000_000);
        assert!(!er.is_fallback);
    }

    #[tokio::test]
    async fn epoch_without_election_loads_none() {
        let store = MemoryStore::new();

        let c0 = make_test_committee(EpochNumber(0), 4);
        persist_initial_epoch(&store, &c0).unwrap();

        let c1 = make_test_committee(EpochNumber(1), 4);
        let transition = EpochTransition {
            from_epoch: EpochNumber(0),
            to_epoch: EpochNumber(1),
            trigger: EpochTransitionTrigger::TimeElapsed,
            final_commit_count: 50,
            transitioned_at: TimestampMs::now(),
        };
        // No election result passed.
        persist_epoch_transition(&store, &c1, &transition).unwrap();

        let state = load_epoch_state(&store)
            .unwrap()
            .expect("should be present");
        assert_eq!(state.epoch, EpochNumber(1));
        assert!(state.election_result.is_none());
    }
}
