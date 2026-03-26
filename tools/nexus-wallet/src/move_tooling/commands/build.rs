use clap::Args;
use std::path::PathBuf;

#[derive(Args)]
pub struct BuildArgs {
    #[arg(short, long, default_value = ".")]
    pub package_dir: PathBuf,

    #[arg(long, value_delimiter = ',')]
    pub named_addresses: Vec<String>,

    #[arg(long, default_value_t = false)]
    pub skip_fetch: bool,
}

pub fn run(args: BuildArgs) -> anyhow::Result<()> {
    let named_addrs: Vec<String> = args.named_addresses.clone();
    let parsed_addrs = nexus_move_package::parse_named_address_assignments(&named_addrs)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    tracing::info!(
        package_dir = %args.package_dir.display(),
        "compiling Move package"
    );

    let build_result =
        nexus_move_package::build::build_package(&args.package_dir, &parsed_addrs, None)
            .map_err(|e| anyhow::anyhow!("Move build failed: {e}"))?;

    tracing::info!(modules = build_result.module_count, "build succeeded");

    tracing::info!(
        artifact_dir = %build_result.artifact_dir.display(),
        modules = build_result.module_count,
        total_bytes = build_result.total_bytes,
        "nexus artifact generated"
    );

    println!(
        "Nexus artifact: {} ({} module(s), {} bytes)",
        build_result.artifact_dir.display(),
        build_result.module_count,
        build_result.total_bytes,
    );

    Ok(())
}
