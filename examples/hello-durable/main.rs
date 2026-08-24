use std::sync::Arc;

use anyhow::{Result, bail};
use odori::{
    Agent, AgentRegistry, ConnectTarget, EmbeddedEngineConfig, Engine, OdoriRuntime, Providers,
    providers::CodexProvider,
};
use odori_embedded_harness::take_storage_flag;

#[tokio::main]
async fn main() -> Result<()> {
    let mut arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let storage = take_storage_flag(&mut arguments)?;
    if !arguments.is_empty() {
        bail!("usage: hello-durable [--storage <mode>]");
    }
    // Start the local durable engine that owns this run's history.
    let engine = Engine::start_with_embedded_config(EmbeddedEngineConfig {
        storage,
        ..EmbeddedEngineConfig::default()
    })
    .await?;
    println!(
        "ENGINE: {:?}; startup={:?}",
        engine.startup_report(),
        engine.startup_elapsed()
    );

    // Register an agent against the authenticated Codex subscription provider.
    let mut agents = AgentRegistry::new();
    agents.register(Agent::new("hello", "Answer clearly.").with_provider("codex"));
    let providers = Providers::new(Arc::new(CodexProvider::new()));

    let runtime = OdoriRuntime::builder("hello-durable")
        .connect(ConnectTarget::service_override(engine.service_override()))
        .agents(agents)
        .providers(providers)
        .start()
        .await?;

    // The stable run ID is the idempotency key for the whole execution.
    let answer: String = runtime
        .runner()
        .run("hello", "Say hello.", "hello-1")
        .await?;
    println!("{answer}");

    // Drain the worker before stopping the engine it is connected to.
    runtime.shutdown().await?;
    engine.shutdown().await?;
    Ok(())
}
