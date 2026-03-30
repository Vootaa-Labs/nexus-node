// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Nexus Move package artifact generation.
//!
//! After `aptos move compile` produces the standard build output, this
//! module post-processes it into a Nexus-specific artifact bundle:
//!
//! ```text
//! <package_dir>/nexus-artifact/
//! ├── package-metadata.bcs   ← BCS-encoded PackageMetadata
//! ├── manifest.json          ← Human-readable summary
//! └── bytecode/
//!     ├── counter.mv         ← Compiled modules (copied)
//!     └── ...
//! ```
//!
//! The [`PackageMetadata`] struct is layout-compatible with the one in
//! `nexus-execution::move_adapter::package` so that the node can decode
//! artifacts produced by this CLI.

use nexus_primitives::AccountAddress;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpgradePolicy {
    Immutable,
    Compatible,
    GovernanceOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PackageMetadata {
    pub name: String,
    pub package_hash: [u8; 32],
    pub named_addresses: Vec<(String, AccountAddress)>,
    pub module_hashes: Vec<(String, [u8; 32])>,
    pub abi_hash: [u8; 32],
    pub upgrade_policy: UpgradePolicy,
    pub deployer: AccountAddress,
    pub version: u64,
}

#[derive(Serialize)]
struct Manifest {
    package_name: String,
    package_hash: String,
    module_count: usize,
    total_bytecode_bytes: usize,
    upgrade_policy: String,
    modules: Vec<ManifestModule>,
}

#[derive(Serialize)]
struct ManifestModule {
    name: String,
    size_bytes: usize,
    blake3: String,
}

#[derive(Debug)]
pub struct ArtifactResult {
    pub artifact_dir: PathBuf,
    #[allow(dead_code)]
    pub metadata: PackageMetadata,
    pub module_count: usize,
    pub total_bytes: usize,
}

pub fn generate(package_dir: &Path) -> anyhow::Result<ArtifactResult> {
    let build_dir = package_dir.join("build");
    if !build_dir.exists() {
        anyhow::bail!("no build/ directory found in {}", package_dir.display());
    }

    let (pkg_name, pkg_build_dir) = find_package_build(&build_dir)?;
    let modules = collect_modules(&pkg_build_dir.join("bytecode_modules"))?;
    if modules.is_empty() {
        anyhow::bail!("no .mv bytecode modules found for package '{}'", pkg_name);
    }

    let upgrade_policy = parse_upgrade_policy(package_dir);
    let named_addresses = parse_named_addresses(package_dir);

    let mut module_hashes = Vec::with_capacity(modules.len());
    let mut hasher = blake3::Hasher::new();
    let mut total_bytes = 0usize;

    for (name, bytes) in &modules {
        let h = blake3::hash(bytes);
        module_hashes.push((name.clone(), *h.as_bytes()));
        hasher.update(bytes);
        total_bytes += bytes.len();
    }

    let package_hash = *hasher.finalize().as_bytes();

    let metadata = PackageMetadata {
        name: pkg_name.clone(),
        package_hash,
        named_addresses,
        module_hashes: module_hashes.clone(),
        abi_hash: [0u8; 32],
        upgrade_policy,
        deployer: AccountAddress([0u8; 32]),
        version: 1,
    };

    let artifact_dir = package_dir.join("nexus-artifact");
    std::fs::create_dir_all(&artifact_dir)?;

    let bcs_bytes = bcs::to_bytes(&metadata)?;
    std::fs::write(artifact_dir.join("package-metadata.bcs"), &bcs_bytes)?;

    let bc_dir = artifact_dir.join("bytecode");
    std::fs::create_dir_all(&bc_dir)?;
    for (name, bytes) in &modules {
        std::fs::write(bc_dir.join(format!("{name}.mv")), bytes)?;
    }

    let manifest = Manifest {
        package_name: pkg_name,
        package_hash: hex::encode(package_hash),
        module_count: modules.len(),
        total_bytecode_bytes: total_bytes,
        upgrade_policy: format!("{:?}", upgrade_policy),
        modules: module_hashes
            .iter()
            .zip(modules.iter())
            .map(|((name, hash), (_, bytes))| ManifestModule {
                name: name.clone(),
                size_bytes: bytes.len(),
                blake3: hex::encode(hash),
            })
            .collect(),
    };
    let json = serde_json::to_string_pretty(&manifest)?;
    std::fs::write(artifact_dir.join("manifest.json"), json)?;

    Ok(ArtifactResult {
        artifact_dir,
        metadata,
        module_count: modules.len(),
        total_bytes,
    })
}

fn find_package_build(build_dir: &Path) -> anyhow::Result<(String, PathBuf)> {
    // Read the package name from Move.toml so we pick the user's package
    // and not a dependency directory (e.g. "framework").
    let move_toml = build_dir.parent().unwrap_or(build_dir).join("Move.toml");
    if let Ok(content) = std::fs::read_to_string(&move_toml) {
        if let Ok(table) = content.parse::<toml::Table>() {
            if let Some(name) = table
                .get("package")
                .and_then(|v| v.as_table())
                .and_then(|t| t.get("name"))
                .and_then(|v| v.as_str())
            {
                let candidate = build_dir.join(name);
                if candidate.is_dir() {
                    return Ok((name.to_string(), candidate));
                }
            }
        }
    }

    // Fallback: first directory that contains a bytecode_modules/ subdirectory
    for entry in std::fs::read_dir(build_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() && entry.path().join("bytecode_modules").is_dir() {
            if let Some(name) = entry.file_name().to_str() {
                return Ok((name.to_string(), entry.path()));
            }
        }
    }
    anyhow::bail!("no package directory found in build/")
}

fn collect_modules(bytecode_dir: &Path) -> anyhow::Result<Vec<(String, Vec<u8>)>> {
    let mut modules = Vec::new();
    if !bytecode_dir.exists() {
        return Ok(modules);
    }
    for entry in std::fs::read_dir(bytecode_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "mv") && path.is_file() {
            let name = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let bytes = std::fs::read(&path)?;
            modules.push((name, bytes));
        }
    }
    modules.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(modules)
}

