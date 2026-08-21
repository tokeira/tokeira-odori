//! Quota-gated Codex proof through the real embedded engine and HTTP MCP bridge.
//!
//! This also records the call identity Codex presents after a real MCP timeout.
//! Run explicitly with an authenticated pinned Codex CLI:
//!
//! ```console
//! cargo test --manifest-path tests/embedded/Cargo.toml --test codex_live -- --ignored --nocapture
//! ```

use std::{
    net::TcpListener,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

use anyhow::Result;
use odori_agents::{Agent, AgentRegistry, Providers, RunConfig, Tool};
use odori_engine::{ConnectTarget, OdoriRuntime};
use odori_mcp_bridge::BridgeConfig;
use odori_providers::CodexProvider;
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
#[ignore = "burns subscription quota; needs an authenticated codex CLI"]
#[allow(clippy::print_stderr)] // The observed IDs are copied into the task's DONE record.
async fn live_codex_timeout_retry_executes_durably_with_fresh_identity() -> Result<()> {
    let executions = Arc::new(AtomicU32::new(0));
    let call_ids = Arc::new(Mutex::new(Vec::new()));
    let tool_executions = executions.clone();
    let tool_call_ids = call_ids.clone();
    let mut registry = AgentRegistry::new();
    registry.register(
        Agent::new(
            "facts",
            "You have one tool: record_fact. You MUST call it when asked. If the first call \
             times out, call it exactly one more time, then report the successful tool response \
             verbatim.",
        )
        .with_tool(Tool::new(
            "record_fact",
            "Record a fact durably and return its receipt.",
            json!({"type": "object", "properties": {"fact": {"type": "string"}},
                   "required": ["fact"]}),
            move |context, _args| {
                let executions = tool_executions.clone();
                let call_ids = tool_call_ids.clone();
                async move {
                    let ordinal = executions.fetch_add(1, Ordering::SeqCst);
                    call_ids
                        .lock()
                        .expect("call id lock")
                        .push(context.invocation_id.clone());
                    if ordinal == 0 {
                        // Codex is pinned to one second below. This durable activity
                        // completes even though the MCP client abandons its response.
                        tokio::time::sleep(Duration::from_millis(1_500)).await;
                        return Ok(json!("late-first-result"));
                    }
                    Ok(json!(format!("retry-ok:{}", context.invocation_id)))
                }
            },
        )),
    );

    let provider = Arc::new(CodexProvider::new());
    let (engine, _g1, _g2) = start_engine().await?;
    let mut bridge_config = BridgeConfig::default();
    bridge_config.keepalive = Duration::from_millis(250);
    bridge_config.mcp_timeout_pin = Some(Duration::from_secs(1));
    let runtime = OdoriRuntime::builder("tq-live-codex-bridge")
        .connect(ConnectTarget::service_override(engine.service_override()))
        .agents(registry)
        .providers(Providers::new(provider))
        .bridge(bridge_config)
        .start()
        .await?;

    let config = RunConfig::default()
        .with_turn_timeout(Duration::from_secs(300))
        .with_turn_max_attempts(1);
    let text: String = runtime
        .runner()
        .run_with_config(
            "facts",
            "Record this fact: Codex reached a durable Odori tool. If the call times out, \
             retry it exactly once. Reply with exactly the successful tool result.",
            "run-live-codex-bridge-1",
            config,
        )
        .await?;

    let observed_ids = call_ids.lock().expect("call id lock").clone();
    eprintln!("Codex durable timeout retry observation: call_ids={observed_ids:?}");
    assert_eq!(
        executions.load(Ordering::SeqCst),
        2,
        "a fresh retry identity should execute as a second durable invocation"
    );
    assert_eq!(observed_ids.len(), 2);
    assert_ne!(
        observed_ids[0], observed_ids[1],
        "Codex unexpectedly reused the timed-out call id"
    );
    assert!(
        text.contains("retry-ok:"),
        "the second durable receipt must reach the final answer: {text}"
    );

    runtime.shutdown().await?;
    engine.shutdown().await?;
    Ok(())
}
