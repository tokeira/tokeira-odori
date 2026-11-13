//! Quota-gated live proof: Codex app-server → HTTP MCP tool server → workflow update.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use odori_agents::{
    Agent, AgentRegistry, Tool, ToolCallResult,
    provider::{
        AgentDirectives, McpBootstrapper, Provider, SessionDirective, TurnEventSink, TurnId,
        TurnRequest,
    },
    run::{ToolCallReply, ToolCallRequest},
};
use odori_mcp_bridge::{
    McpToolServer, McpToolServerConfig, McpToolServerError, ToolCallDispatcher,
};
use odori_providers::CodexProvider;
use serde_json::json;
use tokio::sync::mpsc;

#[derive(Debug)]
struct EchoToolCallDispatcher;

#[async_trait]
impl ToolCallDispatcher for EchoToolCallDispatcher {
    async fn dispatch(
        &self,
        _workflow_id: &str,
        tool_call: ToolCallRequest,
    ) -> Result<ToolCallReply, McpToolServerError> {
        // Long enough for the MCP tool server to emit several live progress frames,
        // but below Codex's fixed wall-clock timeout (the Phase-1 probe proved
        // progress does not reset that timeout on the pinned CLI).
        tokio::time::sleep(Duration::from_millis(1_500)).await;
        Ok(ToolCallReply::Completed(ToolCallResult::text(format!(
            "ran {} for {}",
            tool_call.tool, tool_call.tool_call_id.call_id
        ))))
    }
}

#[tokio::test]
#[ignore = "requires an authenticated Codex subscription, loopback access, and quota"]
async fn codex_calls_framework_tool_through_live_mcp_server() {
    let mut registry = AgentRegistry::new();
    registry.register(
        Agent::new("ops", "Call the requested framework tool.").with_tool(Tool::new(
            "deploy",
            "Return the mcp_server's durable deployment marker.",
            json!({"type": "object", "properties": {}}),
            |_context, _args| async { Ok(json!("unused by echo update client")) },
        )),
    );
    let mut config = McpToolServerConfig::default();
    config.keepalive = Duration::from_millis(250);
    config.mcp_timeout_pin = Some(Duration::from_secs(2));
    let mcp_server =
        McpToolServer::start(Arc::new(registry), Arc::new(EchoToolCallDispatcher), config)
            .await
            .expect("start loopback MCP tool server");

    let turn_id = TurnId {
        run_id: "codex-live-bridge".into(),
        turn: 0,
        attempt: 1,
    };
    let mcp_config = mcp_server
        .bootstrap_mcp("workflow-live", &turn_id, "ops")
        .expect("ops exposes one framework tool");
    let mut request = TurnRequest::new(
        turn_id,
        AgentDirectives::new("ops", "Use only the Odori deploy tool when asked."),
        "Call the Odori deploy tool exactly once, then reply with exactly its text result.",
        SessionDirective::Start,
    );
    request.tool_config.mcp_servers.push(mcp_config.mcp_server);
    request.tool_config.mcp_timeout = mcp_config.mcp_timeout;
    request.tool_config.allowed_tools = Some(mcp_config.allowed_tools);

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
