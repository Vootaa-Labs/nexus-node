//! P5 end-to-end recovery tests.
//!
//! Validates snapshot export → import, pruning → query behaviour,
//! schema migration → cold start, and storage re-open semantics.
//!
//! Acceptance criteria (from roadmap D-4):
//!   - Snapshot export on store A → import on store B → data matches.
//!   - Pruning removes old entries; newer entries remain queryable.
//!   - Migration from a fresh v1-style DB to current schema survives cold restart.
//!   - Re-opened store retains previously written state.

use nexus_storage::config::StorageConfig;
use nexus_storage::rocks::RocksStore;
use nexus_storage::traits::StateStorage;
use nexus_storage::types::ColumnFamily;
use tempfile::TempDir;

/// Helper: open a new RocksStore in a fresh temp directory.
fn open_temp_store(label: &str) -> (RocksStore, TempDir) {
    let tmp =
        TempDir::new().unwrap_or_else(|e| panic!("failed to create tempdir for {label}: {e}"));
    let config = StorageConfig::for_testing(tmp.path().join("db"));
    let store =
        RocksStore::open(&config).unwrap_or_else(|e| panic!("failed to open store {label}: {e}"));
    (store, tmp)
}

// ── Snapshot export → import ────────────────────────────────────────────

#[test]
fn snapshot_export_import_round_trip() {
    let (src_store, src_tmp) = open_temp_store("src");
    let (dst_store, _dst_tmp) = open_temp_store("dst");

    // Write 50 entries into the source store's State CF.
    let cf = ColumnFamily::State.as_str();
    for i in 0..50u32 {
        src_store
            .put_sync(
                cf,
                format!("key-{i:04}").into_bytes(),
                format!("val-{i:04}").into_bytes(),
            )
            .unwrap();
    }

    // Export snapshot (entry count includes the schema version marker).
    let snap_dir = src_tmp.path().join("snapshot_out");
    std::fs::create_dir_all(&snap_dir).unwrap();
    let manifest = src_store
        .export_state_snapshot(&snap_dir, 42, None)
        .unwrap();

    assert_eq!(manifest.version, 1);
    assert_eq!(manifest.block_height, 42);
    // 50 user entries + 1 schema version marker key
    assert!(
        manifest.entry_count >= 50,
        "expected at least 50 entries, got {}",
        manifest.entry_count
    );
    assert!(manifest.content_hash.is_some());

    // Import into destination store.
    let imported_manifest = dst_store.import_state_snapshot(&snap_dir, None).unwrap();
    assert_eq!(imported_manifest.entry_count, manifest.entry_count);

    // Verify every entry is present in the destination.
    for i in 0..50u32 {
        let key = format!("key-{i:04}");
        let val = dst_store.get_sync(cf, key.as_bytes()).unwrap();
        assert_eq!(
            val.as_deref(),
            Some(format!("val-{i:04}").as_bytes()),
            "entry {key} mismatch after import"
        );
    }
}

#[test]
fn snapshot_import_tampered_content_rejected() {
    let (src_store, src_tmp) = open_temp_store("src");
    let (dst_store, _dst_tmp) = open_temp_store("dst");

    let cf = ColumnFamily::State.as_str();
    for i in 0..5u32 {
        src_store
            .put_sync(
                cf,
                format!("k{i}").into_bytes(),
                format!("v{i}").into_bytes(),
            )
            .unwrap();
    }

    let snap_dir = src_tmp.path().join("snap");
    std::fs::create_dir_all(&snap_dir).unwrap();
    src_store.export_state_snapshot(&snap_dir, 1, None).unwrap();

    // Tamper with the snapshot file.
    let snap_file = snap_dir.join("state_snapshot.bin");
    let mut data = std::fs::read(&snap_file).unwrap();
    if let Some(last) = data.last_mut() {
        *last ^= 0xFF;
    }
    std::fs::write(&snap_file, &data).unwrap();

    let result = dst_store.import_state_snapshot(&snap_dir, None);
    assert!(result.is_err(), "tampered snapshot should be rejected");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("integrity")
            || err_msg.contains("truncated")
            || err_msg.contains("invalid"),
        "error should mention integrity/truncation, got: {err_msg}"
    );
}

// ── Pruning → query behaviour ───────────────────────────────────────────

