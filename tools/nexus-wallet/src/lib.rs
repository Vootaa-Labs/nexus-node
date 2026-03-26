pub mod commands;
pub mod move_tooling;

pub use move_tooling::artifact;
pub use move_tooling::rpc_client;

use clap::Parser;
use commands::{Cli, Commands};

pub fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
}

pub fn run_wallet_cli() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Address(args) => commands::address::run(args),
        Commands::Balance(args) => commands::balance::run(args),
        Commands::Transfer(args) => commands::transfer::run(args),
        Commands::Status(args) => commands::status::run(args),
        Commands::Faucet(args) => commands::faucet::run(args),
        Commands::VerifyAnchor(args) => commands::verify_anchor::run(args),
        Commands::Move { command } => move_tooling::commands::run(command),
    }
}
