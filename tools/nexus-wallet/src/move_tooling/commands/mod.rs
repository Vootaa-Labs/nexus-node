// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

pub mod balance;
pub mod build;
pub mod call;
pub mod deploy;
pub mod dry_run;
pub mod faucet;
pub mod inspect;
pub mod query;
pub mod script;
pub mod upgrade;

use clap::Subcommand;

#[derive(Subcommand)]
pub enum MoveCommands {
    /// Compile a Move package.
    Build(build::BuildArgs),
    /// Inspect a compiled Move package (ABI, metadata, modules).
    Inspect(inspect::InspectArgs),
    /// Deploy compiled modules to a Nexus node.
    Deploy(deploy::DeployArgs),
    /// Invoke an entry function on a deployed contract.
    Call(call::CallArgs),
    /// Query a view function or resource.
    Query(query::QueryArgs),
    /// Request test tokens from the devnet faucet.
    Faucet(faucet::FaucetArgs),
    /// Query native token balance for an account.
    Balance(balance::BalanceArgs),
    /// Upgrade a previously deployed Move contract.
    Upgrade(upgrade::UpgradeArgs),
    /// Dry-run a Move call without committing state.
    DryRun(dry_run::DryRunArgs),
    /// Execute a compiled Move script.
    Script(script::ScriptArgs),
}

pub fn run(command: MoveCommands) -> anyhow::Result<()> {
    match command {
        MoveCommands::Build(args) => build::run(args),
        MoveCommands::Inspect(args) => inspect::run(args),
        MoveCommands::Deploy(args) => deploy::run(args),
        MoveCommands::Call(args) => call::run(args),
        MoveCommands::Query(args) => query::run(args),
        MoveCommands::Faucet(args) => faucet::run(args),
        MoveCommands::Balance(args) => balance::run(args),
        MoveCommands::Upgrade(args) => upgrade::run(args),
        MoveCommands::DryRun(args) => dry_run::run(args),
        MoveCommands::Script(args) => script::run(args),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Helper: a trivially invalid RPC URL that fails validate_rpc_url.
    fn bad_url() -> String {
        "ws://bad".into()
    }

    #[test]
    fn dispatch_query_rejects_bad_url() {
        let cmd = MoveCommands::Query(query::QueryArgs {
            contract: hex::encode([0u8; 32]),
            function: "f::g".into(),
            args: vec![],
            rpc_url: bad_url(),
        });
        assert!(run(cmd).is_err());
    }

    #[test]
    fn dispatch_call_rejects_bad_url() {
        let cmd = MoveCommands::Call(call::CallArgs {
            contract: hex::encode([0u8; 32]),
            function: "f::g".into(),
            args: vec![],
            type_args: vec![],
            rpc_url: bad_url(),
            key_file: None,
            gas_limit: 100_000,
            nonce: 0,
            poll_attempts: 1,
        });
        assert!(run(cmd).is_err());
    }

    #[test]
    fn dispatch_dry_run_rejects_bad_url() {
        let cmd = MoveCommands::DryRun(dry_run::DryRunArgs {
            contract: hex::encode([0u8; 32]),
            function: "f::g".into(),
            type_args: vec![],
            args: vec![],
            key_file: None,
            gas_limit: 1_000_000,
            rpc_url: bad_url(),
        });
        assert!(run(cmd).is_err());
    }

    #[test]
    fn dispatch_faucet_rejects_bad_url() {
        let cmd = MoveCommands::Faucet(faucet::FaucetArgs {
            key_file: None,
            address: None,
            rpc_url: bad_url(),
        });
        assert!(run(cmd).is_err());
    }

    #[test]
    fn dispatch_balance_rejects_bad_url() {
        let cmd = MoveCommands::Balance(balance::BalanceArgs {
            address: Some(hex::encode([0u8; 32])),
            key_file: None,
            rpc_url: bad_url(),
        });
        assert!(run(cmd).is_err());
    }

    #[test]
    fn dispatch_upgrade_rejects_bad_url() {
        let dir = tempfile::TempDir::new().unwrap();
        let cmd = MoveCommands::Upgrade(upgrade::UpgradeArgs {
            package: dir.path().to_path_buf(),
            contract: hex::encode([0u8; 32]),
            key_file: None,
            gas_limit: 2_000_000,
            rpc_url: bad_url(),
        });
        assert!(run(cmd).is_err());
    }

    #[test]
    fn dispatch_deploy_rejects_bad_url() {
        let dir = tempfile::TempDir::new().unwrap();
        let cmd = MoveCommands::Deploy(deploy::DeployArgs {
            package_dir: dir.path().to_path_buf(),
            rpc_url: bad_url(),
            key_file: None,
            gas_limit: 500_000,
            nonce: 0,
            poll_attempts: 1,
        });
        assert!(run(cmd).is_err());
    }

    #[test]
    fn dispatch_script_rejects_bad_url() {
        let dir = tempfile::TempDir::new().unwrap();
        let script = dir.path().join("test.mv");
        std::fs::write(&script, b"bytecode").unwrap();
        let cmd = MoveCommands::Script(script::ScriptArgs {
            script_file: script,
            type_args: vec![],
            args: vec![],
            key_file: None,
            gas_limit: 1_000_000,
            rpc_url: bad_url(),
        });
        assert!(run(cmd).is_err());
    }

    #[test]
    fn dispatch_inspect_errors_on_empty_dir() {
        let dir = tempfile::TempDir::new().unwrap();
        let cmd = MoveCommands::Inspect(inspect::InspectArgs {
            package_dir: dir.path().to_path_buf(),
            verbose: false,
        });
        // inspect looks for build/ dir which doesn't exist → error
        assert!(run(cmd).is_err());
    }

    #[test]
    fn dispatch_build_errors_on_nonexistent_package() {
        let cmd = MoveCommands::Build(build::BuildArgs {
            package_dir: PathBuf::from("/nonexistent/package"),
            named_addresses: vec![],
            skip_fetch: false,
            output_dir: None,
        });
        assert!(run(cmd).is_err());
    }
}
