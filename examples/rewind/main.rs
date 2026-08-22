#[path = "scenario/mod.rs"]
mod scenario;

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    let report = scenario::run_rewind(true).await?;
    scenario::verify_rewind(&report)
}
