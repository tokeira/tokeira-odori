#[path = "../support/mod.rs"]
mod support;

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    let report = support::run_scripted_fleet(true).await?;
    support::verify_fleet(&report)?;
    println!(
        "GREEN: applied={:?}, turns={}, tokens={}",
        report.applied,
        report.output.turns,
        report.output.usage.input_tokens + report.output.usage.output_tokens
    );
    Ok(())
}
