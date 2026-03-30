// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

use crate::artifact;
use clap::Args;
use std::path::PathBuf;

#[derive(Args)]
pub struct InspectArgs {
    #[arg(short, long, default_value = ".")]
    pub package_dir: PathBuf,

    #[arg(long, default_value_t = false)]
    pub verbose: bool,
}

pub fn run(args: InspectArgs) -> anyhow::Result<()> {
    let build_dir = args.package_dir.join("build");
    if !build_dir.exists() {
        anyhow::bail!(
            "no build/ directory found in {}. Run `nexus-wallet move build` first.",
            args.package_dir.display()
        );
    }

    let package_name = find_package_dir(&build_dir)?;
    let pkg_dir = build_dir.join(&package_name);
    let bytecode_dir = pkg_dir.join("bytecode_modules");
    if bytecode_dir.exists() {
        println!("=== Package: {} ===\n", package_name);
        list_modules(&bytecode_dir, args.verbose)?;
    } else {
        println!("No bytecode_modules found for package '{}'", package_name);
    }

    let meta_path = args.package_dir.join("nexus-artifact/package-metadata.bcs");
    if meta_path.exists() {
        let bcs_bytes = std::fs::read(&meta_path)?;
        let meta: artifact::PackageMetadata = bcs::from_bytes(&bcs_bytes)?;
        println!("=== Nexus Package Metadata ===\n");
        println!("  Name:           {}", meta.name);
        println!("  Package hash:   {}", hex::encode(meta.package_hash));
        println!("  Upgrade policy: {:?}", meta.upgrade_policy);
        println!("  Version:        {}", meta.version);
        println!("  Modules:        {}", meta.module_hashes.len());
        for (name, hash) in &meta.module_hashes {
            println!("    {name:24} blake3:{}", hex::encode(&hash[..8]));
        }
        if !meta.named_addresses.is_empty() {
            println!("  Named addresses:");
            for (name, addr) in &meta.named_addresses {
                println!("    {name} = 0x{}", hex::encode(addr.0));
            }
        }
        println!();
    }

    Ok(())
}

fn find_package_dir(build_dir: &PathBuf) -> anyhow::Result<String> {
    for entry in std::fs::read_dir(build_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            if let Some(name) = entry.file_name().to_str() {
                return Ok(name.to_string());
            }
        }
    }
    anyhow::bail!("no package directory found in build/")
}

fn list_modules(bytecode_dir: &PathBuf, verbose: bool) -> anyhow::Result<()> {
    let mut modules = Vec::new();
    for entry in std::fs::read_dir(bytecode_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "mv") && path.is_file() {
            modules.push(path);
        }
    }

    let deps_dir = bytecode_dir.join("dependencies");
    let mut dep_count = 0u32;
    if deps_dir.exists() {
        for entry in std::fs::read_dir(&deps_dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                dep_count += count_mv_files(&entry.path());
            }
        }
    }

    modules.sort();

    println!("Modules ({}):", modules.len());
    for path in &modules {
        let name = path.file_stem().unwrap_or_default().to_string_lossy();
        let size = std::fs::metadata(path)?.len();
        let hash = blake3::hash(&std::fs::read(path)?);
        println!(
            "  {name:24} {size:>6} bytes  blake3:{}",
            hex::encode(&hash.as_bytes()[..8])
        );

        if verbose {
            let bytes = std::fs::read(path)?;
            println!("    hex: {}", hex::encode(&bytes));
        }
    }

    if dep_count > 0 {
        println!("\nDependency modules: {dep_count}");
    }

    println!();
    Ok(())
}

