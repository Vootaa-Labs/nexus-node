use anyhow::{Context, Result};
use clap::Args;

use crate::rpc_client;
use nexus_execution::types::{TransactionBody, TransactionPayload};
use nexus_primitives::{ContractAddress, EpochNumber};

#[derive(Args)]
pub struct DryRunArgs {
    #[arg(long, short = 'c')]
    pub contract: String,

    #[arg(long, short = 'f')]
    pub function: String,

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

pub fn run(args: DryRunArgs) -> Result<()> {
    rpc_client::validate_rpc_url(&args.rpc_url)?;

    let identity = match &args.key_file {
        Some(path) => rpc_client::load_identity(path)?,
        None => {
            tracing::warn!("no --key-file provided; using ephemeral dev identity");
            rpc_client::ephemeral_identity()
        }
    };

    let contract_bytes: [u8; 32] = hex::decode(&args.contract)
        .context("decoding contract address")?
        .try_into()
        .map_err(|v: Vec<u8>| anyhow::anyhow!("address must be 32 bytes, got {}", v.len()))?;
    let contract = ContractAddress(contract_bytes);

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
        target_shard: None,
        payload: TransactionPayload::MoveCall {
            contract,
            function: args.function.clone(),
            type_args,
            args: call_args,
        },
        chain_id: 0,
    };

    let signed = rpc_client::sign_transaction(&identity, body)?;
    let url = format!(
        "{}/v2/tx/submit?dry_run=true",
        args.rpc_url.trim_end_matches('/')
    );
    let tx_json = serde_json::to_value(&signed).context("serializing transaction to JSON")?;

    let response = rpc_client::http_agent()
        .post(&url)
        .set("Content-Type", "application/json")
        .send_json(tx_json)
        .with_context(|| format!("POST {url}"))?;

    let result: serde_json::Value = response.into_json().context("parsing dry-run response")?;
    println!("Dry-run result for {}:", args.function);
    println!("{}", serde_json::to_string_pretty(&result)?);

    Ok(())
}