fn parse_upgrade_policy(package_dir: &Path) -> UpgradePolicy {
    let move_toml = package_dir.join("Move.toml");
    if let Ok(content) = std::fs::read_to_string(&move_toml) {
        if let Ok(table) = content.parse::<toml::Table>() {
            if let Some(pkg) = table.get("package").and_then(|v| v.as_table()) {
                if let Some(policy) = pkg.get("upgrade_policy").and_then(|v| v.as_str()) {
                    return match policy {
                        "compatible" => UpgradePolicy::Compatible,
                        "governance_only" => UpgradePolicy::GovernanceOnly,
                        _ => UpgradePolicy::Immutable,
                    };
                }
            }
        }
    }
    UpgradePolicy::Immutable
}

fn parse_named_addresses(package_dir: &Path) -> Vec<(String, AccountAddress)> {
    let move_toml = package_dir.join("Move.toml");
    let mut result = Vec::new();
    if let Ok(content) = std::fs::read_to_string(&move_toml) {
        if let Ok(table) = content.parse::<toml::Table>() {
            let section = table
                .get("dev-addresses")
                .or_else(|| table.get("addresses"));
            if let Some(addrs) = section.and_then(|v| v.as_table()) {
                for (name, val) in addrs {
                    if let Some(hex_str) = val.as_str() {
                        if hex_str != "_" {
                            let stripped = hex_str.strip_prefix("0x").unwrap_or(hex_str);
                            match hex::decode(stripped) {
                                Ok(bytes) => {
                                    let mut addr = [0u8; 32];
                                    let start = 32usize.saturating_sub(bytes.len());
                                    addr[start..].copy_from_slice(&bytes);
                                    result.push((name.clone(), AccountAddress(addr)));
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        name,
                                        hex_str,
                                        %e,
                                        "Move.toml: failed to parse named address, skipping"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    result
}

pub fn load_package_modules(package_dir: &Path) -> anyhow::Result<Vec<Vec<u8>>> {
    let artifact_dir = package_dir.join("nexus-artifact");
    if artifact_dir.exists() {
        return load_artifact_modules(&artifact_dir);
    }

    let build_dir = package_dir.join("build");
    if !build_dir.exists() {
        anyhow::bail!(
            "no nexus-artifact/ or build/ directory found in {}. Run `nexus-wallet move build` first.",
            package_dir.display()
        );
    }
    load_build_modules(&build_dir)
}

fn load_artifact_modules(artifact_dir: &Path) -> anyhow::Result<Vec<Vec<u8>>> {
    let bc_dir = artifact_dir.join("bytecode");
    if !bc_dir.exists() {
        anyhow::bail!("nexus-artifact/bytecode/ directory not found");
    }
    let mut modules = Vec::new();
    let mut entries: Vec<_> = std::fs::read_dir(&bc_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "mv") && e.path().is_file())
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        modules.push(std::fs::read(entry.path())?);
    }
    Ok(modules)
}

fn load_build_modules(build_dir: &Path) -> anyhow::Result<Vec<Vec<u8>>> {
    let mut modules = Vec::new();
    for pkg_entry in std::fs::read_dir(build_dir)? {
        let pkg_entry = pkg_entry?;
        if !pkg_entry.file_type()?.is_dir() {
            continue;
        }
        let bytecode_dir = pkg_entry.path().join("bytecode_modules");
        if !bytecode_dir.exists() {
            continue;
        }
        for entry in std::fs::read_dir(&bytecode_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "mv") && path.is_file() {
                modules.push(std::fs::read(&path)?);
            }
        }
    }
    Ok(modules)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    // ── UpgradePolicy serde ──────────────────────────────────────────

    #[test]
    fn upgrade_policy_serde_roundtrip() {
        for policy in [
            UpgradePolicy::Immutable,
            UpgradePolicy::Compatible,
            UpgradePolicy::GovernanceOnly,
        ] {
            let json = serde_json::to_string(&policy).unwrap();
            let back: UpgradePolicy = serde_json::from_str(&json).unwrap();
            assert_eq!(back, policy);
        }
    }

    // ── parse_upgrade_policy ─────────────────────────────────────────

    #[test]
    fn parse_upgrade_policy_defaults_to_immutable_for_missing_toml() {
        let dir = TempDir::new().unwrap();
        assert_eq!(parse_upgrade_policy(dir.path()), UpgradePolicy::Immutable);
    }

    #[test]
    fn parse_upgrade_policy_reads_compatible_from_toml() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("Move.toml"),
            "[package]\nname = \"test\"\nupgrade_policy = \"compatible\"\n",
        )
        .unwrap();
        assert_eq!(parse_upgrade_policy(dir.path()), UpgradePolicy::Compatible);
    }

    #[test]
    fn parse_upgrade_policy_reads_governance_only_from_toml() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("Move.toml"),
            "[package]\nname = \"test\"\nupgrade_policy = \"governance_only\"\n",
        )
        .unwrap();
        assert_eq!(
            parse_upgrade_policy(dir.path()),
            UpgradePolicy::GovernanceOnly
        );
    }

    #[test]
    fn parse_upgrade_policy_defaults_for_unknown_policy_string() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("Move.toml"),
            "[package]\nname = \"test\"\nupgrade_policy = \"exotic_future_value\"\n",
        )
        .unwrap();
        // Unknown values fall through to Immutable.
        assert_eq!(parse_upgrade_policy(dir.path()), UpgradePolicy::Immutable);
    }

    #[test]
    fn parse_upgrade_policy_defaults_when_field_absent() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("Move.toml"), "[package]\nname = \"test\"\n").unwrap();
        assert_eq!(parse_upgrade_policy(dir.path()), UpgradePolicy::Immutable);
    }

    // ── parse_named_addresses ────────────────────────────────────────

    #[test]
    fn parse_named_addresses_returns_empty_for_missing_toml() {
        let dir = TempDir::new().unwrap();
        let addrs = parse_named_addresses(dir.path());
        assert!(addrs.is_empty());
    }

    #[test]
    fn parse_named_addresses_parses_hex_address() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("Move.toml"),
            "[addresses]\ncounter = \"0x01\"\n",
        )
        .unwrap();
        let addrs = parse_named_addresses(dir.path());
        assert_eq!(addrs.len(), 1);
        assert_eq!(addrs[0].0, "counter");
        // 0x01 in a 32-byte array: last byte = 1, rest zeros.
        assert_eq!(addrs[0].1 .0[31], 1u8);
        assert!(addrs[0].1 .0[..31].iter().all(|&b| b == 0));
    }

    #[test]
    fn parse_named_addresses_skips_placeholder_underscore() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("Move.toml"),
            "[dev-addresses]\ncounter = \"_\"\n",
        )
        .unwrap();
        let addrs = parse_named_addresses(dir.path());
        assert!(addrs.is_empty());
    }

    #[test]
    fn parse_named_addresses_skips_invalid_hex() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("Move.toml"),
            "[addresses]\nbad = \"0xZZZZ\"\n",
        )
        .unwrap();
        // Should not panic; just skip the invalid entry.
        let addrs = parse_named_addresses(dir.path());
        assert!(addrs.is_empty());
    }

    #[test]
    fn parse_named_addresses_reads_dev_addresses_over_addresses() {
        let dir = TempDir::new().unwrap();
        // When both sections exist, parse_named_addresses reads dev-addresses
        // (or addresses as fallback). In TOML, both can coexist; our code tries
        // dev-addresses first then addresses.
        fs::write(
            dir.path().join("Move.toml"),
            "[addresses]\ncounter = \"0x01\"\n[dev-addresses]\ncounter = \"0x02\"\n",
        )
        .unwrap();
        let addrs = parse_named_addresses(dir.path());
        // dev-addresses takes priority
        assert_eq!(addrs.len(), 1);
        assert_eq!(addrs[0].1 .0[31], 2u8);
    }

    // ── collect_modules ──────────────────────────────────────────────

    #[test]
    fn collect_modules_returns_empty_for_nonexistent_dir() {
        let dir = TempDir::new().unwrap();
        let nonexistent = dir.path().join("bytecode_modules");
        let modules = collect_modules(&nonexistent).unwrap();
        assert!(modules.is_empty());
    }

    #[test]
    fn collect_modules_returns_sorted_by_name() {
        let dir = TempDir::new().unwrap();
        let bc_dir = dir.path().join("bytecode_modules");
        fs::create_dir(&bc_dir).unwrap();
        fs::write(bc_dir.join("zebra.mv"), b"ZZ").unwrap();
        fs::write(bc_dir.join("alpha.mv"), b"AA").unwrap();
        fs::write(bc_dir.join("middle.mv"), b"MM").unwrap();

        let modules = collect_modules(&bc_dir).unwrap();
        assert_eq!(modules.len(), 3);
        assert_eq!(modules[0].0, "alpha");
        assert_eq!(modules[1].0, "middle");
        assert_eq!(modules[2].0, "zebra");
    }

    #[test]
    fn collect_modules_ignores_non_mv_files() {
        let dir = TempDir::new().unwrap();
        let bc_dir = dir.path().join("bytecode_modules");
        fs::create_dir(&bc_dir).unwrap();
        fs::write(bc_dir.join("counter.mv"), b"MV").unwrap();
        fs::write(bc_dir.join("counter.json"), b"{}").unwrap();

        let modules = collect_modules(&bc_dir).unwrap();
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].0, "counter");
    }

    // ── PackageMetadata serde ────────────────────────────────────────

    #[test]
    fn package_metadata_bcs_roundtrip() {
        let meta = PackageMetadata {
            name: "my_pkg".to_string(),
            package_hash: [0xAB; 32],
            named_addresses: vec![("counter".to_string(), AccountAddress([1u8; 32]))],
            module_hashes: vec![("mod_a".to_string(), [0x11; 32])],
            abi_hash: [0u8; 32],
            upgrade_policy: UpgradePolicy::Compatible,
            deployer: AccountAddress([0x22; 32]),
            version: 3,
        };
        let bytes = bcs::to_bytes(&meta).unwrap();
        let back: PackageMetadata = bcs::from_bytes(&bytes).unwrap();
        assert_eq!(back, meta);
    }

    // ── load_package_modules ─────────────────────────────────────────

    #[test]
    fn load_package_modules_reads_from_nexus_artifact_bytecode() {
        let dir = TempDir::new().unwrap();
        let bc_dir = dir.path().join("nexus-artifact").join("bytecode");
        fs::create_dir_all(&bc_dir).unwrap();
        fs::write(bc_dir.join("foo.mv"), b"BYTES").unwrap();

        let modules = load_package_modules(dir.path()).unwrap();
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0], b"BYTES");
    }

    #[test]
    fn load_package_modules_falls_back_to_build_dir() {
        let dir = TempDir::new().unwrap();
        let pkg_dir = dir.path().join("build").join("my_package");
        let bc_dir = pkg_dir.join("bytecode_modules");
        fs::create_dir_all(&bc_dir).unwrap();
        fs::write(bc_dir.join("module_a.mv"), b"MODABYTES").unwrap();

        let modules = load_package_modules(dir.path()).unwrap();
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0], b"MODABYTES");
    }

    #[test]
    fn load_package_modules_errors_when_neither_dir_exists() {
        let dir = TempDir::new().unwrap();
        let result = load_package_modules(dir.path());
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("nexus-artifact") || msg.contains("build"));
    }

    #[test]
    fn load_package_modules_errors_when_nexus_artifact_has_no_bytecode_dir() {
        let dir = TempDir::new().unwrap();
        // Create nexus-artifact/ but no bytecode/ subdirectory.
        fs::create_dir(dir.path().join("nexus-artifact")).unwrap();
        let result = load_package_modules(dir.path());
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("bytecode"));
    }

    // ── generate() ──────────────────────────────────────────────────

    #[test]
    fn generate_errors_without_build_dir() {
        let dir = TempDir::new().unwrap();
        let err = generate(dir.path()).expect_err("should fail without build/");
        assert!(err.to_string().contains("no build/"));
    }

    #[test]
    fn generate_errors_without_bytecode_modules() {
        let dir = TempDir::new().unwrap();
        let pkg_dir = dir.path().join("build").join("my_pkg");
        fs::create_dir_all(&pkg_dir).unwrap();
        // No bytecode_modules inside
        fs::write(
            dir.path().join("Move.toml"),
            "[package]\nname = \"my_pkg\"\n",
        )
        .unwrap();
        let err = generate(dir.path()).expect_err("should fail without .mv");
        assert!(err.to_string().contains("no .mv"));
    }

    #[test]
    fn generate_success_with_bytecode() {
        let dir = TempDir::new().unwrap();
        let pkg_dir = dir.path().join("build").join("my_pkg");
        let bc_dir = pkg_dir.join("bytecode_modules");
        fs::create_dir_all(&bc_dir).unwrap();
        fs::write(bc_dir.join("counter.mv"), b"fake bytecode").unwrap();
        fs::write(
            dir.path().join("Move.toml"),
            "[package]\nname = \"my_pkg\"\nupgrade_policy = \"compatible\"\n[addresses]\ncounter = \"0x01\"\n",
        )
        .unwrap();

        let result = generate(dir.path()).unwrap();
        assert_eq!(result.module_count, 1);
        assert!(result.total_bytes > 0);
        assert!(result.artifact_dir.join("manifest.json").exists());
        assert!(result.artifact_dir.join("package-metadata.bcs").exists());
        assert!(result.artifact_dir.join("bytecode").join("counter.mv").exists());
        assert_eq!(result.metadata.upgrade_policy, UpgradePolicy::Compatible);
        assert_eq!(result.metadata.named_addresses.len(), 1);
    }

    #[test]
    fn find_package_build_fallback_to_first_dir_with_bytecode() {
        let dir = TempDir::new().unwrap();
        let build_dir = dir.path().join("build");
        let pkg_dir = build_dir.join("fallback_pkg");
        let bc_dir = pkg_dir.join("bytecode_modules");
        fs::create_dir_all(&bc_dir).unwrap();
        fs::write(bc_dir.join("mod.mv"), b"bytes").unwrap();
        // No Move.toml — should fall back to directory scan
        let (name, path) = find_package_build(&build_dir).unwrap();
        assert_eq!(name, "fallback_pkg");
        assert_eq!(path, pkg_dir);
    }

    #[test]
    fn find_package_build_errors_when_empty() {
        let dir = TempDir::new().unwrap();
        let build_dir = dir.path().join("build");
        fs::create_dir(&build_dir).unwrap();
        let result = find_package_build(&build_dir);
        assert!(result.is_err());
    }

    // ── load_build_modules ──────────────────────────────────────────

    #[test]
    fn load_build_modules_ignores_non_dir_entries() {
        let dir = TempDir::new().unwrap();
        let build_dir = dir.path().join("build");
        fs::create_dir(&build_dir).unwrap();
        // Create a file instead of a directory
        fs::write(build_dir.join("not_a_dir"), b"file").unwrap();
        let modules = load_build_modules(&build_dir).unwrap();
        assert!(modules.is_empty());
    }

    #[test]
    fn load_artifact_modules_sorted_order() {
        let dir = TempDir::new().unwrap();
        let bc_dir = dir.path().join("bytecode");
        fs::create_dir(&bc_dir).unwrap();
        fs::write(bc_dir.join("z_mod.mv"), b"Z").unwrap();
        fs::write(bc_dir.join("a_mod.mv"), b"A").unwrap();
        let modules = load_artifact_modules(dir.path()).unwrap();
        assert_eq!(modules.len(), 2);
        // Should be sorted by filename
        assert_eq!(modules[0], b"A");
        assert_eq!(modules[1], b"Z");
    }
}
