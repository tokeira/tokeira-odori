#[path = "scenario/mod.rs"]
mod scenario;

use anyhow::{Context as _, Result, bail};
use odori_embedded_harness::take_storage_flag;

/// Spans below `warn` are exported only for Odori's agent-semantic layer.
///
/// This is the redaction default, not a cosmetic one: the embedded engine
/// and the Temporal SDK internals emit wide diagnostic spans whose fields
/// include serialized activity payloads — prompts included — at `info`
/// and below. Odori's own `invoke_agent`/`chat`/`execute_tool` spans carry
/// names, identifiers, and accounting, never content. Raise the rest into
/// an exporter only when you understand what leaves the process.
const REDACTED_EXPORT_FILTER: &str = "warn,odori_agents=info";

fn main() -> Result<()> {
    let mut arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let storage = take_storage_flag(&mut arguments)?;
    if !arguments.is_empty() {
        bail!("usage: logfire [--storage <mode>]");
    }

    // This example exists to land a trace in Logfire; running without the
    // token would "succeed" while demonstrating nothing. No fallback.
    if std::env::var_os("LOGFIRE_TOKEN").is_none() {
        bail!(
            "LOGFIRE_TOKEN must be set (create a write token under your \
             Logfire project settings); the region is parsed from the token"
        );
    }
    if std::env::var_os("RUST_LOG").is_none() {
        // The logfire SDK reads its filter from RUST_LOG and defaults to
        // TRACE. Seed the redacting default before any thread exists —
        // main() has not built the tokio runtime yet.
        // SAFETY: single-threaded at this point; no concurrent environment
        // access is possible.
        unsafe { std::env::set_var("RUST_LOG", REDACTED_EXPORT_FILTER) };
    }

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
