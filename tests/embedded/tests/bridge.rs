//! Crash-mid-turn through the real stack (mcp-bridge spec, Testing
//! Strategy): a scripted MCP-client provider plays the harness against the
//! bridge, the run-loop workflow, `execute_tool`, and the embedded engine —
//! killing itself mid-turn to prove dedupe, and replaying stale attempts to
//! prove fencing. No real harness involved; deterministic by construction.

use std::{
    net::TcpListener,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

use anyhow::Result;
use async_trait::async_trait;
use odori_agents::{
    Agent, AgentRegistry, Providers, Tool,
    provider::{
        McpTransport, Provider, TurnError, TurnEventSink, TurnOutcome, TurnRequest, TurnTooling,
    },
};
use odori_engine::{ConnectTarget, OdoriRuntime};
use odori_mcp_bridge::BridgeConfig;
use serde_json::{Value, json};
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

/// The bridge endpoint and token from a turn's tooling.
fn endpoint(tooling: &TurnTooling) -> (String, String) {
    let server = tooling.mcp_servers.first().expect("bridge attached");
    let McpTransport::Http { url, headers } = &server.transport else {
        panic!("bridge transport must be HTTP");
    };
    (url.clone(), headers[0].1.clone())
}

/// One MCP `tools/call` as a harness would issue it. Returns the full SSE
/// body (progress frames included) and the final JSON-RPC frame.
async fn mcp_call(url: &str, auth: &str, tool: &str, call_id: &str) -> (String, Value) {
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {
            "name": tool,
            "arguments": {"target": "prod"},
            "_meta": {"claudecode/toolUseId": call_id, "progressToken": 7},
        },
    });
    let response = reqwest::Client::new()
        .post(url)
        .header("Authorization", auth)
        .json(&body)
        .send()
        .await
        .expect("bridge reachable")
        .text()
        .await
        .expect("sse body");
    let last = response
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .next_back()
        .expect("final frame");
    let frame: Value = serde_json::from_str(last).expect("json frame");
    (response, frame)
}

async fn mcp_status(url: &str, auth: &str) -> reqwest::StatusCode {
    reqwest::Client::new()
        .post(url)
        .header("Authorization", auth)
        .json(&json!({"jsonrpc": "2.0", "id": 9, "method": "tools/list"}))
        .send()
        .await
        .expect("bridge reachable")
        .status()
}

