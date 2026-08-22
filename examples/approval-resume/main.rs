#[path = "scenario/mod.rs"]
mod scenario;

use std::path::Path;

use anyhow::{Result, bail};

#[tokio::main]
async fn main() -> Result<()> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    match arguments.as_slice() {
        [command, state] if command == "prepare" => {
            scenario::prepare(Path::new(state), true).await?;
        }
        [command, state, flag, plan_hash] if command == "resume" && flag == "--approve" => {
            scenario::resume(Path::new(state), plan_hash, true).await?;
        }
        _ => bail!(
            "usage:\n  approval-resume prepare <state-directory>\n  approval-resume resume <state-directory> --approve <plan-hash>"
        ),
    }
    Ok(())
}
