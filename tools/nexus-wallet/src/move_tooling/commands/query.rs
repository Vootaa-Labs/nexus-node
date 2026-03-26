// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

use crate::rpc_client;
use clap::Args;

#[derive(Args)]
pub struct QueryArgs {
    /// Contract address (hex, with or without 0x prefix).
    #[arg(long)]
    pub contract: String,

    /// View function name (e.g. "counter::get_count").
    #[arg(long)]
    pub function: String,

    /// BCS-encoded arguments (hex strings).
    #[arg(long, value_delimiter = ',')]
    pub args: Vec<String>,

    /// Nexus RPC endpoint URL.
    #[arg(long, default_value = "http://127.0.0.1:8080")]
    pub rpc_url: String,
}

pub fn run(args: QueryArgs) -> anyhow::Result<()> {
    rpc_client::validate_rpc_url(&args.rpc_url)?;

    tracing::info!(
        contract = %args.contract,
        function = %args.function,
        rpc = %args.rpc_url,
        "querying view function"
    );

    let request = rpc_client::ContractQueryRequest {
        contract: args.contract,
        function: args.function,
        type_args: Vec::new(),
        args: args.args,
    };

    let resp = rpc_client::query_view_function(&args.rpc_url, &request)?;

    match resp.return_value {
        Some(value) => println!("{value}"),
        None => println!("(no return value)"),
    }
    println!("Gas used: {}", resp.gas_used);

    Ok(())
}
