//! Fuzz target: snapshot file parsing.
//!
//! Exercises the snapshot binary format parser (header + manifest +
//! entry stream) without writing to a real database.  The goal is to
//! ensure that malformed files never cause panics.

#![no_main]

use libfuzzer_sys::fuzz_target;

use nexus_storage::rocks::SnapshotManifest;

/// Maximum key / value sizes matching the production constants.
const MAX_KEY: usize = 1024;
const MAX_VAL: usize = 16 * 1024 * 1024;

/// Re-implement the parsing logic from `import_state_snapshot` in a
/// side-effect-free way so it can run millions of iterations per second.
fn parse_snapshot(data: &[u8]) -> Result<(), ()> {
    if data.len() < 4 {
        return Err(());
    }

    let header_len = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    if header_len > 65_536 {
        return Err(());
    }

    let rest = &data[4..];
    if rest.len() < header_len {
        return Err(());
    }

    let manifest: SnapshotManifest = bcs::from_bytes(&rest[..header_len]).map_err(|_| ())?;
    if manifest.version != 1 {
        return Err(());
    }

    // Walk the entry stream to exercise size-limit checks.
    let mut cursor = &rest[header_len..];
    let mut hasher = blake3::Hasher::new();

    for _ in 0..manifest.entry_count {
        if cursor.len() < 4 {
            return Err(());
        }
        let k_len = u32::from_le_bytes([cursor[0], cursor[1], cursor[2], cursor[3]]) as usize;
        cursor = &cursor[4..];
        if k_len > MAX_KEY || cursor.len() < k_len {
            return Err(());
        }
        hasher.update(&cursor[..k_len]);
        cursor = &cursor[k_len..];

        if cursor.len() < 4 {
            return Err(());
        }
        let v_len = u32::from_le_bytes([cursor[0], cursor[1], cursor[2], cursor[3]]) as usize;
        cursor = &cursor[4..];
        if v_len > MAX_VAL || cursor.len() < v_len {
            return Err(());
        }
        hasher.update(&cursor[..v_len]);
        cursor = &cursor[v_len..];
    }

    // Verify integrity hash if present.
    if let Some(expected) = manifest.content_hash {
        let actual: [u8; 32] = *hasher.finalize().as_bytes();
        if actual != expected {
            return Err(());
        }
    }

    Ok(())
}

fuzz_target!(|data: &[u8]| {
    // Also fuzz the manifest BCS path independently.
    let _ = bcs::from_bytes::<SnapshotManifest>(data);

    // Full binary format parse.
    let _ = parse_snapshot(data);
});
