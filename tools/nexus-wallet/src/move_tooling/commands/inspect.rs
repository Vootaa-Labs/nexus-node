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