fn result_text(frame: &Value) -> Option<String> {
    frame
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// A provider that *is* an MCP client: attempt 1 calls the bridge and then
/// dies before delivering; attempt 2 replays the same call id and returns.
#[derive(Debug)]
struct DyingMcpHarness {
    observed: Mutex<Vec<String>>,
    bodies: Mutex<Vec<String>>,
}

#[async_trait]
impl Provider for DyingMcpHarness {
    fn name(&self) -> &str {
        "dying-mcp-harness"
    }

    async fn execute_turn(
        &self,
        request: TurnRequest,
        _events: TurnEventSink,
    ) -> Result<TurnOutcome, TurnError> {
        let (url, auth) = endpoint(&request.tooling);
        let (body, frame) = mcp_call(&url, &auth, "deploy", "tu-dedupe").await;
        let text = result_text(&frame).expect("tool result");
        self.observed.lock().expect("lock").push(text.clone());
        self.bodies.lock().expect("lock").push(body);
        if request.identity.attempt == 1 {
            // Died after the tool executed, before the result reached the
            // model — the spec's crash-mid-turn shape.
            return Err(TurnError::HarnessDied {
                exit_code: Some(137),
                stderr_head: "killed mid-await".to_owned(),
            });
        }
        Ok(TurnOutcome::new("sess-bridge", text))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn crash_mid_turn_replays_without_reexecution() -> Result<()> {
    let executions = Arc::new(AtomicU32::new(0));
    let counter = executions.clone();
    let mut registry = AgentRegistry::new();
    registry.register(Agent::new("ops", "operate").with_tool(Tool::new(
        "deploy",
        "Deploy the target",
        json!({"type": "object", "properties": {"target": {"type": "string"}}}),
        move |_context, _args| {
            let counter = counter.clone();
            async move {
                // Slow enough that keepalive progress must flow (spec
                // Requirement 5.1; the bridge cadence below is 100ms).
                tokio::time::sleep(Duration::from_millis(350)).await;
                let execution = counter.fetch_add(1, Ordering::SeqCst) + 1;
                Ok(json!(format!("deployment-{execution}")))
            }
        },
    )));

    let provider = Arc::new(DyingMcpHarness {
        observed: Mutex::new(Vec::new()),
        bodies: Mutex::new(Vec::new()),
    });
    let (engine, _g1, _g2) = start_engine().await?;
    let mut bridge_config = BridgeConfig::default();
    bridge_config.keepalive = Duration::from_millis(100);
    let runtime = OdoriRuntime::builder("tq-bridge-crash")
        .connect(ConnectTarget::service_override(engine.service_override()))
        .agents(registry)
        .providers(Providers::new(provider.clone()))
        .bridge(bridge_config)
        .start()
        .await?;

    let text: String = runtime
        .runner()
        .run("ops", "deploy prod", "run-bridge-1")
        .await?;

    // The tool ran exactly once across both attempts; both attempts saw the
    // same recorded result; the run's answer is that result.
    assert_eq!(
        executions.load(Ordering::SeqCst),
        1,
        "dedupe failed: tool re-executed"
    );
    let observed = provider.observed.lock().expect("lock").clone();
    assert_eq!(
        observed,
        vec!["deployment-1".to_owned(), "deployment-1".to_owned()]
    );
    assert_eq!(text, "deployment-1");

    // Keepalive progress flowed while the slow execution ran.
    let bodies = provider.bodies.lock().expect("lock").clone();
    assert!(
        bodies[0].contains("notifications/progress"),
        "no keepalive progress in the first attempt's stream"
    );

    runtime.shutdown().await?;
    engine.shutdown().await?;
    Ok(())
}

/// Attempt 1 stashes its attachment and dies; attempt 2 issues its own call
/// first (advancing the fence), then replays through the *stale* attempt-1
/// attachment: a new call must be fenced, the recorded call must be served.
#[derive(Debug)]
struct ZombieMcpHarness {
    stale: Mutex<Option<(String, String)>>,
    fenced_code: Mutex<Option<i64>>,
    replayed_text: Mutex<Option<String>>,
}

#[async_trait]
impl Provider for ZombieMcpHarness {
    fn name(&self) -> &str {
        "zombie-mcp-harness"
    }

    async fn execute_turn(
        &self,
        request: TurnRequest,
        _events: TurnEventSink,
    ) -> Result<TurnOutcome, TurnError> {
        let (url, auth) = endpoint(&request.tooling);
        if request.identity.attempt == 1 {
            let (_, frame) = mcp_call(&url, &auth, "deploy", "tu-a").await;
            result_text(&frame).expect("attempt 1 tool result");
            *self.stale.lock().expect("lock") = Some((url, auth));
            return Err(TurnError::HarnessDied {
                exit_code: Some(137),
                stderr_head: "killed".to_owned(),
            });
        }
        // Attempt 2, own attachment: advances the turn's attempt watermark.
        let (_, frame) = mcp_call(&url, &auth, "deploy", "tu-b").await;
        let own = result_text(&frame).expect("attempt 2 tool result");
        // The zombie now calls through attempt 1's stale attachment.
        let (stale_url, stale_auth) = self.stale.lock().expect("lock").clone().expect("stashed");
        let (_, fenced) = mcp_call(&stale_url, &stale_auth, "deploy", "tu-zombie").await;
        *self.fenced_code.lock().expect("lock") =
            fenced.pointer("/error/code").and_then(Value::as_i64);
        let (_, replayed) = mcp_call(&stale_url, &stale_auth, "deploy", "tu-a").await;
        *self.replayed_text.lock().expect("lock") = result_text(&replayed);
        Ok(TurnOutcome::new("sess-zombie", own))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn stale_attempts_are_fenced_but_served_recorded_results() -> Result<()> {
    let executions = Arc::new(AtomicU32::new(0));
    let counter = executions.clone();
    let mut registry = AgentRegistry::new();
    registry.register(Agent::new("ops", "operate").with_tool(Tool::new(
        "deploy",
        "Deploy the target",
        json!({"type": "object", "properties": {}}),
        move |context, _args| {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(json!(format!("ran-{}", context.invocation_id)))
            }
        },
    )));

    let provider = Arc::new(ZombieMcpHarness {
        stale: Mutex::new(None),
        fenced_code: Mutex::new(None),
        replayed_text: Mutex::new(None),
    });
    let (engine, _g1, _g2) = start_engine().await?;
    let runtime = OdoriRuntime::builder("tq-bridge-fence")
        .connect(ConnectTarget::service_override(engine.service_override()))
        .agents(registry)
        .providers(Providers::new(provider.clone()))
        .bridge(BridgeConfig::default())
        .start()
        .await?;

    let text: String = runtime
        .runner()
        .run("ops", "deploy prod", "run-bridge-2")
        .await?;
    assert_eq!(text, "ran-tu-b");

    // The zombie's fresh call was fenced (spec Requirement 4.2)…
    assert_eq!(
        *provider.fenced_code.lock().expect("lock"),
        Some(-32011),
        "stale-attempt call was not fenced"
    );
    // …its recorded call was served (Requirement 4.3)…
    assert_eq!(
        provider.replayed_text.lock().expect("lock").as_deref(),
        Some("ran-tu-a"),
        "stale-attempt replay was not served from the registry"
    );
    // …and only the two legitimate executions ran (tu-a, tu-b).
    assert_eq!(executions.load(Ordering::SeqCst), 2);

    // The same stale token that reached fencing while the workflow was live
    // is evicted only after the workflow close event is observable.
    let (stale_url, stale_auth) = provider.stale.lock().expect("lock").clone().expect("stashed");
    let mut status = reqwest::StatusCode::OK;
    for _ in 0..100 {
        status = mcp_status(&stale_url, &stale_auth).await;
        if status == reqwest::StatusCode::UNAUTHORIZED {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(status, reqwest::StatusCode::UNAUTHORIZED);

    runtime.shutdown().await?;
    engine.shutdown().await?;
    Ok(())
}
