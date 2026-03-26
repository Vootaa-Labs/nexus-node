// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

use crate::{artifact, rpc_client};
use clap::Args;
use nexus_execution::types::{TransactionBody, TransactionPayload};
use nexus_primitives::{ContractAddress, EpochNumber};
use std::path::PathBuf;

#[derive(Args)]
pub struct DeployArgs {
    /// Path to the Move package directory (containing build/).
    #[arg(short, long, default_value = ".")]
    pub package_dir: PathBuf,

    /// Nexus RPC endpoint URL.
    #[arg(long, default_value = "http://127.0.0.1:8080")]
    pub rpc_url: String,

    /// Deployer private key file (hex-encoded).
    #[arg(long)]
    pub key_file: Option<PathBuf>,

    /// Gas limit for the publish transaction.
    #[arg(long, default_value_t = 500_000)]
    pub gas_limit: u64,

    /// Sender nonce (sequence number).
    #[arg(long, default_value_t = 0)]
    pub nonce: u64,

    /// Maximum poll attempts for tx confirmation.
    #[arg(long, default_value_t = 30)]
    pub poll_attempts: u32,
}

pub fn run(args: DeployArgs) -> anyhow::Result<()> {
    rpc_client::validate_rpc_url(&args.rpc_url)?;

    let modules = artifact::load_package_modules(&args.package_dir)?;
    if modules.is_empty() {
        anyhow::bail!("no .mv bytecode modules found");
    }

    let artifact_dir = args.package_dir.join("nexus-artifact");
    let meta_path = artifact_dir.join("package-metadata.bcs");
    let pkg_name = if meta_path.exists() {
        let bcs_bytes = std::fs::read(&meta_path)?;
        let meta: artifact::PackageMetadata = bcs::from_bytes(&bcs_bytes)?;
        meta.name
    } else {
        "unknown".to_string()
    };

    let identity = match &args.key_file {
        Some(path) => rpc_client::load_identity(path)?,
        None => {
            anyhow::bail!(
                "--key-file is required for deploy operations; \
                 ephemeral keys risk permanent fund loss"
            );
        }
    };

    let total_bytes: usize = modules.iter().map(|m| m.len()).sum();
    tracing::info!(
        package = %pkg_name,
        module_count = modules.len(),
        total_bytes,
        deployer = %identity.address,
        rpc = %args.rpc_url,
        "deploying modules"
    );

    let bytecode_hash = {
        let mut hasher = blake3::Hasher::new();
        for m in &modules {
            hasher.update(m);
        }
        hasher.finalize()
    };
    let contract_addr =
        ContractAddress::from_deployment(&identity.address, bytecode_hash.as_bytes());

    let body = TransactionBody {
        sender: identity.address,
        sequence_number: args.nonce,
        expiry_epoch: EpochNumber(u64::MAX),
        gas_limit: args.gas_limit,
        gas_price: 1,
        target_shard: None,
        payload: TransactionPayload::MovePublish {
            bytecode_modules: modules,
        },
        chain_id: 0,
    };

    let signed_tx = rpc_client::sign_transaction(&identity, body)?;
    let resp = rpc_client::submit_transaction(&args.rpc_url, &signed_tx)?;

    println!("Submitted publish tx: {}", resp.tx_digest);
    println!("Expected contract address: 0x{}", contract_addr.to_hex());

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
