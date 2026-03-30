// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

use anyhow::{Context, Result};
use clap::Args;

use crate::{artifact, rpc_client};
use nexus_execution::types::{TransactionBody, TransactionPayload};
use nexus_primitives::{ContractAddress, EpochNumber};

#[derive(Args)]
pub struct UpgradeArgs {
    #[arg(long, short = 'p')]
    pub package: std::path::PathBuf,

    #[arg(long, short = 'c')]
    pub contract: String,

    #[arg(long, short = 'k')]
    pub key_file: Option<std::path::PathBuf>,

    #[arg(long, default_value = "2000000")]
    pub gas_limit: u64,

    #[arg(long, default_value = "http://127.0.0.1:8080")]
    pub rpc_url: String,
}

pub fn run(args: UpgradeArgs) -> Result<()> {
    rpc_client::validate_rpc_url(&args.rpc_url)?;

    let identity = match &args.key_file {
        Some(path) => rpc_client::load_identity(path)?,
        None => {
            anyhow::bail!(
                "--key-file is required for upgrade operations; \
                 ephemeral keys risk permanent fund loss"
            );
        }
    };

    let modules = artifact::load_package_modules(&args.package)?;
    if modules.is_empty() {
        anyhow::bail!("no bytecode modules found in {:?}", args.package);
    }
    println!("Loaded {} module(s) for upgrade", modules.len());

    let contract_bytes: [u8; 32] = hex::decode(&args.contract)
        .context("decoding contract address hex")?
        .try_into()
        .map_err(|v: Vec<u8>| anyhow::anyhow!("address must be 32 bytes, got {}", v.len()))?;
    let contract = ContractAddress(contract_bytes);

    let body = TransactionBody {
        sender: identity.address,
        sequence_number: 0,
        expiry_epoch: EpochNumber(u64::MAX),
        gas_limit: args.gas_limit,
        gas_price: 1,
        target_shard: Some(rpc_client::resolve_target_shard(
            &args.rpc_url,
            &identity.address,
        )?),
        payload: TransactionPayload::MoveUpgrade {
            contract,
            bytecode_modules: modules,
        },
        chain_id: 1,
    };

    let signed = rpc_client::sign_transaction(&identity, body)?;
    println!("TxDigest: {}", signed.digest);

    let resp = rpc_client::submit_transaction(&args.rpc_url, &signed)?;
    if !resp.accepted {
        anyhow::bail!("upgrade transaction rejected by node");
    }
    println!("Submitted: accepted");

    match rpc_client::poll_tx_status(&args.rpc_url, &resp.tx_digest, 30, 1000)? {
        Some(status) => {
            println!("Status:   {}", status.status);
            println!("Gas used: {}", status.gas_used);
        }
        None => {
            println!("Timeout waiting for confirmation");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_args(package_dir: &std::path::Path) -> UpgradeArgs {
        UpgradeArgs {
            package: package_dir.to_path_buf(),
            contract: hex::encode([0u8; 32]),
            key_file: None,
            gas_limit: 2_000_000,
            rpc_url: "http://127.0.0.1:8080".into(),
        }
    }

    #[test]
    fn run_rejects_invalid_rpc_url() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut args = base_args(dir.path());
        args.rpc_url = "ftp://invalid".into();
        assert!(run(args).is_err());
    }

    #[test]
    fn run_requires_key_file() {
        // key_file = None → bail! before any filesystem or network call.
        let dir = tempfile::TempDir::new().unwrap();
        let err = run(base_args(dir.path())).unwrap_err().to_string();
        assert!(err.contains("--key-file"), "unexpected: {err}");
    }
}
