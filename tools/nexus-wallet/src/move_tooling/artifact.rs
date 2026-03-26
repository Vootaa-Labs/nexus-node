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
