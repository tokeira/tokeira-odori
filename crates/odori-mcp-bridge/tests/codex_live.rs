//! Quota-gated live proof: Codex app-server → HTTP bridge → workflow update.
#![cfg(feature = "preview")]

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use odori_agents::{
    Agent, AgentRegistry, Tool, ToolCallResult,
    provider::{
        AgentDirectives, AttachmentSource, Provider, SessionDirective, TurnEventSink, TurnIdentity,
        TurnRequest,
    },
    run::{ToolInvocation, ToolInvocationReply},
};
use odori_mcp_bridge::{Bridge, BridgeConfig, BridgeError, UpdateClient};
use odori_providers::CodexProvider;
use serde_json::json;
use tokio::sync::mpsc;

#[derive(Debug)]
struct EchoUpdateClient;

#[async_trait]
impl UpdateClient for EchoUpdateClient {
    async fn tool_invoked(
        &self,
        _workflow_id: &str,
        invocation: ToolInvocation,
    ) -> Result<ToolInvocationReply, BridgeError> {
        // Long enough for the bridge to emit several live progress frames,
        // but below Codex's fixed wall-clock timeout (the Phase-1 probe proved
        // progress does not reset that timeout on the pinned CLI).
        tokio::time::sleep(Duration::from_millis(1_500)).await;
        Ok(ToolInvocationReply::Completed(ToolCallResult::text(
            format!(
                "ran {} for {}",
                invocation.tool, invocation.identity.call_id
            ),
        )))
    }
}

#[tokio::test]
#[ignore = "requires an authenticated Codex subscription, loopback access, and quota"]
async fn codex_calls_framework_tool_through_live_bridge() {
    let mut registry = AgentRegistry::new();
    registry.register(
        Agent::new("ops", "Call the requested framework tool.").with_tool(Tool::new(
            "deploy",
            "Return the bridge's durable deployment marker.",
            json!({"type": "object", "properties": {}}),
            |_context, _args| async { Ok(json!("unused by echo update client")) },
        )),
    );
    let mut config = BridgeConfig::default();
    config.keepalive = Duration::from_millis(250);
    config.mcp_timeout_pin = Some(Duration::from_secs(2));
    let bridge = Bridge::start(Arc::new(registry), Arc::new(EchoUpdateClient), config)
        .await
        .expect("start loopback bridge");

    let identity = TurnIdentity {
        run_id: "codex-live-bridge".into(),
        turn: 0,
        attempt: 1,
    };
    let attachment = bridge
        .attachment_for("workflow-live", &identity, "ops")
        .expect("ops exposes one framework tool");
    let mut request = TurnRequest::new(
        identity,
        AgentDirectives::new("ops", "Use only the Odori deploy tool when asked."),
        "Call the Odori deploy tool exactly once, then reply with exactly its text result.",
        SessionDirective::Start,
    );
    request.tooling.mcp_servers.push(attachment.mcp_server);
    request.tooling.mcp_timeout = attachment.mcp_timeout;
    request.tooling.allowed_native_tools = Some(attachment.allowed_tools);

    let (sender, _receiver) = mpsc::channel(64);
    let outcome = CodexProvider::new()
        .execute_turn(request, TurnEventSink::new(sender))
        .await
        .expect("Codex bridged turn succeeds");
    assert!(
        outcome.text.contains("ran deploy for exec-"),
        "unexpected final text: {}",
        outcome.text
    );
}
