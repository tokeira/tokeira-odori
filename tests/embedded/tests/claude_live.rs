//! Live tests: the real Claude Code harness through the real embedded
//! engine — the provider's acceptance run and the mcp-bridge's Claude live
//! E2E (the bridged-turn test the O6 implementation left waiting).
//!
//! `#[ignore]`d by default: these burn subscription quota and need an
//! authenticated `claude` CLI on PATH. Run explicitly:
//!
//! ```console
//! cargo test --manifest-path tests/embedded/Cargo.toml --test claude_live -- --ignored
//! ```

use std::{
    net::TcpListener,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

use anyhow::Result;
use odori_agents::{Agent, AgentRegistry, Providers, RunConfig, Tool};
use odori_engine::{ConnectTarget, OdoriRuntime};
use odori_mcp_bridge::BridgeConfig;
use odori_providers::ClaudeProvider;
use serde_json::json;
use tokeira_engine::{Engine, TokeiraConfig};

async fn start_engine() -> Result<(Engine, TcpListener, TcpListener)> {
    let grpc_guard = TcpListener::bind("127.0.0.1:0")?;
    let nexus_guard = TcpListener::bind("127.0.0.1:0")?;
    let mut config = TokeiraConfig::default();
    config.infrastructure.network.grpc_addr = grpc_guard.local_addr()?.to_string();
    config.policy.nexus_completion.http_addr = nexus_guard.local_addr()?.to_string();
    let engine = Engine::start_with_config(config).await?;
    Ok((engine, grpc_guard, nexus_guard))
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "burns subscription quota; needs an authenticated claude CLI"]
async fn live_claude_turn_through_the_embedded_engine() -> Result<()> {
    let mut registry = AgentRegistry::new();
    registry.register(Agent::new(
        "parrot",
        "You reply with exactly what the user asks for, nothing else.",
    ));
    let provider = Arc::new(ClaudeProvider::new());
    let (engine, _g1, _g2) = start_engine().await?;
    let runtime = OdoriRuntime::builder("tq-live-claude")
        .connect(ConnectTarget::service_override(engine.service_override()))
        .agents(registry)
        .providers(Providers::new(provider.clone()))
        .start()
        .await?;

    let config = RunConfig::default()
        .with_turn_timeout(Duration::from_secs(300))
        .with_turn_max_attempts(1);
    let text: String = runtime
        .runner()
        .run_with_config(
            "parrot",
            "Reply with exactly the word: pirouette",
            "run-live-1",
            config,
        )
        .await?;
    assert!(
        text.to_lowercase().contains("pirouette"),
        "live turn answered: {text}"
    );
    assert!(provider.detected_version().is_some());

    runtime.shutdown().await?;
    engine.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "burns subscription quota; needs an authenticated claude CLI"]
async fn live_bridged_tool_call_executes_durably() -> Result<()> {
    let executions = Arc::new(AtomicU32::new(0));
    let counter = executions.clone();
    let mut registry = AgentRegistry::new();
    registry.register(
        Agent::new(
            "facts",
            "You have one tool: record_fact. When asked to record a fact, you MUST call \
             the record_fact tool with the fact as its `fact` argument, then report the \
             tool's response verbatim.",
        )
        .with_tool(Tool::new(
            "record_fact",
            "Record a fact durably. Returns a receipt string.",
            json!({"type": "object", "properties": {"fact": {"type": "string"}},
                   "required": ["fact"]}),
            move |context, args| {
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    let fact = args
                        .pointer("/fact")
                        .and_then(|value| value.as_str())
                        .unwrap_or("<none>");
                    Ok(json!(format!(
                        "receipt: recorded {fact:?} (invocation {})",
                        context.invocation_id
                    )))
                }
            },
        )),
    );
    let provider = Arc::new(ClaudeProvider::new());
    let (engine, _g1, _g2) = start_engine().await?;
    let runtime = OdoriRuntime::builder("tq-live-bridge")
        .connect(ConnectTarget::service_override(engine.service_override()))
        .agents(registry)
        .providers(Providers::new(provider))
        .bridge(BridgeConfig::default())
        .start()
        .await?;

    let config = RunConfig::default().with_turn_timeout(Duration::from_secs(300));
    let text: String = runtime
        .runner()
        .run_with_config(
            "facts",
            "Record this fact: odori danced live on the embedded engine.",
            "run-live-bridge-1",
            config,
        )
        .await?;

    // The tool executed durably (as an activity, through the bridge), and
    // its receipt round-tripped through the harness into the run's answer.
    assert_eq!(
        executions.load(Ordering::SeqCst),
        1,
        "the bridged tool must execute exactly once"
    );
    assert!(
        text.contains("receipt: recorded"),
        "the tool receipt must reach the final answer: {text}"
    );

    runtime.shutdown().await?;
    engine.shutdown().await?;
    Ok(())
}
