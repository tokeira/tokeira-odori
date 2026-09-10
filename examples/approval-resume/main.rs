use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use odori_dev_support::StorageArgs;
use odori_examples::approval_resume as scenario;

/// Run to a human approval gate, persist, and resume against the same history.
#[derive(Debug, Parser)]
#[command(name = "approval-resume")]
struct Cli {
    #[command(flatten)]
    storage: StorageArgs,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run until the approval gate and persist the waiting state.
    Prepare {
        /// Directory the run's state is written to.
        state_directory: PathBuf,
    },
    /// Resume the persisted run with an approved plan hash.
    Resume {
        /// Directory a previous `prepare` wrote its state to.
        state_directory: PathBuf,
        /// The plan hash being approved, as printed by `prepare`.
        #[arg(long, value_name = "PLAN_HASH")]
        approve: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let storage = cli.storage.resolve()?;
    match cli.command {
        Command::Prepare { state_directory } => {
            scenario::prepare_with_storage(&state_directory, true, storage).await?;
        }
        Command::Resume {
            state_directory,
            approve,
        } => {
            scenario::resume_with_storage(&state_directory, &approve, true, storage).await?;
        }
    }
    Ok(())
}
