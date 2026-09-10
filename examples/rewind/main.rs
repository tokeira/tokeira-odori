use anyhow::Result;
use clap::Parser;
use odori_dev_support::StorageArgs;
use odori_examples::rewind as scenario;

/// Replay a run from history and diverge its timeline.
#[derive(Debug, Parser)]
#[command(name = "rewind")]
struct Cli {
    #[command(flatten)]
    storage: StorageArgs,
}

#[tokio::main]
async fn main() -> Result<()> {
    let storage = Cli::parse().storage.resolve()?;
    let report = scenario::run_rewind_with_storage(true, storage).await?;
    scenario::verify_rewind(&report)
}
