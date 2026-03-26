//! Chain identity validation — genesis hash consistency checks.
//!
//! On first boot, stores the genesis hash in a marker file inside the
//! data directory. On subsequent boots, compares the current genesis hash
//! against the stored value. Fails fast on mismatch to prevent split-brain.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use anyhow::Context;
use nexus_config::genesis::GenesisConfig;
use nexus_primitives::Blake3Digest;

/// Name of the marker file that stores the genesis hash.
const GENESIS_HASH_FILE: &str = "genesis-hash";

/// Name of the marker file that stores the chain ID.
const CHAIN_ID_FILE: &str = "chain-id";

/// Validate chain identity on node boot.
///
/// - First boot (no marker files): stores chain_id and genesis_hash.
/// - Subsequent boots: compares and fails on mismatch.
///
/// `data_dir` is the node's persistent data directory (e.g. `/nexus/data`).
pub fn validate_chain_identity(
    data_dir: &Path,
    genesis: &GenesisConfig,
) -> anyhow::Result<Blake3Digest> {
    let genesis_hash = genesis.genesis_hash();
    let hash_hex = hex::encode(genesis_hash.as_bytes());

    let hash_path = data_dir.join(GENESIS_HASH_FILE);
    let chain_id_path = data_dir.join(CHAIN_ID_FILE);

    if hash_path.exists() {
        // Subsequent boot — validate consistency.
        let stored_hash = read_trimmed(&hash_path)?;
        if stored_hash != hash_hex {
            anyhow::bail!(
                "GENESIS HASH MISMATCH — refusing to start.\n\
                 Stored:  {stored_hash}\n\
                 Current: {hash_hex}\n\
                 The genesis file does not match the chain this node was initialised with.\n\
                 If you intend to re-initialise, remove {path}",
                path = hash_path.display(),
            );
        }
    } else {
        // First boot — record genesis hash.
        std::fs::write(&hash_path, &hash_hex)
            .with_context(|| format!("failed to write genesis hash to {}", hash_path.display()))?;
    }

    if chain_id_path.exists() {
        let stored_id = read_trimmed(&chain_id_path)?;
        if stored_id != genesis.chain_id {
            anyhow::bail!(
                "CHAIN ID MISMATCH — refusing to start.\n\
                 Stored:  {stored_id}\n\
                 Current: {chain_id}\n\
                 The genesis file belongs to a different chain.\n\
                 If you intend to re-initialise, remove {path}",
                chain_id = genesis.chain_id,
                path = chain_id_path.display(),
            );
        }
    } else {
        std::fs::write(&chain_id_path, &genesis.chain_id)
            .with_context(|| format!("failed to write chain ID to {}", chain_id_path.display()))?;
    }

    Ok(genesis_hash)
}

/// Path where the genesis hash marker is stored.
pub fn genesis_hash_path(data_dir: &Path) -> PathBuf {
    data_dir.join(GENESIS_HASH_FILE)
}

fn read_trimmed(path: &Path) -> anyhow::Result<String> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    Ok(content.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_genesis() -> GenesisConfig {
        GenesisConfig::for_testing()
    }

    #[test]
    fn first_boot_stores_markers() {
        let tmp = tempfile::tempdir().unwrap();
        let genesis = test_genesis();
        let hash = validate_chain_identity(tmp.path(), &genesis).unwrap();

        // Verify files were written.
        let stored_hash = std::fs::read_to_string(tmp.path().join("genesis-hash")).unwrap();
        assert_eq!(stored_hash, hex::encode(hash.as_bytes()));

        let stored_id = std::fs::read_to_string(tmp.path().join("chain-id")).unwrap();
        assert_eq!(stored_id, "nexus-test-0");
    }

    #[test]
    fn second_boot_same_genesis_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let genesis = test_genesis();

        // First boot.
        validate_chain_identity(tmp.path(), &genesis).unwrap();
        // Second boot — same genesis should succeed.
        validate_chain_identity(tmp.path(), &genesis).unwrap();
    }

    #[test]
    fn genesis_hash_mismatch_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let genesis = test_genesis();

        // First boot.
        validate_chain_identity(tmp.path(), &genesis).unwrap();

        // Tamper with stored hash.
        std::fs::write(tmp.path().join("genesis-hash"), "deadbeef00000000").unwrap();

        let result = validate_chain_identity(tmp.path(), &genesis);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("GENESIS HASH MISMATCH"));
    }

    #[test]
    fn chain_id_mismatch_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let genesis = test_genesis();

        // First boot.
        validate_chain_identity(tmp.path(), &genesis).unwrap();

        // Tamper with chain ID.
        std::fs::write(tmp.path().join("chain-id"), "wrong-chain-99").unwrap();

        let result = validate_chain_identity(tmp.path(), &genesis);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("CHAIN ID MISMATCH"));
    }

    #[test]
    fn genesis_hash_deterministic() {
        let genesis = test_genesis();
        let h1 = genesis.genesis_hash();
        let h2 = genesis.genesis_hash();
        assert_eq!(h1, h2);
    }

    #[test]
    fn different_genesis_different_hash() {
        let g1 = test_genesis();
        let mut g2 = test_genesis();
        g2.chain_id = "nexus-test-different".to_owned();
        assert_ne!(g1.genesis_hash(), g2.genesis_hash());
    }
}
