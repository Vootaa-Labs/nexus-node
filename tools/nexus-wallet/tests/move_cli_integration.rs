// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for the unified `nexus-wallet move ...` developer CLI.
//!
//! These tests verify that the wallet entrypoint now exposes the full Move
//! workflow and preserves the legacy contract-tooling behavior.

use std::path::{Path, PathBuf};
use std::process::Command;

fn nexus_wallet_bin() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_BIN_EXE_nexus-wallet"));
    assert!(
        path.exists(),
        "nexus-wallet binary not found at {}",
        path.display()
    );
    path
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

fn copy_example_contract(name: &str, tmp: &Path) -> PathBuf {
    let src = workspace_root().join("contracts/examples").join(name);
    let dst = tmp.join(name);
    copy_dir_recursive(&src, &dst).unwrap();
    let _ = std::fs::remove_dir_all(dst.join("build"));
    let _ = std::fs::remove_dir_all(dst.join("nexus-artifact"));

    dst
}

fn run_wallet_move(args: &[&str]) -> std::process::Output {
    Command::new(nexus_wallet_bin())
        .arg("move")
        .args(args)
        .output()
        .expect("failed to execute nexus-wallet move")
}

fn assert_artifact(package_dir: &Path, expected_pkg_name: &str) {
    let artifact_dir = package_dir.join("nexus-artifact");
    assert!(artifact_dir.exists(), "nexus-artifact/ not created");

    let manifest_path = artifact_dir.join("manifest.json");
    assert!(manifest_path.exists(), "manifest.json not created");
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
    assert_eq!(manifest["package_name"], expected_pkg_name);
    assert!(manifest["module_count"].as_u64().unwrap() >= 1);
    assert!(manifest["total_bytecode_bytes"].as_u64().unwrap() > 0);

    let meta_path = artifact_dir.join("package-metadata.bcs");
    assert!(meta_path.exists(), "package-metadata.bcs not created");
    assert!(std::fs::metadata(&meta_path).unwrap().len() > 0);

    let bc_dir = artifact_dir.join("bytecode");
    assert!(bc_dir.exists(), "bytecode/ not created");
    let mv_count = std::fs::read_dir(&bc_dir)
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .ok()
                .and_then(|e| e.path().extension().map(|ext| ext == "mv"))
                .unwrap_or(false)
        })
        .count();
    assert_eq!(
        mv_count,
        manifest["module_count"].as_u64().unwrap() as usize,
        "bytecode .mv file count doesn't match manifest"
    );
}

#[test]
fn move_help_exits_ok() {
    let output = run_wallet_move(&["--help"]);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Move package, contract, and script operations"));
    assert!(stdout.contains("build"));
    assert!(stdout.contains("deploy"));
    assert!(stdout.contains("query"));
}

