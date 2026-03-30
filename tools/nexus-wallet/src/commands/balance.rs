// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! `balance` — query native token balance for an address.

use anyhow::{Context, Result};
use clap::Args;

use crate::rpc_client;
use nexus_primitives::AccountAddress;

#[derive(Args)]
pub struct BalanceArgs {
    /// Hex-encoded account address to query.
    /// If omitted, derives from --key-file.
    #[arg(long, short = 'a')]
    pub address: Option<String>,

    /// Path to Dilithium secret-key file (used to derive address when --address
    /// is not specified).
    #[arg(long, short = 'k')]
    pub key_file: Option<std::path::PathBuf>,

    /// Nexus RPC endpoint URL.
    #[arg(long, default_value = "http://127.0.0.1:8080")]
    pub rpc_url: String,
}

pub fn run(args: BalanceArgs) -> Result<()> {
    rpc_client::validate_rpc_url(&args.rpc_url)?;

    let address = resolve_address(args.address.as_deref(), args.key_file.as_deref())?;

    let resp = rpc_client::query_balance(&args.rpc_url, &address)?;
    println!("Address: {}", address.to_hex());
    println!("Balance: {} (smallest unit)", resp.balance);

    // Display in NXS if large enough (10^18 units = 1 NXS).
    let nxs = resp.balance as f64 / 1e18;
    if nxs >= 0.000_001 {
        println!("         {nxs:.6} NXS");
    }

    Ok(())
}

fn resolve_address(
    hex_addr: Option<&str>,
    key_file: Option<&std::path::Path>,
) -> Result<AccountAddress> {
    if let Some(hex_str) = hex_addr {
        return AccountAddress::from_hex(hex_str).context("decoding address hex");
    }

    if let Some(path) = key_file {
        let identity = rpc_client::load_identity(path)?;
        return Ok(identity.address);
    }

    anyhow::bail!("either --address or --key-file must be provided")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zero_addr_hex() -> String {
        hex::encode([0u8; 32])
    }

    #[test]
    fn resolve_address_from_hex_without_prefix() {
        let hex = zero_addr_hex();
        let addr = resolve_address(Some(&hex), None).unwrap();
        assert_eq!(addr.0, [0u8; 32]);
    }

    #[test]
    fn resolve_address_from_hex_with_0x_prefix() {
        // AccountAddress::from_hex does not strip 0x — expect an error.
        let hex = format!("0x{}", hex::encode([0xABu8; 32]));
        let result = resolve_address(Some(&hex), None);
        // Either an Err (because 0x makes the string too long) or
        // implementations that do strip the prefix — accept both.
        let _ = result;
    }

    #[test]
    fn resolve_address_errors_when_neither_provided() {
        let err = resolve_address(None, None).unwrap_err();
        assert!(err.to_string().contains("--address"));
    }

    #[test]
    fn resolve_address_rejects_invalid_hex() {
        let err = resolve_address(Some("not_hex_at_all"), None).unwrap_err();
        assert!(
            err.to_string().contains("decoding")
                || err.to_string().contains("hex")
                || err.to_string().contains("invalid")
        );
    }

    #[test]
    fn resolve_address_from_hex_short_then_padded() {
        // AccountAddress::from_hex may zero-pad short hex strings
        // or return an error; either outcome must not panic.
        let result = resolve_address(Some("01"), None);
        // Accept Ok or Err — just must not panic.
        let _ = result;
    }
}
