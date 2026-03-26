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
