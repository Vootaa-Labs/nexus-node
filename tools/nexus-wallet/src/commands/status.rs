// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! `status` — check transaction receipt by digest.

use anyhow::Result;
use clap::Args;

use crate::rpc_client;

#[derive(Args)]
pub struct StatusArgs {
    /// Hex-encoded transaction digest.
    #[arg(long, short = 't')]
    pub tx_digest: String,

    /// Nexus RPC endpoint URL.
    #[arg(long, default_value = "http://127.0.0.1:8080")]
    pub rpc_url: String,
}

pub fn run(args: StatusArgs) -> Result<()> {
    rpc_client::validate_rpc_url(&args.rpc_url)?;

    match rpc_client::poll_tx_status(&args.rpc_url, &args.tx_digest, 1, 0)? {
        Some(status) => {
            println!("TxDigest: {}", args.tx_digest);
            println!("Status:   {}", status.status);
            println!("Gas used: {}", status.gas_used);
        }
        None => {
            println!("Transaction not found: {}", args.tx_digest);
        }
    }
    Ok(())
}
