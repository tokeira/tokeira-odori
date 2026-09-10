// The run's narration is the example.
#![allow(clippy::print_stdout)]

use anyhow::Result;
use clap::Parser;
use odori_dev_support::StorageArgs;
use odori_examples::slice_fleet as scenario;

/// Run the scripted multi-agent fleet over a bundled fixture.
#[derive(Debug, Parser)]
#[command(name = "slice-fleet")]
struct Cli {
    #[command(flatten)]
    storage: StorageArgs,
}

#[tokio::main]
async fn main() -> Result<()> {
    let storage = Cli::parse().storage.resolve()?;
    let report = scenario::run_scripted_fleet_with_storage(true, storage).await?;
    scenario::verify_fleet(&report)?;
    println!(
        "GREEN: applied={:?}, turns={}, tokens={}",
        report.applied,
        report.output.turns,
        report.output.usage.input_tokens + report.output.usage.output_tokens
    );
    Ok(())
}
