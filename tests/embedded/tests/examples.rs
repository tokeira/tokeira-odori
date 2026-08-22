//! Flagship examples against the real embedded engine. The scripted paths
//! are unguarded; the real subscription smoke consumes quota and is ignored.

#[path = "../../../examples/approval-resume/scenario/mod.rs"]
mod approval_resume;
#[path = "../../../examples/rewind/scenario/mod.rs"]
mod rewind;
#[path = "../../../examples/slice-fleet/scenario/mod.rs"]
mod slice_fleet;

use std::{
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::{Context as _, Result, ensure};
use odori_agents::{Agent, AgentRegistry, Providers, RunConfig};
use odori_engine::{ConnectTarget, OdoriRuntime};
use odori_providers::{ClaudeProvider, CodexProvider};
use tokeira_engine::{Engine, TokeiraConfig};

#[tokio::test(flavor = "multi_thread")]
async fn slice_fleet_enforces_the_full_scripted_path() -> Result<()> {
    let report = slice_fleet::run_scripted_fleet(false).await?;
    slice_fleet::verify_fleet(&report)
}

#[tokio::test(flavor = "multi_thread")]
async fn rewind_resumes_exactly_and_diverges_timelines() -> Result<()> {
    let report = rewind::run_rewind(false).await?;
    rewind::verify_rewind(&report)
}

#[tokio::test(flavor = "multi_thread")]
async fn rewind_survives_worker_replacement_with_default_cache() -> Result<()> {
    let report = rewind::run_rewind(false).await?;
    ensure!(report.replacement_retry_attempt >= 2);
    ensure!(report.replacement_completion <= Duration::from_secs(15));
    rewind::verify_rewind(&report)
}

static APPROVAL_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TemporaryStateDirectory {
    path: PathBuf,
}

impl TemporaryStateDirectory {
    fn new() -> Result<Self> {
        let sequence = APPROVAL_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "odori-approval-process-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir(&path)
            .with_context(|| format!("create temporary state directory {}", path.display()))?;
        Ok(Self { path })
    }
}

impl Drop for TemporaryStateDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn approval_stage(stage: &str, state: &Path, plan_hash: Option<&str>) -> Result<Output> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args([
            "--ignored",
            "--exact",
            "approval_resume_process_stage",
            "--nocapture",
        ])
        .env("ODORI_APPROVAL_STAGE", stage)
        .env("ODORI_APPROVAL_STATE", state);
    if let Some(plan_hash) = plan_hash {
        command.env("ODORI_APPROVAL_HASH", plan_hash);
    }
    command.output().context("run approval-resume subprocess")
}

#[test]
fn approval_resume_crosses_a_process_boundary() -> Result<()> {
    let state = TemporaryStateDirectory::new()?;
    let prepare = approval_stage("prepare", &state.path, None)?;
    ensure!(
        prepare.status.success(),
        "prepare process failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&prepare.stdout),
        String::from_utf8_lossy(&prepare.stderr)
    );
    let prepare_stdout = String::from_utf8(prepare.stdout)?;
    ensure!(prepare_stdout.contains("HUMAN APPROVAL REQUIRED"));
    ensure!(prepare_stdout.contains("SNAPSHOT WRITTEN"));
    let snapshot = state.path.join("engine.snapshot");
    ensure!(snapshot.is_file() && std::fs::metadata(&snapshot)?.len() > 0);
    approval_resume::verify_waiting_state(&state.path)?;

    let refused = approval_stage("resume", &state.path, Some("unreviewed-plan"))?;
    ensure!(!refused.status.success(), "an unreviewed hash was accepted");
    let refusal = format!(
        "{}{}",
        String::from_utf8_lossy(&refused.stdout),
        String::from_utf8_lossy(&refused.stderr)
    );
    ensure!(refusal.contains("does not match proposal"));
    approval_resume::verify_waiting_state(&state.path)?;

    let resume = approval_stage("resume", &state.path, Some(approval_resume::PLAN_HASH))?;
    ensure!(
        resume.status.success(),
        "resume process failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&resume.stdout),
        String::from_utf8_lossy(&resume.stderr)
    );
    let resume_stdout = String::from_utf8(resume.stdout)?;
    ensure!(resume_stdout.contains("RESTORED"));
    ensure!(resume_stdout.contains("HUMAN APPROVAL RECORDED"));
    ensure!(resume_stdout.contains("APPLIED ONCE"));
    ensure!(resume_stdout.contains("GREEN"));
    approval_resume::verify_completed_state(&state.path)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "subprocess helper invoked by approval_resume_crosses_a_process_boundary"]
async fn approval_resume_process_stage() -> Result<()> {
    let stage = std::env::var("ODORI_APPROVAL_STAGE").context("missing subprocess stage")?;
    let state = PathBuf::from(
        std::env::var_os("ODORI_APPROVAL_STATE").context("missing subprocess state path")?,
    );
    match stage.as_str() {
        "prepare" => {
            approval_resume::prepare(&state, true).await?;
        }
        "resume" => {
            let plan_hash =
                std::env::var("ODORI_APPROVAL_HASH").context("missing approval hash")?;
            approval_resume::resume(&state, &plan_hash, true).await?;
        }
        other => anyhow::bail!("unknown approval subprocess stage {other:?}"),
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "burns Claude and Codex subscription quota; set ODORI_RUN_LIVE_EXAMPLES=1"]
async fn live_cross_provider_example_smoke() -> Result<()> {
    ensure!(
        std::env::var("ODORI_RUN_LIVE_EXAMPLES").as_deref() == Ok("1"),
        "set ODORI_RUN_LIVE_EXAMPLES=1 to confirm quota use"
    );
    let grpc_guard = TcpListener::bind("127.0.0.1:0")?;
    let nexus_guard = TcpListener::bind("127.0.0.1:0")?;
    let mut config = TokeiraConfig::default();
    config.infrastructure.network.grpc_addr = grpc_guard.local_addr()?.to_string();
    config.policy.nexus_completion.http_addr = nexus_guard.local_addr()?.to_string();
    let engine = Engine::start_with_config(config).await?;
    let mut agents = AgentRegistry::new();
    agents.register(
        Agent::new("live-claude", "Reply with exactly: claude-reviewed").with_provider("claude"),
    );
    agents.register(
        Agent::new("live-codex", "Reply with exactly: codex-reviewed").with_provider("codex"),
    );
    let runtime = OdoriRuntime::builder("example-live-providers")
        .connect(ConnectTarget::service_override(engine.service_override()))
        .agents(agents)
        .providers(
            Providers::new(Arc::new(ClaudeProvider::new())).with(Arc::new(CodexProvider::new())),
        )
        .start()
        .await?;
    let run_config = RunConfig::default()
        .with_turn_timeout(Duration::from_secs(300))
        .with_turn_max_attempts(1);
    let claude: String = runtime
        .runner()
        .run_with_config(
            "live-claude",
            "review the marker",
            "example-live-claude",
            run_config.clone(),
        )
        .await?;
    let codex: String = runtime
        .runner()
        .run_with_config(
            "live-codex",
            "review the marker",
            "example-live-codex",
            run_config,
        )
        .await?;
    ensure!(claude.contains("claude-reviewed"));
    ensure!(codex.contains("codex-reviewed"));
    runtime.shutdown().await?;
    engine.shutdown().await?;
    Ok(())
}
