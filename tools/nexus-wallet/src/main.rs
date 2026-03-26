//! `nexus-wallet` — developer wallet CLI for Nexus.
//!
//! Subcommands:
//! - `address`  — derive and display AccountAddress from a key file
//! - `balance`  — query native token balance for an address
//! - `transfer` — send native tokens from the loaded wallet to a recipient
//! - `status`   — check transaction receipt by digest
//! - `faucet`   — request test tokens on devnet (dev-mode only)
//! - `move ...` — build, inspect, deploy, call, query, upgrade, and dry-run Move packages

fn main() -> anyhow::Result<()> {
    nexus_wallet::init_tracing();
    nexus_wallet::run_wallet_cli()
}
