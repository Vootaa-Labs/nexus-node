use anyhow::Result;
use clap::Args;

use crate::rpc_client;

#[derive(Args)]
pub struct FaucetArgs {
    #[arg(long, short = 'k')]
    pub key_file: Option<std::path::PathBuf>,

    #[arg(long, short = 'a')]
    pub address: Option<String>,

    #[arg(long, default_value = "http://127.0.0.1:8080")]
    pub rpc_url: String,
}

pub fn run(args: FaucetArgs) -> Result<()> {
    rpc_client::validate_rpc_url(&args.rpc_url)?;

    let address = if let Some(hex_str) = &args.address {
        nexus_primitives::AccountAddress::from_hex(hex_str)
            .map_err(|e| anyhow::anyhow!("invalid hex address: {e}"))?
    } else {
        let identity = match &args.key_file {
            Some(path) => rpc_client::load_identity(path)?,
            None => {
                tracing::warn!("no --key-file provided; generating ephemeral address");
                rpc_client::ephemeral_identity()
            }
        };
        identity.address
    };

    println!("Requesting faucet for: {}", address.to_hex());

    let resp = rpc_client::request_faucet(&args.rpc_url, &address)?;
    println!("Minted:   {} (smallest unit)", resp.amount);
    println!("TxDigest: {}", resp.tx_digest);

    Ok(())
}
