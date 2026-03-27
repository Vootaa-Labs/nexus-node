// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

use anyhow::{Context, Result};
use clap::Args;

use crate::rpc_client;
use nexus_execution::types::{TransactionBody, TransactionPayload};
use nexus_primitives::EpochNumber;

#[derive(Args)]
pub struct ScriptArgs {
    #[arg(long, short = 's')]
    pub script_file: std::path::PathBuf,

    #[arg(long, value_delimiter = ',')]
    pub type_args: Vec<String>,

    #[arg(long, value_delimiter = ',')]
    pub args: Vec<String>,

    #[arg(long, short = 'k')]
    pub key_file: Option<std::path::PathBuf>,

    #[arg(long, default_value = "1000000")]
    pub gas_limit: u64,

    #[arg(long, default_value = "http://127.0.0.1:8080")]
    pub rpc_url: String,
}

pub fn run(args: ScriptArgs) -> Result<()> {
    rpc_client::validate_rpc_url(&args.rpc_url)?;

    let identity = match &args.key_file {
        Some(path) => rpc_client::load_identity(path)?,
        None => {
            anyhow::bail!(
                "--key-file is required for script operations; \
                 ephemeral keys risk permanent fund loss"
            );
        }
    };

    let bytecode = std::fs::read(&args.script_file)
        .with_context(|| format!("reading script file {:?}", args.script_file))?;
    if bytecode.is_empty() {
        anyhow::bail!("script file is empty");
    }
    println!("Script bytecode: {} bytes", bytecode.len());

    let type_args: Vec<Vec<u8>> = args
        .type_args
        .iter()
        .map(|h| hex::decode(h).context("decoding type arg hex"))
        .collect::<Result<_>>()?;

    let call_args: Vec<Vec<u8>> = args
        .args
        .iter()
        .map(|h| hex::decode(h).context("decoding call arg hex"))
        .collect::<Result<_>>()?;

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
        payload: TransactionPayload::MoveScript {
            bytecode,
            type_args,
            args: call_args,
        },
        chain_id: 1,
    };

    let signed = rpc_client::sign_transaction(&identity, body)?;
    println!("TxDigest: {}", signed.digest);

    let resp = rpc_client::submit_transaction(&args.rpc_url, &signed)?;
    if !resp.accepted {
        anyhow::bail!("script transaction rejected by node");
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