#[test]
fn prune_removes_old_entries_newer_entries_survive() {
    let (store, _tmp) = open_temp_store("prune");

    // Write entries with sequence-keyed keys across multiple CFs.
    // prune_before operates on Blocks, Transactions, Receipts CFs using
    // big-endian u64 keys.
    let cfs = [
        ColumnFamily::Blocks,
        ColumnFamily::Transactions,
        ColumnFamily::Receipts,
    ];

    for &cf in &cfs {
        for seq in 0u64..20 {
            let key = seq.to_be_bytes().to_vec();
            let val = format!("data-{seq}").into_bytes();
            store.put_sync(cf.as_str(), key, val).unwrap();
        }
    }

    // Prune everything before sequence 10.
    let result = store.prune_before(10).unwrap();
    assert!(
        result.blocks_pruned > 0 || result.transactions_pruned > 0 || result.receipts_pruned > 0,
        "pruning should have removed some entries"
    );

    // Entries 0–9 should be gone; 10–19 should survive.
    for &cf in &cfs {
        for seq in 0u64..10 {
            let key = seq.to_be_bytes().to_vec();
            let val = store.get_sync(cf.as_str(), &key).unwrap();
            assert!(
                val.is_none(),
                "entry seq={seq} in {cf:?} should have been pruned"
            );
        }
        for seq in 10u64..20 {
            let key = seq.to_be_bytes().to_vec();
            let val = store.get_sync(cf.as_str(), &key).unwrap();
            assert!(
                val.is_some(),
                "entry seq={seq} in {cf:?} should survive pruning"
            );
        }
    }
}

// ── Migration → cold start ──────────────────────────────────────────────

#[test]
fn migration_cold_start_preserves_state() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("db");
    let config = StorageConfig::for_testing(db_path.clone());

    // First open: writes some data, then drop (simulates shutdown).
    {
        let store = RocksStore::open(&config).unwrap();
        let cf = ColumnFamily::State.as_str();
        store
            .put_sync(cf, b"alpha".to_vec(), b"one".to_vec())
            .unwrap();
        store
            .put_sync(cf, b"beta".to_vec(), b"two".to_vec())
            .unwrap();
        // store drops here, DB closed.
    }

    // Second open: cold start, migration runs, data must survive.
    {
        let store = RocksStore::open(&config).unwrap();
        let cf = ColumnFamily::State.as_str();

        let alpha = store.get_sync(cf, b"alpha").unwrap();
        assert_eq!(alpha.as_deref(), Some(b"one".as_slice()));

        let beta = store.get_sync(cf, b"beta").unwrap();
        assert_eq!(beta.as_deref(), Some(b"two".as_slice()));
    }
}

// ── Re-open preserves all column families ───────────────────────────────

#[test]
fn reopen_store_preserves_cross_cf_data() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("db");
    let config = StorageConfig::for_testing(db_path.clone());

    let cfs = [
        ColumnFamily::Blocks,
        ColumnFamily::Transactions,
        ColumnFamily::Receipts,
        ColumnFamily::State,
    ];

    // First session: write one entry per CF.
    {
        let store = RocksStore::open(&config).unwrap();
        for (i, &cf) in cfs.iter().enumerate() {
            store
                .put_sync(
                    cf.as_str(),
                    format!("cf-key-{i}").into_bytes(),
                    format!("cf-val-{i}").into_bytes(),
                )
                .unwrap();
        }
    }

    // Second session: verify all entries.
    {
        let store = RocksStore::open(&config).unwrap();
        for (i, &cf) in cfs.iter().enumerate() {
            let key = format!("cf-key-{i}");
            let val = store.get_sync(cf.as_str(), key.as_bytes()).unwrap();
            assert_eq!(
                val.as_deref(),
                Some(format!("cf-val-{i}").as_bytes()),
                "CF {cf:?} data lost after reopen"
            );
        }
    }
}

// ── Checkpoint → open as readonly replica ───────────────────────────────

#[test]
fn checkpoint_export_opens_as_independent_store() {
    let (store, tmp) = open_temp_store("checkpoint-src");

    let cf = ColumnFamily::State.as_str();
    store
        .put_sync(cf, b"ck-key".to_vec(), b"ck-val".to_vec())
        .unwrap();

    let ck_path = tmp.path().join("checkpoint");
    store.create_checkpoint(&ck_path).unwrap();

    // Open checkpoint as a new store (independent of source).
    let ck_config = StorageConfig::for_testing(ck_path.clone());
    let ck_store = RocksStore::open_at(&ck_path, &ck_config).unwrap();

    let val = ck_store.get_sync(cf, b"ck-key").unwrap();
    assert_eq!(val.as_deref(), Some(b"ck-val".as_slice()));

    // Writes to the checkpoint should NOT appear in the original.
    ck_store
        .put_sync(cf, b"ck-only".to_vec(), b"extra".to_vec())
        .unwrap();
    let absent = store.get_sync(cf, b"ck-only").unwrap();
    assert!(
        absent.is_none(),
        "checkpoint writes must be isolated from source"
    );
}

// ── Storage stats after mixed writes ────────────────────────────────────

#[test]
fn storage_stats_reflects_written_data() {
    let (store, _tmp) = open_temp_store("stats");

    let cf = ColumnFamily::State.as_str();
    for i in 0..100u32 {
        store
            .put_sync(cf, format!("s-{i:04}").into_bytes(), vec![0xAA; 256])
            .unwrap();
    }

    let stats = store.storage_stats().unwrap();
    assert!(!stats.is_empty(), "should return stats for all CFs");

    let state_cf = stats.iter().find(|s| s.cf == ColumnFamily::State);
    assert!(state_cf.is_some(), "State CF should be in stats");
}
