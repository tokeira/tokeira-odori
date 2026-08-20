//! Spike: drive headless Claude Code (`claude -p --output-format
//! stream-json`) from Rust the way the O3 provider will — supervised
//! subprocess, line-parsed event stream, captured terminal result, session
//! resume, exit-code taxonomy. Findings live in this spike's README.
//!
//! Probes (each `cargo run -- <probe>`):
//! - `turn [PROMPT]` — one fresh turn; parse the stream, time the
//!   milestones, print the session id the resume probe needs.
//! - `resume <SESSION_ID> [PROMPT]` — a follow-up turn against an existing
//!   session; proves history retention across processes.
//! - `taxonomy` — the failure shapes a provider must classify: missing
//!   session, unknown flag, and (while local OAuth is expired) auth failure.

mod events;

use std::{
    process::Stdio,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use events::{ContentBlock, Event};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    process::Command,
};

/// Hard ceiling on one harness turn — generous; the provider will instead
/// pair a Temporal start-to-close timeout with streaming heartbeats.
const TURN_TIMEOUT: Duration = Duration::from_secs(300);

/// One supervised run of the harness: everything the provider will need to
/// map a turn onto an activity result.
#[derive(Debug, Default)]
struct TurnReport {
    session_id: Option<String>,
    final_text: Option<String>,
    is_error: bool,
    terminal_reason: Option<String>,
    exit_code: Option<i32>,
    stderr: String,
    event_counts: Vec<(String, u32)>,
    first_event_ms: Option<u128>,
    total_ms: u128,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("turn") => {
            let prompt = args
                .get(1)
                .cloned()
                .unwrap_or_else(|| "Reply with exactly the word: pirouette".into());
            let report = run_turn(&prompt, None).await?;
            print_report("turn", &report);
        }
        Some("resume") => {
            let session = args.get(1).context("resume needs a session id")?;
            let prompt = args
                .get(2)
                .cloned()
                .unwrap_or_else(|| "What word did you reply with before?".into());
            let report = run_turn(&prompt, Some(session)).await?;
            print_report("resume", &report);
        }
        Some("taxonomy") => taxonomy().await?,
        _ => bail!("usage: claude-driver-spike <turn [prompt] | resume <id> [prompt] | taxonomy>"),
    }
    Ok(())
}

/// Build the command the way the provider will: explicit flag set, scrubbed
/// environment, piped stdio, kill-on-drop so a cancelled activity cannot leak
/// a harness process.
fn harness_command(prompt: &str, resume: Option<&str>) -> Command {
    let mut cmd = Command::new("claude");
    if let Some(id) = resume {
        cmd.arg("--resume").arg(id);
    }
    cmd.arg("-p")
        .arg(prompt)
        .args(["--output-format", "stream-json", "--verbose"]);
    // Finding: a harness spawned from inside another agent session inherits
    // that session's ANTHROPIC_BASE_URL (a host-side proxy) and fails auth
    // against it. The provider must scrub inherited vendor transport config
    // and let the harness use its own credential store.
    for var in [
        "ANTHROPIC_BASE_URL",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_API_KEY",
    ] {
        cmd.env_remove(var);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    cmd
}

async fn run_turn(prompt: &str, resume: Option<&str>) -> Result<TurnReport> {
    let started = Instant::now();
    let mut child = harness_command(prompt, resume)
        .spawn()
        .context("spawning `claude` (is Claude Code installed and on PATH?)")?;

    let stdout = child.stdout.take().context("stdout piped above")?;
    let mut stderr = child.stderr.take().context("stderr piped above")?;
    let stderr_task = tokio::spawn(async move {
        let mut buf = String::new();
        let _ = stderr.read_to_string(&mut buf).await;
        buf
    });

    let mut report = TurnReport::default();
    let mut lines = BufReader::new(stdout).lines();
    let deadline = tokio::time::sleep(TURN_TIMEOUT);
    tokio::pin!(deadline);

    loop {
        let line = tokio::select! {
            line = lines.next_line() => line?,
            _ = &mut deadline => {
                child.start_kill().ok();
                bail!("turn exceeded {TURN_TIMEOUT:?}; harness killed");
            }
        };
        let Some(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        report
            .first_event_ms
            .get_or_insert(started.elapsed().as_millis());

        // Malformed lines are counted, not fatal: protocol drift tolerance.
        let event: Event = match serde_json::from_str(&line) {
            Ok(event) => event,
            Err(_) => {
                count(&mut report.event_counts, "unparsed");
                continue;
            }
        };
        match &event {
            Event::System(system) => {
                count(
                    &mut report.event_counts,
                    &format!("system:{}", system.subtype),
                );
                if system.subtype == "init" {
                    report.session_id.clone_from(&system.session_id);
                }
            }
            Event::Assistant(assistant) => {
                count(&mut report.event_counts, "assistant");
                for block in &assistant.message.content {
                    if let ContentBlock::ToolUse { name, .. } = block {
                        count(&mut report.event_counts, &format!("tool_use:{name}"));
                    }
                }
            }
            Event::User(_) => count(&mut report.event_counts, "user"),
            Event::Result(result) => {
                count(&mut report.event_counts, "result");
                report.is_error = result.is_error;
                report.final_text.clone_from(&result.result);
                report.terminal_reason.clone_from(&result.terminal_reason);
                // On a fresh run `result.session_id` matches init's; on a
                // failed resume it merely echoes the requested id.
                report
                    .session_id
                    .get_or_insert_with(|| result.session_id.clone());
            }
            Event::Other(_) => count(&mut report.event_counts, "other"),
        }
    }

    let status = child.wait().await?;
    report.exit_code = status.code();
    report.stderr = stderr_task.await.unwrap_or_default();
    report.total_ms = started.elapsed().as_millis();
    Ok(report)
}

/// Exercise the failure shapes the provider's retry taxonomy must separate.
async fn taxonomy() -> Result<()> {
    // Missing session: stdout still carries a terminal `result` event
    // (subtype error_during_execution, num_turns 0); the reason is stderr's.
    let missing = run_turn("hi", Some("00000000-0000-0000-0000-000000000000")).await?;
    print_report("resume-missing-session", &missing);

    // Unknown flag: rejected before any stream exists — no events at all.
    let started = Instant::now();
    let out = Command::new("claude")
        .args(["--definitely-not-a-flag"])
        .output()
        .await?;
    println!(
        "\n== bad-flag ==\n  exit={:?} in {}ms, stdout {}B, stderr: {}",
        out.status.code(),
        started.elapsed().as_millis(),
        out.stdout.len(),
        String::from_utf8_lossy(&out.stderr)
            .lines()
            .next()
            .unwrap_or_default(),
    );
    Ok(())
}

fn count(counts: &mut Vec<(String, u32)>, key: &str) {
    match counts.iter_mut().find(|(name, _)| name == key) {
        Some((_, n)) => *n += 1,
        None => counts.push((key.to_owned(), 1)),
    }
}

fn print_report(label: &str, report: &TurnReport) {
    println!("== {label} ==");
    println!("  exit={:?} is_error={}", report.exit_code, report.is_error);
    println!(
        "  session={} terminal_reason={:?}",
        report.session_id.as_deref().unwrap_or("<none>"),
        report.terminal_reason,
    );
    println!(
        "  first_event={:?}ms total={}ms",
        report.first_event_ms, report.total_ms
    );
    println!("  events: {:?}", report.event_counts);
    println!("  result: {:?}", report.final_text);
    if !report.stderr.is_empty() {
        println!(
            "  stderr: {}",
            report.stderr.lines().next().unwrap_or_default()
        );
    }
}
