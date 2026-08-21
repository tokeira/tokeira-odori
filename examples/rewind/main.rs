#[path = "../support/mod.rs"]
mod support;

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    let report = support::run_rewind(true).await?;
    support::verify_rewind(&report)
}
