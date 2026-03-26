//! `transfer` — send native tokens from wallet to a recipient.

use anyhow::{Context, Result};
use clap::Args;

use crate::rpc_client;
use nexus_primitives::{AccountAddress, Amount, EpochNumber, TokenId};

#[derive(Args)]
pub struct TransferArgs {
    /// Path to Dilithium secret-key file.
    #[arg(long, short = 'k')]
    pub key_file: Option<std::path::PathBuf>,

    /// Hex-encoded recipient address.
    #[arg(long, short = 'r')]
    pub to: String,

    /// Amount to transfer (in smallest unit).
    #[arg(long)]
    pub amount: u64,

    /// Gas limit.
    #[arg(long, default_value = "100000")]
    pub gas_limit: u64,

    /// Gas price per unit.
    #[arg(long, default_value = "1")]
    pub gas_price: u64,

    /// Explicit sender nonce (auto-increments if omitted).
    #[arg(long, default_value = "0")]
    pub nonce: u64,

    /// Chain ID for replay protection.
    #[arg(long, default_value = "1")]
    pub chain_id: u64,

    /// Nexus RPC endpoint URL.
    #[arg(long, default_value = "http://127.0.0.1:8080")]
    pub rpc_url: String,

    /// Maximum poll attempts for tx confirmation.
    #[arg(long, default_value = "30")]
    pub poll_attempts: u32,
}

pub fn run(args: TransferArgs) -> Result<()> {
    rpc_client::validate_rpc_url(&args.rpc_url)?;

    let identity = match &args.key_file {
        Some(path) => rpc_client::load_identity(path)?,
        None => {
            anyhow::bail!(
                "--key-file is required for transfer operations; \
                 ephemeral keys risk permanent fund loss"
            );
        }
    };

    let recipient = AccountAddress::from_hex(&args.to).context("decoding recipient address")?;

    println!("Sender:    {}", identity.address.to_hex());
    println!("Recipient: {}", recipient.to_hex());
    println!("Amount:    {}", args.amount);

    use nexus_execution::types::{TransactionBody, TransactionPayload};

    let body = TransactionBody {
        sender: identity.address,
        sequence_number: args.nonce,
        expiry_epoch: EpochNumber(u64::MAX),
        gas_limit: args.gas_limit,
        gas_price: args.gas_price,
        target_shard: None,
        payload: TransactionPayload::Transfer {
            recipient,
            amount: Amount(args.amount),
            token: TokenId::Native,
        },
        chain_id: args.chain_id,
    };

    let signed = rpc_client::sign_transaction(&identity, body)?;
    let digest_hex = hex::encode(signed.digest.as_bytes());
    println!("TxDigest:  {digest_hex}");

    let resp = rpc_client::submit_transaction(&args.rpc_url, &signed)?;
    if !resp.accepted {
        anyhow::bail!("transaction rejected by node");
    }
    println!("Submitted: accepted");

    // Poll for confirmation.
    print!("Confirming");
    match rpc_client::poll_tx_status(&args.rpc_url, &digest_hex, args.poll_attempts, 1000)? {
        Some(status) => {
            println!("\nStatus:    {}", status.status);
            println!("Gas used:  {}", status.gas_used);
        }
        None => {
            println!("\nTimeout waiting for confirmation (tx may still be processing)");
        }
    }

    Ok(())
}
