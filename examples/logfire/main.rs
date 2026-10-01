// The run's narration is the example.
#![allow(clippy::print_stdout)]

use anyhow::{Context as _, Result, bail};
use clap::Parser;
use odori_dev_support::StorageArgs;
use odori_examples::logfire as scenario;

/// Export one run's agent-semantic spans to Logfire.
#[derive(Debug, Parser)]
#[command(name = "logfire")]
struct Cli {
    #[command(flatten)]
    storage: StorageArgs,
}

/// Spans below `warn` are exported only for Odori's agent-semantic layer.
///
/// This is the redaction default, not a cosmetic one: the embedded engine
/// and the Temporal SDK internals emit wide diagnostic spans whose fields
/// include serialized activity payloads — prompts included — at `info`
/// and below. Odori's own `invoke_agent`/`chat`/`execute_tool` spans carry
/// names, identifiers, and accounting, never content. Raise the rest into
/// an exporter only when you understand what leaves the process.
const REDACTED_EXPORT_FILTER: &str = "warn,odori_agents=info";

/// Seed the redacting default filter unless the operator set one.
///
/// The logfire SDK takes its per-target filter only from `RUST_LOG`, which is
/// why this writes the process environment at all. The single `unsafe` lives
/// here so the carve-out is one site, not the whole example.
#[allow(unsafe_code)]
fn seed_export_filter() {
    if std::env::var_os("RUST_LOG").is_none() {
        // SAFETY: called from `main` before the tokio runtime is built, so
        // the process is still single-threaded and no concurrent environment
        // access is possible.
        unsafe { std::env::set_var("RUST_LOG", REDACTED_EXPORT_FILTER) };
    }
}

fn main() -> Result<()> {
    let storage = Cli::parse().storage.resolve()?;

    // This example exists to land a trace in Logfire; running without the
    // token would "succeed" while demonstrating nothing. No fallback.
    if std::env::var_os("LOGFIRE_TOKEN").is_none() {
        bail!(
            "LOGFIRE_TOKEN must be set (create a write token under your \
             Logfire project settings); the region is parsed from the token"
        );
    }
    seed_export_filter();

    let logfire = logfire::configure()
        .with_service_name("odori-logfire-example")
        .finish()
        .context("configure the Logfire exporter")?;

    let report = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("build the tokio runtime")?
        .block_on(scenario::run_scripted_conversation(storage));

    // Flush the exporter before reporting, so the trace is already in
    // Logfire when the user goes looking for it.
    logfire.shutdown().context("flush spans to Logfire")?;
    let report = report?;

    println!("final text: {}", report.output.text);
    println!("saved notes: {:?}", report.saved_notes);
    println!(
        "usage: {} input + {} output tokens, ${:.4} across {} turns",
        report.output.usage.input_tokens,
        report.output.usage.output_tokens,
        report.output.usage.total_cost_usd,
        report.output.turns,
    );
    println!(
        "open your Logfire project's Live view and find the trace named \
         \"invoke_agent day-planner\" (run id logfire-1)"
    );
    Ok(())
}
