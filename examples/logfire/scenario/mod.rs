//! A deterministic two-turn conversation with one durable tool call: the
//! smallest run that produces the full agent-observability tree (run →
//! turns → tool execution) without consuming any vendor quota.

use std::{
    net::TcpListener,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::Result;
use async_trait::async_trait;
use odori::{
    Agent, AgentRegistry, Providers, RunOutput, Tool,
    agents::provider::{
        McpTransport, Provider, TurnError, TurnEvent, TurnEventSink, TurnOutcome, TurnRequest,
        TurnTooling, TurnUsage,
    },
};
use odori_engine::{
    ConnectTarget, EmbeddedEngineConfig, EmbeddedStorageConfig, Engine, OdoriRuntime, TokeiraConfig,
};
use odori_mcp_bridge::BridgeConfig;
use serde_json::{Value, json};

/// What one scripted conversation produced, for verification.
#[derive(Debug)]
pub struct ScenarioReport {
    /// The run's final output.
    pub output: RunOutput,
    /// Notes recorded by the durable `save_note` tool, in execution order.
    pub saved_notes: Vec<String>,
}

/// Per-turn scripted usage, deliberately distinct so token totals are
/// attributable to their turn in a trace.
fn scripted_usage(turn: u32) -> TurnUsage {
    let mut usage = TurnUsage::default();
    usage.total_cost_usd = Some(0.0025 * f64::from(turn + 1));
    usage.input_tokens = Some(640 + u64::from(turn) * 100);
    usage.output_tokens = Some(120 + u64::from(turn) * 40);
    usage.duration = Some(Duration::from_millis(350));
    usage
}

fn tooling_error(error: impl std::fmt::Display) -> TurnError {
    TurnError::Tooling {
        message: error.to_string(),
    }
}

/// The scripted stand-in for a subscription harness: it emits the same
/// event stream a real provider would (session identity, tool-use
/// liveness, usage snapshots) and drives one durable framework tool call
/// through the attached mcp-bridge on the first turn.
#[derive(Debug)]
struct ScriptedProvider;

#[async_trait]
impl Provider for ScriptedProvider {
    fn name(&self) -> &str {
        "logfire-scripted"
    }

    async fn execute_turn(
        &self,
        request: TurnRequest,
        events: TurnEventSink,
    ) -> Result<TurnOutcome, TurnError> {
        let turn = request.identity.turn;
        let session_id = format!("logfire-session-{turn}");
        events.emit(TurnEvent::SessionStarted {
            session_id: session_id.clone(),
        });

        let text = if turn == 0 {
            // Turn 0: the "model" records a note through the durable bridge
            // tool before answering.
            events.emit(TurnEvent::ToolUse {
                name: "save_note".to_owned(),
            });
            let receipt = call_bridge_tool(
                &request.tooling,
                "save_note",
                json!({"text": "pack the telescope"}),
                "logfire-call-0",
            )
            .await?;
            format!("Noted: {receipt}")
        } else {
            "Two notes stand; nothing else is pending.".to_owned()
        };

        events.report_usage(scripted_usage(turn));
        let mut outcome = TurnOutcome::new(session_id, text);
        outcome.usage = scripted_usage(turn);
        Ok(outcome)
    }
}

/// The scripted harness's minimal HTTP MCP client (the same wire shape a
/// real harness uses against the bridge attachment).
async fn call_bridge_tool(
    tooling: &TurnTooling,
    name: &str,
    arguments: Value,
    call_id: &str,
) -> Result<String, TurnError> {
    let server = tooling
        .mcp_servers
        .first()
        .ok_or_else(|| tooling_error("the durable bridge was not attached"))?;
    let McpTransport::Http { url, headers } = &server.transport else {
        return Err(tooling_error("the example requires the HTTP bridge"));
    };
    let authorization = headers
        .iter()
        .find(|(header, _)| header.eq_ignore_ascii_case("authorization"))
        .map(|(_, value)| value.clone())
        .ok_or_else(|| tooling_error("bridge attachment omitted authorization"))?;
    let body = reqwest::Client::new()
        .post(url)
        .header("Authorization", authorization)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": call_id,
            "method": "tools/call",
            "params": {
                "name": name,
                "arguments": arguments,
                "_meta": {"odori/callId": call_id},
            },
        }))
        .send()
        .await
        .map_err(tooling_error)?
        .text()
        .await
        .map_err(tooling_error)?;
    let frame = body
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .next_back()
        .ok_or_else(|| tooling_error("bridge returned no final SSE frame"))?;
    let frame: Value = serde_json::from_str(frame).map_err(tooling_error)?;
    if let Some(message) = frame.pointer("/error/message").and_then(Value::as_str) {
        return Err(tooling_error(message));
    }
    frame
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| tooling_error(format!("tool response has no text result: {frame}")))
}

fn agents(notes: Arc<Mutex<Vec<String>>>) -> AgentRegistry {
    let save_note = Tool::new(
        "save_note",
        "Persist one note durably and return its receipt.",
        json!({
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"],
            "additionalProperties": false,
        }),
        move |context, args| {
            let notes = notes.clone();
            async move {
                let text = args
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_owned();
                let ordinal = {
                    let mut notes = notes.lock().expect("saved notes lock");
                    notes.push(text.clone());
                    notes.len()
                };
                Ok(json!(format!(
                    "note-{ordinal}:{text}:{}",
                    context.invocation_id
                )))
            }
        },
    );
    let mut registry = AgentRegistry::new();
    registry.register(
        Agent::new(
            "day-planner",
            "Keep a durable plan for the day; save notes before answering.",
        )
        .with_provider("logfire-scripted")
        .with_tool(save_note),
    );
    registry
}

/// Start the embedded engine on ephemeral ports and run the scripted
/// two-turn conversation.
pub async fn run_scripted_conversation(storage: EmbeddedStorageConfig) -> Result<ScenarioReport> {
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

    let notes = Arc::new(Mutex::new(Vec::new()));
    let runtime = OdoriRuntime::builder("example-logfire")
        .connect(ConnectTarget::service_override(engine.service_override()))
        .agents(agents(notes.clone()))
        .providers(Providers::new(Arc::new(ScriptedProvider)))
        .bridge(BridgeConfig::default())
        .start()
        .await?;

    let conversation = runtime
        .runner()
        .start_conversation("day-planner", "Plan tomorrow's stargazing trip.", "logfire-1")
        .await?;
    conversation.send("Anything left before sunset?").await?;
    let output = conversation.end().await?;

    runtime.shutdown().await?;
    engine.shutdown().await?;

    let saved_notes = notes.lock().expect("saved notes lock").clone();
    Ok(ScenarioReport {
        output,
        saved_notes,
    })
}
