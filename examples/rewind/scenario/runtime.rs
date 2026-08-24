//! Embedded engine, worker assembly, and the durable checkpoint tool.

use std::{
    net::TcpListener,
    sync::{Arc, atomic::Ordering},
};

use anyhow::Result;
use odori::{Agent, AgentRegistry, Providers, Tool};
use odori_engine::{
    ConnectTarget, EmbeddedEngineConfig, EmbeddedStorageConfig, Engine, OdoriRuntime, TokeiraConfig,
};
use odori_mcp_bridge::BridgeConfig;
use serde_json::{Value, json};

use super::{provider::RewindProvider, state::RewindState};

fn agents(state: Arc<RewindState>) -> AgentRegistry {
    let executions = state.clone();
    let checkpoint = Tool::new(
        "checkpoint",
        "Record the deterministic deliberation checkpoint.",
        json!({
            "type": "object",
            "properties": {"label": {"type": "string"}},
            "required": ["label"],
            "additionalProperties": false,
        }),
        move |context, args| {
            let executions = executions.clone();
            async move {
                let ordinal = executions.tool_executions.fetch_add(1, Ordering::SeqCst) + 1;
                Ok(json!(format!(
                    "checkpoint:{}:{}:execution-{ordinal}",
                    args.get("label")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown"),
                    context.invocation_id
                )))
            }
        },
    );
    let mut registry = AgentRegistry::new();
    registry.register(
        Agent::new(
            "rewind-worker",
            "Record one durable checkpoint, then finish the deliberation. Re-present the same call id after restart.",
        )
        .with_provider("rewind-scripted")
        .with_tool(checkpoint),
    );
    registry.register(
        Agent::new(
            "timeline",
            "Restore the supplied deliberation snapshot and follow exactly the supplied decision.",
        )
        .with_provider("rewind-scripted"),
    );
    registry
}

pub(super) async fn start_engine(
    storage: EmbeddedStorageConfig,
) -> Result<(Engine, TcpListener, TcpListener)> {
    let grpc_guard = TcpListener::bind("127.0.0.1:0")?;
    let nexus_guard = TcpListener::bind("127.0.0.1:0")?;
    let mut config = TokeiraConfig::default();
    config.infrastructure.network.grpc_addr = grpc_guard.local_addr()?.to_string();
    config.policy.nexus_completion.http_addr = nexus_guard.local_addr()?.to_string();
    let engine = Engine::start_with_embedded_config(EmbeddedEngineConfig {
        server: config,
        storage,
        ..EmbeddedEngineConfig::default()
    })
    .await?;
    Ok((engine, grpc_guard, nexus_guard))
}

pub(super) async fn start_runtime(
    embedded: &Engine,
    state: Arc<RewindState>,
) -> Result<OdoriRuntime> {
    OdoriRuntime::builder("example-rewind")
        .connect(ConnectTarget::service_override(embedded.service_override()))
        .agents(agents(state.clone()))
        .providers(Providers::new(Arc::new(RewindProvider { state })))
        .bridge(BridgeConfig::default())
        .start()
        .await
}
