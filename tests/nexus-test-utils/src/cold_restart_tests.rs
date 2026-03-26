//! H-1: End-to-end cold restart integration tests.
//!
//! These tests exercise the full cold-restart recovery path:
//!   Phase 1 — Run a mini consensus pipeline (insert certificates + batches).
//!   Phase 2 — "Crash" (drop all in-memory state).
//!   Phase 3 — Rebuild from the same storage, verify DAG + BatchStore intact.
//!
//! Also covers:
//!   - Epoch-mid crash recovery (advance epochs, crash, verify retention).
//!   - Multi-epoch pipeline cold restart with committed-but-unexecuted batches.

use nexus_consensus::persist::DagPersistence;
use nexus_consensus::types::EpochTransitionTrigger;
use nexus_consensus::ConsensusEngine;
use nexus_node::batch_persist::BatchPersistence;
use nexus_node::batch_store::BatchStore;
use nexus_primitives::{Blake3Digest, EpochNumber, ValidatorIndex};
use nexus_storage::MemoryStore;

use crate::fixtures::consensus::TestCommittee;

// ── Helpers ──────────────────────────────────────────────────────────────────

fn make_tx(seq: u64) -> nexus_execution::types::SignedTransaction {
    use nexus_crypto::{DilithiumSigner, Signer};
    use nexus_execution::types::{
        compute_tx_digest, TransactionBody, TransactionPayload, TX_DOMAIN,
    };
    use nexus_primitives::{AccountAddress, Amount, ShardId, TokenId};

    let (sk, vk) = DilithiumSigner::generate_keypair();
    let body = TransactionBody {
        sender: AccountAddress([1u8; 32]),
        sequence_number: seq,
        expiry_epoch: EpochNumber(100),
        gas_limit: 10_000,
        gas_price: 1,
        target_shard: Some(ShardId(0)),
        payload: TransactionPayload::Transfer {
            recipient: AccountAddress([2u8; 32]),
            amount: Amount(100),
            token: TokenId::Native,
        },
        chain_id: 1,
    };
    let digest = compute_tx_digest(&body).expect("digest");
    let body_bytes = bcs::to_bytes(&body).expect("bcs");
    let sig = DilithiumSigner::sign(&sk, TX_DOMAIN, &body_bytes);
    nexus_execution::types::SignedTransaction {
        body,
        signature: sig,
        sender_pk: vk,
        digest,
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// H-1a: Full pipeline cold restart — DAG + BatchStore survive together
// ═══════════════════════════════════════════════════════════════════════════════

/// Simulates a complete pipeline: insert genesis certificates into the engine
/// (which writes-through to disk) AND insert corresponding batches into the
/// BatchStore (which also writes-through). Then "crash" by dropping all
/// in-memory objects. Rebuild from the same MemoryStore and verify that both
/// the DAG certificates and batch payloads are fully recovered.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cold_restart_full_pipeline_dag_and_batches() {
    let store = MemoryStore::new();
    let num_validators = 4u32;

    // ── Phase 1: Run ──
    let tc = TestCommittee::new(num_validators as usize, EpochNumber(0));
    let certs: Vec<_> = (0..num_validators)
        .map(|i| tc.genesis_cert(ValidatorIndex(i)))
        .collect();

    // DAG engine with persistence.
    let _dag_persist = DagPersistence::new(store.clone());
    let mut engine = ConsensusEngine::new_with_persistence_and_retention(
        EpochNumber(0),
        tc.committee.clone(),
        Box::new(DagPersistence::new(store.clone())),
        2,
    );
    for cert in &certs {
        engine.insert_verified_certificate(cert.clone()).unwrap();
    }

    // BatchStore with persistence — one batch per certificate.
    let batch_persist = BatchPersistence::new(store.clone());
    let batch_store = BatchStore::new_with_persistence(Box::new(batch_persist));
    for (i, cert) in certs.iter().enumerate() {
        batch_store.insert(cert.batch_digest, vec![make_tx(i as u64)]);
    }
    assert_eq!(batch_store.len(), num_validators as usize);

    // Capture expected state before crash.
    let expected_dag_size = engine.dag_size();
    let expected_cert_digests: Vec<_> = certs.iter().map(|c| c.cert_digest).collect();
    let expected_batch_digests: Vec<_> = certs.iter().map(|c| c.batch_digest).collect();

    // ── Phase 2: Crash ──
    drop(engine);
    drop(batch_store);

    // ── Phase 3: Recover ──
    // Restore DAG.
    let dag_persist2 = DagPersistence::new(store.clone());
    let restored_certs = dag_persist2.restore_certificates().unwrap();
    assert_eq!(
        restored_certs.len(),
        expected_dag_size,
        "all {} DAG certificates must survive cold restart",
        expected_dag_size
    );
    for digest in &expected_cert_digests {
        assert!(
            restored_certs.iter().any(|c| c.cert_digest == *digest),
            "cert {digest} must be present after restore"
        );
    }

    // Rebuild engine from restored certs.
    let tc2 = TestCommittee::new(num_validators as usize, EpochNumber(0));
    let mut engine2 = ConsensusEngine::new_with_persistence_and_retention(
        EpochNumber(0),
        tc2.committee.clone(),
        Box::new(DagPersistence::new(store.clone())),
        2,
    );
    for cert in restored_certs {
        engine2.insert_verified_certificate(cert).unwrap();
    }
    assert_eq!(engine2.dag_size(), expected_dag_size);

    // Restore BatchStore.
    let batch_persist2 = BatchPersistence::new(store.clone());
    let batch_store2 = BatchStore::new_with_persistence(Box::new(batch_persist2));
    let restored_batch_count = batch_store2.restore_from_disk();
    assert_eq!(
        restored_batch_count, num_validators as usize,
        "all batches must survive cold restart"
    );
    for digest in &expected_batch_digests {
        assert!(
            batch_store2.get(digest).is_some(),
            "batch {digest} must be available after restore"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// H-1b: Mid-epoch crash — crash during an epoch, verify partial state recovery
// ═══════════════════════════════════════════════════════════════════════════════

/// Insert certificates across 2 epochs, crash mid-epoch-1, verify that
/// all persisted certificates (epoch 0 + partial epoch 1) survive.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cold_restart_mid_epoch_crash() {
    let store = MemoryStore::new();
    let n = 4u32;

    // ── Epoch 0: full round of genesis certs ──
    let tc0 = TestCommittee::new(n as usize, EpochNumber(0));
    let mut engine = ConsensusEngine::new_with_persistence_and_retention(
        EpochNumber(0),
        tc0.committee.clone(),
        Box::new(DagPersistence::new(store.clone())),
        2,
    );
    for i in 0..n {
        let cert = tc0.genesis_cert(ValidatorIndex(i));
        engine.insert_verified_certificate(cert).unwrap();
    }
    assert_eq!(engine.dag_size(), n as usize);

    // ── Advance to epoch 1 ──
    let tc1 = TestCommittee::new(n as usize, EpochNumber(1));
    engine.advance_epoch(
        tc1.committee.clone(),
        EpochTransitionTrigger::CommitThreshold,
    );
    assert_eq!(engine.epoch(), EpochNumber(1));

    // Insert only 2 of 4 epoch-1 certs (mid-epoch).
    let partial_certs: Vec<_> = (0..2)
        .map(|i| {
            tc1.build_cert(
                Blake3Digest([50 + i as u8; 32]),
                ValidatorIndex(i),
                nexus_primitives::RoundNumber(0),
                vec![],
            )
        })
        .collect();
    for cert in &partial_certs {
        engine.insert_verified_certificate(cert.clone()).unwrap();
    }

    // ── Crash ──
    drop(engine);

    // ── Recover ──
    let persist2 = DagPersistence::new(store);
    let restored = persist2.restore_certificates().unwrap();

    // Epoch 0 had 4 certs, epoch 1 had 2 partial → 6 total.
    assert_eq!(restored.len(), 6, "4 epoch-0 + 2 partial epoch-1 certs");

    let epoch0_count = restored
        .iter()
        .filter(|c| c.epoch == EpochNumber(0))
        .count();
    let epoch1_count = restored
        .iter()
        .filter(|c| c.epoch == EpochNumber(1))
        .count();
    assert_eq!(epoch0_count, 4);
    assert_eq!(epoch1_count, 2);
}

// ═══════════════════════════════════════════════════════════════════════════════
// H-1c: Multi-epoch pipeline with retention-aware cleanup across cold restart
// ═══════════════════════════════════════════════════════════════════════════════

/// Run 4 epochs with retention=2, crash after epoch 3, verify that only
/// epochs within the retention window survive on disk.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cold_restart_multi_epoch_retention() {
    let store = MemoryStore::new();
    let n = 4u32;
    let retention = 2u64;

    let tc0 = TestCommittee::new(n as usize, EpochNumber(0));
    let mut engine = ConsensusEngine::new_with_persistence_and_retention(
        EpochNumber(0),
        tc0.committee.clone(),
        Box::new(DagPersistence::new(store.clone())),
        retention,
    );

    // Epoch 0: insert genesis.
    for i in 0..n {
        engine
            .insert_verified_certificate(tc0.genesis_cert(ValidatorIndex(i)))
            .unwrap();
    }

    // Advance 0→1: retention=2, old=0, cutoff=0-2=underflow → no purge.
    let tc1 = TestCommittee::new(n as usize, EpochNumber(1));
    engine.advance_epoch(
        tc1.committee.clone(),
        EpochTransitionTrigger::CommitThreshold,
    );
    for i in 0..n {
        let cert = tc1.build_cert(
            Blake3Digest([10 + i as u8; 32]),
            ValidatorIndex(i),
            nexus_primitives::RoundNumber(0),
            vec![],
        );
        engine.insert_verified_certificate(cert).unwrap();
    }

    // Advance 1→2: old=1, cutoff=1-2=underflow → no purge.
    let tc2 = TestCommittee::new(n as usize, EpochNumber(2));
    engine.advance_epoch(
        tc2.committee.clone(),
        EpochTransitionTrigger::CommitThreshold,
    );
    for i in 0..n {
        let cert = tc2.build_cert(
            Blake3Digest([20 + i as u8; 32]),
            ValidatorIndex(i),
            nexus_primitives::RoundNumber(0),
            vec![],
        );
        engine.insert_verified_certificate(cert).unwrap();
    }

    // Advance 2→3: old=2, cutoff=2-2=0 → purge epoch 0.
    let tc3 = TestCommittee::new(n as usize, EpochNumber(3));
    engine.advance_epoch(
        tc3.committee.clone(),
        EpochTransitionTrigger::CommitThreshold,
    );
    for i in 0..n {
        let cert = tc3.build_cert(
            Blake3Digest([30 + i as u8; 32]),
            ValidatorIndex(i),
            nexus_primitives::RoundNumber(0),
            vec![],
        );
        engine.insert_verified_certificate(cert).unwrap();
    }

    // ── Crash ──
    drop(engine);

    // ── Recover ──
    let persist = DagPersistence::new(store.clone());
    let restored = persist.restore_certificates().unwrap();

    // Epoch 0 was purged. Epochs 1,2,3 each have 4 certs = 12.
    assert_eq!(restored.len(), 12, "epochs 1+2+3 × 4 certs = 12");
    for c in &restored {
        assert_ne!(c.epoch, EpochNumber(0), "epoch 0 should have been purged");
    }

    // Verify a fresh engine can ingest all restored certs for the current epoch (3).
    let tc3_fresh = TestCommittee::new(n as usize, EpochNumber(3));
    let mut engine2 = ConsensusEngine::new_with_persistence_and_retention(
        EpochNumber(3),
        tc3_fresh.committee.clone(),
        Box::new(DagPersistence::new(store)),
        retention,
    );
    let epoch3_certs: Vec<_> = restored
        .into_iter()
        .filter(|c| c.epoch == EpochNumber(3))
        .collect();
    assert_eq!(epoch3_certs.len(), 4);
    for cert in epoch3_certs {
        engine2.insert_verified_certificate(cert).unwrap();
    }
    assert_eq!(engine2.dag_size(), 4);
}

// ═══════════════════════════════════════════════════════════════════════════════
// H-1d: Committed-but-unexecuted batches survive cold restart
// ═══════════════════════════════════════════════════════════════════════════════

/// Consensus commits batches, execution bridge has NOT consumed them.
/// Node crashes, restarts. The execution bridge must be able to pick up
/// those batches to avoid transaction loss.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cold_restart_committed_unexecuted_batch_bridge() {
    let store = MemoryStore::new();
    let n = 4u32;

    // ── Phase 1: Consensus commits certs + batches ──
    let tc = TestCommittee::new(n as usize, EpochNumber(0));
    let mut engine = ConsensusEngine::new_with_persistence_and_retention(
        EpochNumber(0),
        tc.committee.clone(),
        Box::new(DagPersistence::new(store.clone())),
        2,
    );
    let batch_persist = BatchPersistence::new(store.clone());
    let batch_store = BatchStore::new_with_persistence(Box::new(batch_persist));

    // Simulate: proposer creates batches, engine commits certificates.
    let mut committed_batch_digests = Vec::new();
    for i in 0..n {
        let cert = tc.genesis_cert(ValidatorIndex(i));
        engine.insert_verified_certificate(cert.clone()).unwrap();

        // Proposer stores batch.
        let txs = vec![make_tx(i as u64 * 10), make_tx(i as u64 * 10 + 1)];
        batch_store.insert(cert.batch_digest, txs);
        committed_batch_digests.push(cert.batch_digest);
    }

    // Execution bridge has NOT consumed any batches yet.
    assert_eq!(batch_store.len(), n as usize);

    // ── Phase 2: Crash ──
    drop(engine);
    drop(batch_store);

    // ── Phase 3: Cold restart ──
    // Restore DAG.
    let dag_persist2 = DagPersistence::new(store.clone());
    let restored_certs = dag_persist2.restore_certificates().unwrap();
    assert_eq!(restored_certs.len(), n as usize);

    // Restore BatchStore.
    let batch_persist2 = BatchPersistence::new(store);
    let batch_store2 = BatchStore::new_with_persistence(Box::new(batch_persist2));
    let count = batch_store2.restore_from_disk();
    assert_eq!(
        count, n as usize,
        "all committed-but-unexecuted batches must restore"
    );

    // Execution bridge now consumes them.
    for digest in &committed_batch_digests {
        let txs = batch_store2
            .get(digest)
            .expect("committed batch must be available after cold restart");
        assert_eq!(txs.len(), 2, "each batch has 2 transactions");
    }

    // After consumption, remove.
    for digest in &committed_batch_digests {
        batch_store2.remove(digest);
    }
    assert!(batch_store2.is_empty());
}

// ═══════════════════════════════════════════════════════════════════════════════
// H-1e: Empty-state cold restart (genesis bootstrapping)
// ═══════════════════════════════════════════════════════════════════════════════

/// Fresh node starts with empty storage. DAG restore returns nothing,
/// genesis certificates are seeded, and the pipeline can proceed normally.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cold_restart_empty_state_genesis_bootstrap() {
    let store = MemoryStore::new();
    let n = 4u32;

    // Simulate main.rs logic: check disk first.
    let dag_persist = DagPersistence::new(store.clone());
    let restored_certs = dag_persist.restore_certificates().unwrap();
    assert!(
        restored_certs.is_empty(),
        "fresh store should have no certs"
    );

    // Since empty, seed genesis.
    let tc = TestCommittee::new(n as usize, EpochNumber(0));
    let mut engine = ConsensusEngine::new_with_persistence_and_retention(
        EpochNumber(0),
        tc.committee.clone(),
        Box::new(DagPersistence::new(store.clone())),
        2,
    );
    for i in 0..n {
        engine
            .insert_verified_certificate(tc.genesis_cert(ValidatorIndex(i)))
            .unwrap();
    }
    assert_eq!(engine.dag_size(), n as usize);

    // BatchStore also starts empty.
    let batch_persist = BatchPersistence::new(store);
    let batch_store = BatchStore::new_with_persistence(Box::new(batch_persist));
    let count = batch_store.restore_from_disk();
    assert_eq!(count, 0);
    assert!(batch_store.is_empty());
}
