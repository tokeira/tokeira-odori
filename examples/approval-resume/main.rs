#[path = "scenario/mod.rs"]
mod scenario;

use std::path::Path;

use anyhow::{Result, bail};
use odori_embedded_harness::take_storage_flag;

#[tokio::main]
async fn main() -> Result<()> {
    let mut arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let storage = take_storage_flag(&mut arguments)?;
    match arguments.as_slice() {
        [command, state] if command == "prepare" => {
            scenario::prepare_with_storage(Path::new(state), true, storage).await?;
        }
        [command, state, flag, plan_hash] if command == "resume" && flag == "--approve" => {
            scenario::resume_with_storage(Path::new(state), plan_hash, true, storage).await?;
        }
        _ => bail!(
            "usage:\n  approval-resume [--storage <mode>] prepare <state-directory>\n  approval-resume [--storage <mode>] resume <state-directory> --approve <plan-hash>"
        ),
    }
    Ok(())
}
