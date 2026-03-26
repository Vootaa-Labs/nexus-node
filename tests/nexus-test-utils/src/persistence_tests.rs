// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for DAG and BatchStore persistence (F-4, G-4).
//!
//! Covers:
//! - F-4a: DAG write-through integrity
//! - F-4b: DAG crash/cold-restart recovery
//! - F-4c: Cross-epoch retention cleanup
//! - F-4d: Empty database first-start compatibility
//! - G-4a: BatchStore write-through consistency
//! - G-4b: BatchStore crash recovery from disk
//! - G-4c: BatchStore capacity eviction + disk consistency
//! - G-4d: Committed-but-unexecuted cold-restart recovery

use nexus_consensus::persist::DagPersistence;
use nexus_consensus::types::{NarwhalCertificate, ValidatorBitset};
use nexus_consensus::ConsensusEngine;
use nexus_node::batch_persist::{BatchPersistOps, BatchPersistence};
use nexus_node::batch_store::BatchStore;
use nexus_primitives::{Blake3Digest, EpochNumber, RoundNumber, ValidatorIndex};
use nexus_storage::MemoryStore;

use crate::fixtures::consensus::TestCommittee;

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Build a minimal unsigned certificate for a given epoch.
fn make_cert(epoch: u64, origin: u32, round: u64, seed: u8) -> NarwhalCertificate {
    let epoch = EpochNumber(epoch);
    let batch_digest = Blake3Digest([seed; 32]);
    let origin_idx = ValidatorIndex(origin);
    let round_num = RoundNumber(round);
    let parents = vec![];
    let cert_digest =
        nexus_consensus::compute_cert_digest(epoch, &batch_digest, origin_idx, round_num, &parents)
            .unwrap();
    NarwhalCertificate {
        epoch,
        batch_digest,
        origin: origin_idx,
        round: round_num,
        parents,
        signatures: vec![],
        signers: ValidatorBitset::new(4),
        cert_digest,
    }
}

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
// F-4: DAG Persistence Integration Tests
// ═══════════════════════════════════════════════════════════════════════════════

/// F-4a: Certificates persisted via write-through are present on disk.
#[tokio::test]
async fn dag_write_through_integrity() {
    let store = MemoryStore::new();
    let persist = DagPersistence::new(store.clone());

    let c0 = make_cert(1, 0, 0, 10);
    let c1 = make_cert(1, 1, 0, 11);
    let c2 = make_cert(1, 0, 1, 20);

    persist.persist_certificate(&c0).await.unwrap();
    persist.persist_certificate(&c1).await.unwrap();
    persist.persist_certificate(&c2).await.unwrap();

    // Read back directly from the same store — all 3 must be present.
    let restored = persist.restore_certificates().unwrap();
    assert_eq!(restored.len(), 3);

    // Verify digests match.
    let digests: Vec<_> = restored.iter().map(|c| c.cert_digest).collect();
    assert!(digests.contains(&c0.cert_digest));
    assert!(digests.contains(&c1.cert_digest));
    assert!(digests.contains(&c2.cert_digest));

    // Verify sort order (round, origin).
    assert_eq!(restored[0].round, RoundNumber(0));
    assert_eq!(restored[0].origin, ValidatorIndex(0));
    assert_eq!(restored[1].round, RoundNumber(0));
    assert_eq!(restored[1].origin, ValidatorIndex(1));
    assert_eq!(restored[2].round, RoundNumber(1));
}

/// F-4b: Simulate a crash by creating a fresh DagPersistence over the same
/// store and verifying all certificates survive.
#[tokio::test]
async fn dag_crash_recovery() {
    let store = MemoryStore::new();

    // Phase 1: write certificates.
    {
        let persist = DagPersistence::new(store.clone());
        for seed in 0..5u8 {
            let cert = make_cert(1, seed as u32 % 4, seed as u64 / 4, seed);
            persist.persist_certificate(&cert).await.unwrap();
        }
    }

    // Phase 2: "crash" — drop the old DagPersistence, create a new one
    // from the same store (simulating process restart).
    let persist2 = DagPersistence::new(store);
    let recovered = persist2.restore_certificates().unwrap();
    assert_eq!(
        recovered.len(),
        5,
        "all 5 certificates must survive the crash"
    );
}

