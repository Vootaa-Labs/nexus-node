// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

use crate::rpc_client;
use clap::Args;
use nexus_execution::types::{TransactionBody, TransactionPayload};
use nexus_primitives::{ContractAddress, EpochNumber};

#[derive(Args)]
pub struct CallArgs {
    /// Contract address (hex, with or without 0x prefix).
    #[arg(long)]
    pub contract: String,

    /// Fully-qualified function name (e.g. "counter::increment").
    #[arg(long)]
    pub function: String,

    /// BCS-encoded arguments (hex strings).
    #[arg(long, value_delimiter = ',')]
    pub args: Vec<String>,

    /// Type arguments (hex-encoded BCS type tags).
    #[arg(long, value_delimiter = ',')]
    pub type_args: Vec<String>,

    /// Nexus RPC endpoint URL.
    #[arg(long, default_value = "http://127.0.0.1:8080")]
    pub rpc_url: String,

    /// Caller private key file (hex-encoded).
    #[arg(long)]
    pub key_file: Option<std::path::PathBuf>,

    /// Gas limit for the call transaction.
    #[arg(long, default_value_t = 100_000)]
    pub gas_limit: u64,

    /// Sender nonce (sequence number).
    #[arg(long, default_value_t = 0)]
    pub nonce: u64,

    /// Maximum poll attempts for tx confirmation.
    #[arg(long, default_value_t = 30)]
    pub poll_attempts: u32,
}

pub fn run(args: CallArgs) -> anyhow::Result<()> {
    rpc_client::validate_rpc_url(&args.rpc_url)?;

    let addr_hex = args.contract.strip_prefix("0x").unwrap_or(&args.contract);
    let contract = ContractAddress::from_hex(addr_hex)
        .map_err(|e| anyhow::anyhow!("invalid contract address: {e}"))?;

    let call_args: Vec<Vec<u8>> = args
        .args
        .iter()
        .map(|h| hex::decode(h.strip_prefix("0x").unwrap_or(h)))
        .collect::<Result<_, _>>()
        .map_err(|e| anyhow::anyhow!("invalid hex argument: {e}"))?;

    let type_args: Vec<Vec<u8>> = args
        .type_args
        .iter()
        .map(|h| hex::decode(h.strip_prefix("0x").unwrap_or(h)))
        .collect::<Result<_, _>>()
        .map_err(|e| anyhow::anyhow!("invalid hex type argument: {e}"))?;

    let identity = match &args.key_file {
        Some(path) => rpc_client::load_identity(path)?,
        None => {
            anyhow::bail!(
                "--key-file is required for call operations; \
                 ephemeral keys risk permanent fund loss"
            );
        }
    };

    tracing::info!(
        contract = %args.contract,
        function = %args.function,
        arg_count = call_args.len(),
        caller = %identity.address,
        rpc = %args.rpc_url,
        "calling entry function"
    );

    let body = TransactionBody {
        sender: identity.address,
        sequence_number: args.nonce,
        expiry_epoch: EpochNumber(u64::MAX),
        gas_limit: args.gas_limit,
        gas_price: 1,
        target_shard: Some(rpc_client::resolve_target_shard(
            &args.rpc_url,
            &identity.address,
        )?),
        payload: TransactionPayload::MoveCall {
            contract,
            function: args.function,
            type_args,
            args: call_args,
        },
        chain_id: 1,
    };

    let signed_tx = rpc_client::sign_transaction(&identity, body)?;
    let resp = rpc_client::submit_transaction(&args.rpc_url, &signed_tx)?;

    println!("Submitted call tx: {}", resp.tx_digest);

    if !resp.accepted {
        anyhow::bail!("transaction rejected by node");
    }

    match rpc_client::poll_tx_status(&args.rpc_url, &resp.tx_digest, args.poll_attempts, 1000)? {
        Some(status) => {
            println!("Status: {}", status.status);
            println!("Gas used: {}", status.gas_used);
        }
        None => {
            println!(
                "Transaction not yet committed (timed out after {} s)",
                args.poll_attempts
            );
        }
    }

    Ok(())
}
