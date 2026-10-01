//! Support for the integration tests in `tests/`. Shared machinery lives in
//! `odori-dev-support`; this crate supplies the harness's in-memory default.

use std::{net::TcpListener, sync::Arc};

use anyhow::Result;
use odori_agents::{AgentRegistry, Providers, provider::Provider};
use odori_engine::{
    ConnectTarget, EmbeddedEngineConfig, EmbeddedStorageConfig, Engine, OdoriRuntime, TokeiraConfig,
};

/// Start the embedded engine with the integration harness's in-memory default.
pub async fn start_engine() -> Result<(Engine, TcpListener, TcpListener)> {
    let grpc_guard = TcpListener::bind("127.0.0.1:0")?;
    let nexus_guard = TcpListener::bind("127.0.0.1:0")?;
    let mut config = TokeiraConfig::default();
    config.infrastructure.network.grpc_addr = grpc_guard.local_addr()?.to_string();
    config.policy.nexus_completion.http_addr = nexus_guard.local_addr()?.to_string();
    let engine = Engine::start_with_embedded_config(EmbeddedEngineConfig {
        server: config,
        storage: EmbeddedStorageConfig::InMemory,
        ..EmbeddedEngineConfig::default()
    })
    .await?;
    Ok((engine, grpc_guard, nexus_guard))
}

/// Attach an Odori runtime to a running engine over its in-process
/// `service_override` transport — no TCP listener, no port.
pub async fn start_runtime(
    engine: &Engine,
    task_queue: &str,
    registry: AgentRegistry,
    provider: Arc<dyn Provider>,
) -> Result<OdoriRuntime> {
    OdoriRuntime::builder(task_queue)
        .connect(ConnectTarget::service_override(engine.service_override()))
        .agents(registry)
        .providers(Providers::new(provider))
        .start()
        .await
}