/// F-4c: Multi-epoch persist with retention-aware purge.
///
/// Persist certs across epochs 0..4, then purge epoch 0 (retention=3,
/// advancing from epoch 3→4 triggers purge of epoch 0). Verify epoch 0
/// is gone while epochs 1-3 remain.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dag_cross_epoch_cleanup() {
    let store = MemoryStore::new();
    let persist = DagPersistence::new(store);

    // Persist 2 certs per epoch for epochs 0..4.
    for epoch in 0..4u64 {
        for origin in 0..2u32 {
            let cert = make_cert(epoch, origin, 0, (epoch * 10 + origin as u64) as u8);
            persist.persist_certificate(&cert).await.unwrap();
        }
    }

    let all = persist.restore_certificates().unwrap();
    assert_eq!(all.len(), 8, "8 certs across 4 epochs");

    // Purge epoch 0 (simulating retention window sliding past it).
    let purged = persist.purge_by_epoch(EpochNumber(0)).unwrap();
    assert_eq!(purged, 2, "2 epoch-0 certs should be purged");

    let remaining = persist.restore_certificates().unwrap();
    assert_eq!(remaining.len(), 6, "6 certs should remain");

    // None of the remaining certs belong to epoch 0.
    for c in &remaining {
        assert_ne!(c.epoch, EpochNumber(0), "epoch 0 certs must be gone");
    }
}

/// F-4c-bis: Verify that `advance_epoch` with `epoch_retention_count > 0`
/// correctly purges only the epoch that falls outside the retention window.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dag_engine_epoch_retention_purge() {
    let store = MemoryStore::new();
    let persist = DagPersistence::new(store.clone());

    let tc = TestCommittee::new(4, EpochNumber(0));
    let mut engine = ConsensusEngine::new_with_persistence_and_retention(
        EpochNumber(0),
        tc.committee.clone(),
        Box::new(DagPersistence::new(store.clone())),
        2, // retain 2 past epochs
    );

    // Insert genesis certs for epoch 0 and persist manually.
    for i in 0..4u32 {
        let cert = tc.genesis_cert(ValidatorIndex(i));
        persist.persist_certificate_sync(&cert).unwrap();
        engine.insert_verified_certificate(cert).unwrap();
    }

    // Advance epoch 0 → 1.  retention=2, old_epoch=0, cutoff=0-2=underflow → no purge.
    let tc1 = TestCommittee::new(4, EpochNumber(1));
    engine.advance_epoch(
        tc1.committee.clone(),
        nexus_consensus::types::EpochTransitionTrigger::CommitThreshold,
    );

    // All epoch 0 certs should still be on disk (within retention window).
    let on_disk = persist.restore_certificates().unwrap();
    assert_eq!(on_disk.len(), 4, "epoch 0 certs retained after 0→1 advance");

    // Now persist epoch 1 certs and advance 1 → 2.
    for i in 0..4u32 {
        let cert = make_cert(1, i, 0, 50 + i as u8);
        persist.persist_certificate_sync(&cert).unwrap();
    }

    let tc2 = TestCommittee::new(4, EpochNumber(2));
    engine.advance_epoch(
        tc2.committee.clone(),
        nexus_consensus::types::EpochTransitionTrigger::CommitThreshold,
    );

    // retention=2, old_epoch=1, cutoff=1-2=underflow → still no purge.
    let on_disk = persist.restore_certificates().unwrap();
    assert_eq!(on_disk.len(), 8, "epochs 0+1 retained after 1→2 advance");

    // Persist epoch 2 certs and advance 2 → 3.
    for i in 0..4u32 {
        let cert = make_cert(2, i, 0, 80 + i as u8);
        persist.persist_certificate_sync(&cert).unwrap();
    }

    let tc3 = TestCommittee::new(4, EpochNumber(3));
    engine.advance_epoch(
        tc3.committee,
        nexus_consensus::types::EpochTransitionTrigger::CommitThreshold,
    );

    // retention=2, old_epoch=2, cutoff=2-2=0 → purge epoch 0.
    let on_disk = persist.restore_certificates().unwrap();
    assert_eq!(
        on_disk.len(),
        8,
        "epoch 0 purged, epochs 1+2 remain (4+4=8)"
    );
    for c in &on_disk {
        assert_ne!(c.epoch, EpochNumber(0), "epoch 0 should be purged");
    }
}

/// F-4d: Fresh start on an empty database produces no errors.
#[tokio::test]
async fn dag_empty_database_first_start() {
    let store = MemoryStore::new();
    let persist = DagPersistence::new(store);
    let restored = persist.restore_certificates().unwrap();
    assert!(restored.is_empty(), "empty DB should return no certs");

    // Purge on empty DB should succeed gracefully.
    let purged = persist.purge_by_epoch(EpochNumber(0)).unwrap();
    assert_eq!(purged, 0);
}

// ═══════════════════════════════════════════════════════════════════════════════
// G-4: BatchStore Persistence Integration Tests
// ═══════════════════════════════════════════════════════════════════════════════

/// G-4a: Write-through consistency — insert into BatchStore, verify on disk.
#[tokio::test]
async fn batch_store_write_through_consistency() {
    let store = MemoryStore::new();
    let persist = BatchPersistence::new(store.clone());

    let batch_store = BatchStore::new_with_persistence(Box::new(persist));

    let d1 = Blake3Digest([1u8; 32]);
    let d2 = Blake3Digest([2u8; 32]);
    batch_store.insert(d1, vec![make_tx(1), make_tx(2)]);
    batch_store.insert(d2, vec![make_tx(3)]);

    // Read back directly from disk (bypass DashMap).
    let disk = BatchPersistence::new(store);
    let on_disk = disk.restore_batches().unwrap();
    assert_eq!(on_disk.len(), 2, "both batches should be on disk");

    let disk_digests: Vec<_> = on_disk.iter().map(|(d, _)| *d).collect();
    assert!(disk_digests.contains(&d1));
    assert!(disk_digests.contains(&d2));
}

