//! The rewind scenario lifecycle and observable report.

mod bridge;
mod model;
mod observation;
mod provider;
mod runtime;
mod state;

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use anyhow::{Context as _, Result, anyhow, ensure};

use model::{DeliberationSnapshot, RewindEvent, TimelineInput};
use observation::{pending_activity, wait_for_attempt};
use runtime::{start_engine, start_runtime};
use state::RewindState;

/// Observable proof from the rewind example.
#[derive(Debug)]
pub struct RewindReport {
    pub dedupe_tool_executions: u64,
    pub total_tool_executions: u64,
    pub presentations: Vec<(u32, String)>,
    pub durable_attempt_before_replacement: i32,
    pub replacement_retry_attempt: u32,
    pub replacement_completion: std::time::Duration,
    pub timeline_a: String,
    pub timeline_b: String,
}

/// Kill the first harness attempt after a durable tool result, stop its
/// worker, start a replacement over the same engine, and restore the result
/// from the invocation registry. Then use the successful deliberation as one
/// immutable snapshot for two new workflows with deliberately different
/// decisions.
pub async fn run_rewind(print: bool) -> Result<RewindReport> {
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = Arc::new(RewindState {
        tool_executions: AtomicU64::new(0),
        presentations: Mutex::new(Vec::new()),
        events: sender,
    });
    let (embedded, _grpc_guard, _nexus_guard) = start_engine().await?;
    let runtime = start_runtime(&embedded, state.clone()).await?;
    let config = odori::RunConfig::default()
        .with_turn_timeout(std::time::Duration::from_secs(15))
        .with_turn_heartbeat_timeout(std::time::Duration::from_millis(500))
        .with_turn_max_attempts(5);
    let exact_runner = runtime.runner();
    let exact_config = config.clone();
    let exact_deliberation = tokio::spawn(async move {
        exact_runner
            .run_with_config::<String>(
                "rewind-worker",
                "deliberate until the plan-ready checkpoint",
                "rewind-exact-resume",
                exact_config,
            )
            .await
    });
    ensure!(matches!(
        next_event(&mut receiver).await?,
        RewindEvent::CheckpointRecorded
    ));
    ensure!(matches!(
        next_event(&mut receiver).await?,
        RewindEvent::FailureReturned
    ));
    let RewindEvent::RetryPresented(exact_attempt) = next_event(&mut receiver).await? else {
        return Err(anyhow!("same-worker retry was not presented"));
    };
    let snapshot_json = exact_deliberation
        .await
        .context("join exact deliberation")??;
    let snapshot: DeliberationSnapshot = serde_json::from_str(&snapshot_json)?;
    let dedupe_tool_executions = state.tool_executions.load(Ordering::SeqCst);
    if print {
        println!(
            "RESUME EXACTLY: attempts 1 and {exact_attempt} presented stable-checkpoint-call; tool executions={dedupe_tool_executions}"
        );
    }

    let restart_runner = runtime.runner();
    let restart_probe = tokio::spawn(async move {
        restart_runner
            .run_with_config::<String>(
                "rewind-worker",
                "restart canary at the plan-ready checkpoint",
                "rewind-worker-restart-canary",
                config,
            )
            .await
    });
    ensure!(matches!(
        next_event(&mut receiver).await?,
        RewindEvent::CheckpointRecorded
    ));
    ensure!(matches!(
        next_event(&mut receiver).await?,
        RewindEvent::FailureReturned
    ));
    if print {
        println!("KILL: harness exited 137 after the restart canary checkpoint");
        println!("STOP: worker A drains and stops; embedded engine stays alive");
    }
    let durable_client = runtime.client();
    runtime.shutdown().await?;
    let durable_attempt_before_replacement = wait_for_attempt(
        &durable_client,
        "rewind-worker-restart-canary",
        2,
        std::time::Duration::from_secs(5),
    )
    .await?;
    if print {
        println!(
            "DURABLE: engine reports activity attempt {durable_attempt_before_replacement} before worker B starts"
        );
    }
    let replacement = start_runtime(&embedded, state.clone()).await?;
    if print {
        println!("RESTART: replacement worker uses default workflow cache settings");
    }
    let (replacement_retry_attempt, replacement_completion) = match tokio::time::timeout(
        std::time::Duration::from_secs(15),
        receiver.recv(),
    )
    .await
    {
        Ok(Some(RewindEvent::RetryPresented(retry_attempt))) => {
            if print {
                println!("RESTART RESUME: replacement polled attempt {retry_attempt}");
            }
            let completion_started = std::time::Instant::now();
            tokio::time::timeout(std::time::Duration::from_secs(15), restart_probe)
                .await
                .context(
                    "replacement retry did not complete across the 10s sticky fallback window",
                )?
                .context("join restart probe")??;
            let replacement_completion = completion_started.elapsed();
            if print {
                println!(
                    "STICKY FALLBACK: workflow completed on worker B in {replacement_completion:?} after retry presentation"
                );
            }
            (retry_attempt, replacement_completion)
        }
        Ok(Some(event)) => return Err(anyhow!("unexpected restart event: {event:?}")),
        Ok(None) => return Err(anyhow!("restart event channel closed")),
        Err(_) => {
            restart_probe.abort();
            let still_pending = pending_activity(&durable_client, "rewind-worker-restart-canary")
                .await?
                .context("restart-canary activity disappeared without reaching the provider")?;
            ensure!(still_pending.attempt >= durable_attempt_before_replacement);
            return Err(anyhow!(
                "replacement worker did not resume activity attempt {} ({}) within 15s",
                still_pending.attempt,
                still_pending.state
            ));
        }
    };
    let input_a = serde_json::to_string(&TimelineInput {
        snapshot: snapshot.clone(),
        decision: "ship".to_owned(),
    })?;
    let input_b = serde_json::to_string(&TimelineInput {
        snapshot,
        decision: "hold".to_owned(),
    })?;
    let timeline_runner_a = replacement.runner();
    let timeline_runner_b = replacement.runner();
    let (timeline_a, timeline_b) = tokio::try_join!(
        timeline_runner_a.run::<String>("timeline", &input_a, "rewind-timeline-a"),
        timeline_runner_b.run::<String>("timeline", &input_b, "rewind-timeline-b"),
    )?;
    let report = RewindReport {
        dedupe_tool_executions,
        total_tool_executions: state.tool_executions.load(Ordering::SeqCst),
        presentations: state
            .presentations
            .lock()
            .expect("rewind presentations lock")
            .clone(),
        durable_attempt_before_replacement,
        replacement_retry_attempt,
        replacement_completion,
        timeline_a,
        timeline_b,
    };
    if print {
        println!(
            "PRESENTATIONS: {:?}; total checkpoint executions={}",
            report.presentations, report.total_tool_executions
        );
        println!("TIMELINE A: {}", report.timeline_a);
        println!("TIMELINE B: {}", report.timeline_b);
    }
    replacement.shutdown().await?;
    embedded.shutdown().await?;
    Ok(report)
}

async fn next_event(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<RewindEvent>,
) -> Result<RewindEvent> {
    tokio::time::timeout(std::time::Duration::from_secs(15), receiver.recv())
        .await
        .context("rewind event timed out")?
        .context("rewind event channel closed")
}

pub fn verify_rewind(report: &RewindReport) -> Result<()> {
    ensure!(report.dedupe_tool_executions == 1);
    ensure!(report.total_tool_executions == 2);
    ensure!(report.durable_attempt_before_replacement >= 2);
    ensure!(report.replacement_retry_attempt >= 2);
    ensure!(report.replacement_completion <= std::time::Duration::from_secs(15));
    ensure!(report.presentations.len() >= 4);
    ensure!(
        report
            .presentations
            .iter()
            .all(|(_, call_id)| call_id == "stable-checkpoint-call")
    );
    ensure!(report.timeline_a.contains("ship"));
    ensure!(report.timeline_b.contains("hold"));
    ensure!(report.timeline_a != report.timeline_b);
    Ok(())
}
