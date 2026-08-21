//! Handoff integration: a scripted harness calls the model-visible handoff
//! tool through the real HTTP bridge, the parent starts the target's
//! `AgentRun` as a child workflow, and child spend is accounted to both runs.

use std::{
    net::TcpListener,
    sync::{Arc, Mutex},
};

use anyhow::Result;
use async_trait::async_trait;
use odori_agents::{
    Agent, AgentRegistry, Handoff, Providers, RunBudget, RunEnd,
    provider::{
        McpTransport, Provider, TurnError, TurnEvent, TurnEventSink, TurnOutcome, TurnRequest,
        TurnTooling, TurnUsage,
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

fn endpoint(tooling: &TurnTooling) -> (String, String) {
    let server = tooling.mcp_servers.first().expect("bridge attached");
    let McpTransport::Http { url, headers } = &server.transport else {
        panic!("bridge transport must be HTTP");
    };
    (url.clone(), headers[0].1.clone())
}

async fn call_handoff(url: &str, auth: &str) -> String {
    let response = reqwest::Client::new()
        .post(url)
        .header("Authorization", auth)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "transfer_to_specialist",
                "arguments": {"input": "analyze the delegated evidence"},
                "_meta": {"callId": "handoff-call-1"},
            },
        }))
        .send()
        .await
        .expect("bridge reachable")
        .text()
        .await
        .expect("SSE body");
    let last = response
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .next_back()
        .expect("final frame");
    let frame: Value = serde_json::from_str(last).expect("JSON-RPC frame");
    frame
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .expect("handoff text result")
        .to_owned()
}

#[derive(Debug, Default)]
struct HandoffHarness {
    child_inputs: Mutex<Vec<String>>,
}

#[async_trait]
impl Provider for HandoffHarness {
    fn name(&self) -> &str {
        "handoff-harness"
    }

    async fn execute_turn(
        &self,
        request: TurnRequest,
        events: TurnEventSink,
    ) -> Result<TurnOutcome, TurnError> {
        let session_id = format!("session-{}", request.directives.name);
        events.emit(TurnEvent::SessionStarted {
            session_id: session_id.clone(),
        });
        if request.directives.name == "parent" {
            let (url, auth) = endpoint(&request.tooling);
            let text = call_handoff(&url, &auth).await;
            let mut usage = TurnUsage::default();
            usage.input_tokens = Some(10);
            usage.output_tokens = Some(5);
            usage.total_cost_usd = Some(0.10);
            events.report_usage(usage.clone());
            let mut outcome = TurnOutcome::new(session_id, text);
            outcome.usage = usage;
            return Ok(outcome);
        }

        self.child_inputs
            .lock()
            .expect("child inputs lock")
            .push(request.input.clone());
        let mut usage = TurnUsage::default();
        usage.input_tokens = Some(7);
        usage.output_tokens = Some(3);
        usage.total_cost_usd = Some(0.20);
        events.report_usage(usage.clone());
        let mut outcome = TurnOutcome::new(session_id, "specialist-result");
        outcome.usage = usage;
        Ok(outcome)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn handoff_is_child_workflow_and_counts_against_parent_budget() -> Result<()> {
    let mut registry = AgentRegistry::new();
    registry.register(
        Agent::new("parent", "delegate specialist work")
            .with_provider("handoff-harness")
            .with_budget(
                RunBudget::unlimited()
                    .with_max_turns(5)
                    .with_max_total_tokens(100)
                    .with_max_cost_usd(1.0),
            )
            .with_handoff(Handoff::new("specialist")),
    );
    registry.register(
        Agent::new("specialist", "analyze delegated work")
            .with_provider("handoff-harness")
            .with_budget(
                RunBudget::unlimited()
                    .with_max_turns(1)
                    .with_max_total_tokens(50)
                    .with_max_cost_usd(0.5),
            ),
    );
    let provider = Arc::new(HandoffHarness::default());
    let (engine, _g1, _g2) = start_engine().await?;
    let runtime = OdoriRuntime::builder("tq-handoff")
        .connect(ConnectTarget::service_override(engine.service_override()))
        .agents(registry)
        .providers(Providers::new(provider.clone()))
        .bridge(BridgeConfig::default())
        .start()
        .await?;

    let conversation = runtime
        .runner()
        .start_conversation("parent", "delegate this", "run-handoff-1")
        .await?;
    let output = conversation.end().await?;

    assert_eq!(output.text, "specialist-result");
    assert_eq!(output.end, RunEnd::ConversationEnded);
    assert_eq!(output.turns, 2, "one parent turn plus one child turn");
    assert_eq!(output.usage.input_tokens, 17);
    assert_eq!(output.usage.output_tokens, 8);
    assert!((output.usage.total_cost_usd - 0.30).abs() < f64::EPSILON);
    assert_eq!(
        provider.child_inputs.lock().expect("child inputs lock").as_slice(),
        ["analyze the delegated evidence"]
    );

    runtime.shutdown().await?;
    engine.shutdown().await?;
    Ok(())
}
