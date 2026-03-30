// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

use clap::Args;
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct BuildArgs {
    #[arg(short, long, default_value = ".")]
    pub package_dir: PathBuf,

    #[arg(long, value_delimiter = ',')]
    pub named_addresses: Vec<String>,

    #[arg(long, default_value_t = false)]
    pub skip_fetch: bool,

    /// Write the compiled nexus-artifact/ into this directory instead of
    /// <package-dir>/nexus-artifact/.  The build runs in an isolated temp
    /// copy of the package so the original tree is never modified.
    ///
    /// Use this in CI / smoke tests to prevent overwriting git-tracked test
    /// fixture bytecode that encodes dev-addresses (e.g. 0xCAFE).
    #[arg(long)]
    pub output_dir: Option<PathBuf>,
}

pub fn run(args: BuildArgs) -> anyhow::Result<()> {
    let named_addrs: Vec<String> = args.named_addresses.clone();
    let parsed_addrs = nexus_move_package::parse_named_address_assignments(&named_addrs)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    tracing::info!(
        package_dir = %args.package_dir.display(),
        "compiling Move package"
    );

    // When --output-dir is given we build inside a temporary copy of the
    // package so the original nexus-artifact/ (which may be a git-tracked
    // test fixture with dev-addresses baked in) is never touched.
    let (artifact_dir, module_count, total_bytes, _tmpdir) = if let Some(ref out) = args.output_dir
    {
        let tmp =
            tempfile::tempdir().map_err(|e| anyhow::anyhow!("failed to create temp dir: {e}"))?;
        let tmp_pkg = tmp.path();

        // Copy Move.toml and sources/ into the isolated build workspace.
        std::fs::copy(
            args.package_dir.join("Move.toml"),
            tmp_pkg.join("Move.toml"),
        )?;
        let src_sources = args.package_dir.join("sources");
        if src_sources.exists() {
            copy_dir_all(&src_sources, &tmp_pkg.join("sources"))?;
        }

        let result = nexus_move_package::build::build_package(tmp_pkg, &parsed_addrs, None)
            .map_err(|e| anyhow::anyhow!("Move build failed: {e}"))?;

        // Move compiled artifacts from the temp tree to out/nexus-artifact/.
        let dest = out.join("nexus-artifact");
        if dest.exists() {
            std::fs::remove_dir_all(&dest)?;
        }
        std::fs::create_dir_all(out)?;
        copy_dir_all(&result.artifact_dir, &dest)?;

        (dest, result.module_count, result.total_bytes, Some(tmp))
    } else {
        let result =
            nexus_move_package::build::build_package(&args.package_dir, &parsed_addrs, None)
                .map_err(|e| anyhow::anyhow!("Move build failed: {e}"))?;
        (
            result.artifact_dir,
            result.module_count,
            result.total_bytes,
            None,
        )
    };

    tracing::info!(modules = module_count, "build succeeded");
    tracing::info!(
        artifact_dir = %artifact_dir.display(),
        modules = module_count,
        total_bytes,
        "nexus artifact generated"
    );

    println!(
        "Nexus artifact: {} ({} module(s), {} bytes)",
        artifact_dir.display(),
        module_count,
        total_bytes,
    );

    Ok(())
}

/// Recursively copy a directory tree from `src` to `dst`.
fn copy_dir_all(src: &Path, dst: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dest_path = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &dest_path)?;
        } else {
            std::fs::copy(entry.path(), dest_path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn copy_dir_all_copies_flat_files() {
        let src = TempDir::new().unwrap();
        fs::write(src.path().join("a.txt"), b"hello").unwrap();
        fs::write(src.path().join("b.txt"), b"world").unwrap();

        let dst = TempDir::new().unwrap();
        copy_dir_all(src.path(), dst.path()).unwrap();

        assert_eq!(fs::read(dst.path().join("a.txt")).unwrap(), b"hello");
        assert_eq!(fs::read(dst.path().join("b.txt")).unwrap(), b"world");
    }

    #[test]
    fn copy_dir_all_creates_destination_directory() {
        let src = TempDir::new().unwrap();
        fs::write(src.path().join("file.mv"), b"bytecode").unwrap();

        let base = TempDir::new().unwrap();
        let dst = base.path().join("new_output").join("nested");
        // dst does not yet exist.
        copy_dir_all(src.path(), &dst).unwrap();

        assert!(dst.join("file.mv").exists());
    }

    #[test]
    fn copy_dir_all_recurses_into_subdirs() {
        let src = TempDir::new().unwrap();
        let subdir = src.path().join("subdir");
        fs::create_dir(&subdir).unwrap();
        fs::write(subdir.join("module.mv"), b"mv_bytes").unwrap();
        fs::write(src.path().join("top.mv"), b"top_bytes").unwrap();

        let dst = TempDir::new().unwrap();
        copy_dir_all(src.path(), dst.path()).unwrap();

        assert_eq!(fs::read(dst.path().join("top.mv")).unwrap(), b"top_bytes");
        assert_eq!(
            fs::read(dst.path().join("subdir").join("module.mv")).unwrap(),
            b"mv_bytes"
        );
    }

    #[test]
    fn copy_dir_all_empty_source_creates_empty_dest() {
        let src = TempDir::new().unwrap();
        let dst = TempDir::new().unwrap();
        copy_dir_all(src.path(), dst.path()).unwrap();
        // Destination should exist but be empty.
        assert!(dst.path().exists());
        assert_eq!(fs::read_dir(dst.path()).unwrap().count(), 0);
    }
}