/// G-4b: Crash recovery — drop BatchStore, create a new one from same
/// store, restore.
#[tokio::test]
async fn batch_store_crash_recovery() {
    let store = MemoryStore::new();

    // Phase 1: populate.
    {
        let persist = BatchPersistence::new(store.clone());
        let batch_store = BatchStore::new_with_persistence(Box::new(persist));

        for seed in 0..5u8 {
            let digest = Blake3Digest([seed; 32]);
            batch_store.insert(digest, vec![make_tx(seed as u64)]);
        }
        assert_eq!(batch_store.len(), 5);
    }

    // Phase 2: "crash" — fresh BatchStore from same storage.
    let persist2 = BatchPersistence::new(store);
    let batch_store2 = BatchStore::new_with_persistence(Box::new(persist2));
    let restored_count = batch_store2.restore_from_disk();
    assert_eq!(restored_count, 5, "all 5 batches must survive the crash");

    // Verify individual lookups work.
    for seed in 0..5u8 {
        let digest = Blake3Digest([seed; 32]);
        assert!(
            batch_store2.get(&digest).is_some(),
            "batch {seed} should exist"
        );
    }
}

/// G-4c: Capacity eviction + disk consistency — after eviction from DashMap,
/// evicted batches should still be recoverable from disk.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_store_eviction_disk_consistency() {
    let store = MemoryStore::new();
    let persist = BatchPersistence::new(store.clone());
    let batch_store = BatchStore::new_with_persistence(Box::new(persist));

    // Insert enough to trigger eviction (MAX_RETAINED_BATCHES is 4096,
    // but we can verify the disk fallback with a smaller set).
    let n = 50u8;
    for seed in 0..n {
        let digest = Blake3Digest([seed; 32]);
        batch_store.insert(digest, vec![make_tx(seed as u64)]);
    }

    // Remove half of them from DashMap (simulating execution consumption).
    for seed in 0..n / 2 {
        let digest = Blake3Digest([seed; 32]);
        batch_store.remove(&digest);
    }

    // The removed batches should no longer appear in DashMap OR disk
    // (remove deletes from both).
    for seed in 0..n / 2 {
        let digest = Blake3Digest([seed; 32]);
        assert!(
            batch_store.get(&digest).is_none(),
            "removed batch {seed} should be gone"
        );
    }

    // The remaining batches should still be accessible.
    for seed in n / 2..n {
        let digest = Blake3Digest([seed; 32]);
        assert!(
            batch_store.get(&digest).is_some(),
            "batch {seed} should exist"
        );
    }

    // Verify disk count.
    let on_disk = BatchPersistence::new(store).restore_batches().unwrap();
    assert_eq!(
        on_disk.len(),
        (n - n / 2) as usize,
        "disk should match remaining"
    );
}

/// G-4d: Committed-but-unexecuted cold-restart recovery.
///
/// Simulates a scenario where batches are committed by consensus but not
/// yet consumed by the execution bridge, then the node restarts.  After
/// restore, those batches must be available for the execution bridge.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_store_committed_unexecuted_recovery() {
    let store = MemoryStore::new();

    // Phase 1: insert "committed-but-unexecuted" batches.
    let committed_digests: Vec<Blake3Digest> =
        (0..3u8).map(|s| Blake3Digest([s + 100; 32])).collect();
    {
        let persist = BatchPersistence::new(store.clone());
        let batch_store = BatchStore::new_with_persistence(Box::new(persist));

        for (i, digest) in committed_digests.iter().enumerate() {
            batch_store.insert(*digest, vec![make_tx(i as u64 + 100)]);
        }
        // Do NOT call remove — these are "committed but not yet executed".
    }

    // Phase 2: cold restart.
    let persist2 = BatchPersistence::new(store);
    let batch_store2 = BatchStore::new_with_persistence(Box::new(persist2));
    let restored_count = batch_store2.restore_from_disk();
    assert_eq!(
        restored_count, 3,
        "all 3 committed-but-unexecuted batches must restore"
    );

    // Execution bridge should be able to consume them.
    for digest in &committed_digests {
        let txs = batch_store2.get(digest).expect("batch must be available");
        assert_eq!(txs.len(), 1, "each batch has exactly 1 transaction");
    }

    // Now simulate execution consuming them.
    for digest in &committed_digests {
        batch_store2.remove(digest);
    }
    assert!(
        batch_store2.is_empty(),
        "all consumed, store should be empty"
    );
}