#[test]
fn move_build_counter_contract() {
    let tmp = tempfile::tempdir().unwrap();
    let pkg = copy_example_contract("counter", tmp.path());

    let output = run_wallet_move(&[
        "build",
        "--package-dir",
        pkg.to_str().unwrap(),
        "--named-addresses",
        "counter_addr=0xCAFE",
        "--skip-fetch",
    ]);

    assert!(
        output.status.success(),
        "wallet move build failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert_artifact(&pkg, "counter");
}

#[test]
fn move_build_token_contract() {
    let tmp = tempfile::tempdir().unwrap();
    let pkg = copy_example_contract("token", tmp.path());

    let output = run_wallet_move(&[
        "build",
        "--package-dir",
        pkg.to_str().unwrap(),
        "--named-addresses",
        "token_addr=0xCAFE",
        "--skip-fetch",
    ]);

    assert!(output.status.success());
    assert_artifact(&pkg, "token");
}

#[test]
fn move_build_escrow_contract() {
    let tmp = tempfile::tempdir().unwrap();
    let pkg = copy_example_contract("escrow", tmp.path());

    let output = run_wallet_move(&[
        "build",
        "--package-dir",
        pkg.to_str().unwrap(),
        "--named-addresses",
        "escrow_addr=0xCAFE",
        "--skip-fetch",
    ]);

    assert!(output.status.success());
    assert_artifact(&pkg, "escrow");
}

#[test]
fn move_build_missing_named_address_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let pkg = copy_example_contract("counter", tmp.path());

    // Remove dev-addresses so counter_addr is truly unresolved.
    let toml_path = pkg.join("Move.toml");
    let content = std::fs::read_to_string(&toml_path).unwrap();
    let patched = content
        .lines()
        .filter(|l| {
            let section = l.trim();
            section != "[dev-addresses]" && !section.starts_with("counter_addr")
                || section.starts_with("counter_addr = \"_\"")
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&toml_path, patched).unwrap();

    let output = run_wallet_move(&[
        "build",
        "--package-dir",
        pkg.to_str().unwrap(),
        "--named-addresses",
        "wrong_name=0xCAFE",
        "--skip-fetch",
    ]);

    assert!(!output.status.success());
}

#[test]
fn move_deploy_without_build_dir_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let output = run_wallet_move(&[
        "deploy",
        "--package-dir",
        tmp.path().to_str().unwrap(),
        "--rpc-url",
        "http://127.0.0.1:19999",
    ]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no nexus-artifact/ or build/ directory found"));
}

#[test]
fn move_deploy_help_exits_ok() {
    let output = run_wallet_move(&["deploy", "--help"]);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Deploy compiled modules"));
}

#[test]
fn move_call_help_exits_ok() {
    let output = run_wallet_move(&["call", "--help"]);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("entry function"));
}

#[test]
fn move_query_help_exits_ok() {
    let output = run_wallet_move(&["query", "--help"]);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("view function"));
}

#[test]
fn move_deploy_empty_bytecode_dir_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let build_dir = tmp.path().join("build").join("dummy");
    std::fs::create_dir_all(build_dir.join("bytecode_modules")).unwrap();

    let output = run_wallet_move(&[
        "deploy",
        "--package-dir",
        tmp.path().to_str().unwrap(),
        "--rpc-url",
        "http://127.0.0.1:19999",
    ]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no .mv bytecode"));
}

#[test]
fn move_deploy_artifact_without_key_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let pkg = copy_example_contract("counter", tmp.path());

    let build_ok = run_wallet_move(&[
        "build",
        "--package-dir",
        pkg.to_str().unwrap(),
        "--named-addresses",
        "counter_addr=0xCAFE",
        "--skip-fetch",
    ]);
    assert!(build_ok.status.success());

    let output = run_wallet_move(&[
        "deploy",
        "--package-dir",
        pkg.to_str().unwrap(),
        "--rpc-url",
        "http://127.0.0.1:19999",
    ]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--key-file is required for deploy operations"));
}

#[test]
fn move_call_missing_required_args_fails() {
    let output = run_wallet_move(&["call", "--rpc-url", "http://127.0.0.1:19999"]);
    assert!(!output.status.success());
}

#[test]
fn move_call_invalid_hex_args_fails() {
    let output = run_wallet_move(&[
        "call",
        "--contract",
        "0x00000000000000000000000000000000000000000000000000000000000000ab",
        "--function",
        "counter::increment",
        "--args",
        "not_valid_hex",
        "--rpc-url",
        "http://127.0.0.1:19999",
    ]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("invalid hex"));
}

#[test]
fn move_call_to_unreachable_node_fails() {
    let output = run_wallet_move(&[
        "call",
        "--contract",
        "0x00000000000000000000000000000000000000000000000000000000000000ab",
        "--function",
        "counter::increment",
        "--rpc-url",
        "http://127.0.0.1:19999",
    ]);

    assert!(!output.status.success());
}

#[test]
fn move_query_missing_required_args_fails() {
    let output = run_wallet_move(&["query", "--rpc-url", "http://127.0.0.1:19999"]);
    assert!(!output.status.success());
}

#[test]
fn move_query_to_unreachable_node_fails() {
    let output = run_wallet_move(&[
        "query",
        "--contract",
        "0x00000000000000000000000000000000000000000000000000000000000000ab",
        "--function",
        "counter::get_count",
        "--rpc-url",
        "http://127.0.0.1:19999",
    ]);

    assert!(!output.status.success());
}
