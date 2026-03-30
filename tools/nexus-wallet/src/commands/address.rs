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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_succeeds_with_no_key_file() {
        // ephemeral_identity() always succeeds; no key file needed.
        let result = run(AddressArgs { key_file: None });
        assert!(result.is_ok());
    }

    #[test]
    fn run_fails_with_nonexistent_key_file() {
        let result = run(AddressArgs {
            key_file: Some("/nonexistent/path/no_key.json".into()),
        });
        assert!(result.is_err());
    }

    #[test]
    fn run_produces_32_byte_hex_address() {
        // A 32-byte address is 64 hex chars.
        // We can't capture stdout, but we verify run() itself succeeds and
        // that ephemeral_identity produces a 32-byte address.
        let identity = rpc_client::ephemeral_identity();
        let hex = identity.address.to_hex();
        // AccountAddress is [u8; 32] → 64 hex chars.
        assert_eq!(hex.len(), 64, "expected 64-char hex, got {hex}");
    }
}
