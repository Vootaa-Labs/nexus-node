//! CLI command definitions for nexus-wallet.

pub mod address;
pub mod balance;
pub mod faucet;
pub mod status;
pub mod transfer;
pub mod verify_anchor;

use clap::{Parser, Subcommand};

pub use crate::move_tooling::commands::MoveCommands;

/// Nexus developer wallet CLI.
#[derive(Parser)]
#[command(
    name = "nexus-wallet",
    version,
    about = "Developer wallet CLI for Nexus"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

/// Top-level subcommands.
#[derive(Subcommand)]
pub enum Commands {
    /// Derive and display AccountAddress from a Dilithium key file.
    Address(address::AddressArgs),
    /// Query native token balance for an address.
    Balance(balance::BalanceArgs),
    /// Send native tokens to a recipient.
    Transfer(transfer::TransferArgs),
    /// Check transaction status by digest.
    Status(status::StatusArgs),
    /// Request test tokens on devnet (dev-mode only).
    Faucet(faucet::FaucetArgs),
    /// Verify a provenance anchor receipt.
    VerifyAnchor(verify_anchor::VerifyAnchorArgs),
    /// Move package, contract, and script operations.
    Move {
        #[command(subcommand)]
        command: MoveCommands,
    },
}
