#[path = "scenario/mod.rs"]
mod scenario;

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    let report = scenario::run_scripted_fleet(true).await?;
    scenario::verify_fleet(&report)?;
    println!(
        "GREEN: applied={:?}, turns={}, tokens={}",
        report.applied,
        report.output.turns,
        report.output.usage.input_tokens + report.output.usage.output_tokens
    );
    Ok(())
}
