// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! `address` — derive and display AccountAddress from a key file.

use anyhow::Result;
use clap::Args;

use crate::rpc_client;

#[derive(Args)]
pub struct AddressArgs {
    /// Path to Dilithium secret-key file (JSON or raw hex).
    #[arg(long, short = 'k')]
    pub key_file: Option<std::path::PathBuf>,
}

pub fn run(args: AddressArgs) -> Result<()> {
    let identity = match &args.key_file {
        Some(path) => rpc_client::load_identity(path)?,
        None => {
            eprintln!("warn: no --key-file provided, generating ephemeral identity");
            rpc_client::ephemeral_identity()
        }
    };

    println!("{}", identity.address.to_hex());
    Ok(())
}
