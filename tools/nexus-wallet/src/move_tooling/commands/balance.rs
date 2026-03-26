// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

use anyhow::{Context, Result};
use clap::Args;

use crate::rpc_client;
use nexus_primitives::AccountAddress;

#[derive(Args)]
pub struct BalanceArgs {
    #[arg(long, short = 'a')]
    pub address: Option<String>,

    #[arg(long, short = 'k')]
    pub key_file: Option<std::path::PathBuf>,

    #[arg(long, default_value = "http://127.0.0.1:8080")]
    pub rpc_url: String,
}

pub fn run(args: BalanceArgs) -> Result<()> {
    rpc_client::validate_rpc_url(&args.rpc_url)?;

    let address = resolve_address(args.address.as_deref(), args.key_file.as_deref())?;
    let resp = rpc_client::query_balance(&args.rpc_url, &address)?;

    println!("Address: {}", address.to_hex());
    println!("Balance: {} (smallest unit)", resp.balance);

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
