//! Flagship examples against the real embedded engine. The scripted paths
//! are unguarded; the real subscription smoke consumes quota and is ignored.

#[path = "../../../examples/support/mod.rs"]
mod support;

use std::{net::TcpListener, sync::Arc, time::Duration};

use anyhow::{Result, ensure};
use odori_agents::{Agent, AgentRegistry, Providers, RunConfig};
use odori_engine::{ConnectTarget, OdoriRuntime};
use odori_providers::{ClaudeProvider, CodexProvider};
use tokeira_engine::{Engine, TokeiraConfig};

#[tokio::test(flavor = "multi_thread")]
async fn slice_fleet_enforces_the_full_scripted_path() -> Result<()> {
    let report = support::run_scripted_fleet(false).await?;
    support::verify_fleet(&report)
}

#[tokio::test(flavor = "multi_thread")]
async fn rewind_resumes_exactly_and_diverges_timelines() -> Result<()> {
    let report = support::run_rewind(false).await?;
    support::verify_rewind(&report)
}

#[tokio::test(flavor = "multi_thread")]
async fn rewind_survives_worker_replacement_with_default_cache() -> Result<()> {
    let report = support::run_rewind(false).await?;
    ensure!(report.replacement_retry_attempt >= 2);
    ensure!(report.replacement_completion <= Duration::from_secs(15));
    support::verify_rewind(&report)
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