fn count_mv_files(dir: &PathBuf) -> u32 {
    let mut count = 0;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.path().extension().is_some_and(|e| e == "mv") {
                count += 1;
            }
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    // ── find_package_dir ───────────────────────────────────────────

    #[test]
    fn find_package_dir_finds_first_subdirectory() {
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join("MyPackage")).unwrap();
        let name = find_package_dir(&dir.path().to_path_buf()).unwrap();
        assert_eq!(name, "MyPackage");
    }

    #[test]
    fn find_package_dir_fails_on_directory_with_only_files() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("not_a_dir.txt"), b"content").unwrap();
        // No subdirectory → should error.
        assert!(find_package_dir(&dir.path().to_path_buf()).is_err());
    }

    #[test]
    fn find_package_dir_fails_on_empty_directory() {
        let dir = TempDir::new().unwrap();
        assert!(find_package_dir(&dir.path().to_path_buf()).is_err());
    }

    // ── count_mv_files ─────────────────────────────────────────────

    #[test]
    fn count_mv_files_counts_only_mv_extension() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("alpha.mv"), b"mv1").unwrap();
        fs::write(dir.path().join("beta.mv"), b"mv2").unwrap();
        fs::write(dir.path().join("gamma.json"), b"{}").unwrap();
        fs::write(dir.path().join("delta.txt"), b"x").unwrap();

        let count = count_mv_files(&dir.path().to_path_buf());
        assert_eq!(count, 2);
    }

    #[test]
    fn count_mv_files_returns_zero_for_empty_directory() {
        let dir = TempDir::new().unwrap();
        assert_eq!(count_mv_files(&dir.path().to_path_buf()), 0);
    }

    #[test]
    fn count_mv_files_returns_zero_for_nonexistent_directory() {
        let nonexistent = PathBuf::from("/nonexistent/path/xyz");
        assert_eq!(count_mv_files(&nonexistent), 0);
    }

    #[test]
    fn count_mv_files_returns_correct_count() {
        let dir = TempDir::new().unwrap();
        for i in 0..5 {
            fs::write(dir.path().join(format!("m{i}.mv")), b"x").unwrap();
        }
        assert_eq!(count_mv_files(&dir.path().to_path_buf()), 5);
    }

    // ── list_modules ───────────────────────────────────────────────

    #[test]
    fn list_modules_with_mv_files() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("alpha.mv"), b"code1").unwrap();
        fs::write(dir.path().join("beta.mv"), b"code2").unwrap();
        fs::write(dir.path().join("readme.txt"), b"skip").unwrap();
        let result = list_modules(&dir.path().to_path_buf(), false);
        assert!(result.is_ok());
    }

    #[test]
    fn list_modules_verbose_mode() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("m.mv"), b"\xDE\xAD").unwrap();
        let result = list_modules(&dir.path().to_path_buf(), true);
        assert!(result.is_ok());
    }

    #[test]
    fn list_modules_with_dependencies_dir() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("main.mv"), b"main").unwrap();
        let deps = dir.path().join("dependencies").join("framework");
        fs::create_dir_all(&deps).unwrap();
        fs::write(deps.join("dep.mv"), b"dep-code").unwrap();
        let result = list_modules(&dir.path().to_path_buf(), false);
        assert!(result.is_ok());
    }

    #[test]
    fn list_modules_empty_dir() {
        let dir = TempDir::new().unwrap();
        let result = list_modules(&dir.path().to_path_buf(), false);
        assert!(result.is_ok());
    }

    // ── run (integration-style) ───────────────────────────────────

    #[test]
    fn run_errors_without_build_dir() {
        let dir = TempDir::new().unwrap();
        let args = InspectArgs {
            package_dir: dir.path().to_path_buf(),
            verbose: false,
        };
        let err = run(args).unwrap_err().to_string();
        assert!(err.contains("no build/"));
    }

    #[test]
    fn run_succeeds_with_build_dir_and_bytecode() {
        let dir = TempDir::new().unwrap();
        let pkg = dir.path().join("build").join("my_pkg");
        let bc = pkg.join("bytecode_modules");
        fs::create_dir_all(&bc).unwrap();
        fs::write(bc.join("test.mv"), b"bytecode").unwrap();
        let args = InspectArgs {
            package_dir: dir.path().to_path_buf(),
            verbose: false,
        };
        assert!(run(args).is_ok());
    }

    #[test]
    fn run_prints_no_bytecode_message_when_missing() {
        let dir = TempDir::new().unwrap();
        let pkg = dir.path().join("build").join("my_pkg");
        fs::create_dir_all(&pkg).unwrap();
        // No bytecode_modules dir inside
        let args = InspectArgs {
            package_dir: dir.path().to_path_buf(),
            verbose: false,
        };
        // Should succeed (prints warning, doesn't error)
        assert!(run(args).is_ok());
    }
}
