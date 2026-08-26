#[path = "scenario/mod.rs"]
mod scenario;

use anyhow::{Result, bail};
use odori_embedded_harness::take_storage_flag;

#[tokio::main]
async fn main() -> Result<()> {
    let mut arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let storage = take_storage_flag(&mut arguments)?;
    if !arguments.is_empty() {
        bail!("usage: slice-fleet [--storage <mode>]");
    }
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
