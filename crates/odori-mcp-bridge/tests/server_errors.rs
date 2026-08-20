//! Unit tests for the MCP server's contract-policy and error-table rows
//! (mcp-bridge spec): auth gate, unserved surfaces, notification handling,
//! tools/list content. Uses a raw HTTP/1.1 client over loopback — no
//! client-side dependencies.
#![cfg(feature = "preview")]

use std::sync::Arc;

use async_trait::async_trait;
use odori_agents::{
    Agent, AgentRegistry, Tool, ToolCallResult,
    provider::{AttachmentSource, McpTransport, TurnIdentity},
    run::{ToolInvocation, ToolInvocationReply},
};
use odori_mcp_bridge::{Bridge, BridgeConfig, BridgeError, UpdateClient};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Debug)]
struct EchoUpdateClient;

#[async_trait]
impl UpdateClient for EchoUpdateClient {
    async fn tool_invoked(
        &self,
        _workflow_id: &str,
        invocation: ToolInvocation,
    ) -> Result<ToolInvocationReply, BridgeError> {
        Ok(ToolInvocationReply::Completed(ToolCallResult::text(
            format!(
                "ran {} for {}",
                invocation.tool, invocation.identity.call_id
            ),
        )))
    }
}

async fn http_post(url: &str, auth: Option<&str>, body: &Value) -> (u16, String) {
    let address = url
        .strip_prefix("http://")
        .and_then(|rest| rest.split_once('/'))
        .expect("bridge url shape")
        .0
        .to_owned();
    let payload = body.to_string();
    let auth_header = auth
        .map(|token| format!("Authorization: Bearer {token}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "POST /mcp HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n{auth_header}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{payload}",
        payload.len()
    );
    let mut stream = tokio::net::TcpStream::connect(&address)
        .await
        .expect("connect");
    stream.write_all(request.as_bytes()).await.expect("write");
    let mut response = String::new();
    stream.read_to_string(&mut response).await.expect("read");
    let status: u16 = response
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .expect("status line");
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_owned())
        .unwrap_or_default();
    (status, body)
}

async fn bridge_with_token() -> (Bridge, String) {
    let mut registry = AgentRegistry::new();
    registry.register(Agent::new("ops", "operate").with_tool(Tool::new(
        "deploy",
        "Deploy the thing",
        json!({"type": "object", "properties": {}}),
        |_context, _args| async { Ok(json!("ok")) },
    )));
    let bridge = Bridge::start(
        Arc::new(registry),
        Arc::new(EchoUpdateClient),
        BridgeConfig::default(),
    )
    .await
    .expect("bridge start");
    let attachment = bridge
        .attachment_for(
            "wf-1",
            &TurnIdentity {
                run_id: "r".into(),
                turn: 0,
                attempt: 1,
            },
            "ops",
        )
        .expect("agent has tools");
    let McpTransport::Http { headers, .. } = &attachment.mcp_server.transport else {
        panic!("bridge attachment must be HTTP");
    };
    let token = headers[0]
        .1
        .strip_prefix("Bearer ")
        .expect("bearer")
        .to_owned();
    (bridge, token)
}

#[tokio::test]
async fn missing_token_is_rejected_before_processing() {
    let (bridge, _token) = bridge_with_token().await;
    let (status, _) = http_post(
        bridge.url(),
        None,
        &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await;
    assert_eq!(status, 401);
    let (status, _) = http_post(
        bridge.url(),
        Some("not-a-real-token"),
        &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await;
    assert_eq!(status, 401);
}

#[tokio::test]
async fn unserved_surfaces_return_method_not_found() {
    let (bridge, token) = bridge_with_token().await;
    for method in ["resources/list", "prompts/list", "completion/complete"] {
        let (status, body) = http_post(
            bridge.url(),
            Some(&token),
            &json!({"jsonrpc": "2.0", "id": 1, "method": method}),
        )
        .await;
        assert_eq!(status, 200);
        let response: Value = serde_json::from_str(&body).expect("json body");
        assert_eq!(
            response.pointer("/error/code").and_then(Value::as_i64),
            Some(-32601)
        );
    }
}

#[tokio::test]
async fn notifications_are_accepted_without_reply() {
    let (bridge, token) = bridge_with_token().await;
    let (status, body) = http_post(
        bridge.url(),
        Some(&token),
        &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )
    .await;
    assert_eq!(status, 202);
    assert!(body.is_empty());
}

#[tokio::test]
async fn initialize_ping_and_tools_list_answer_plain() {
    let (bridge, token) = bridge_with_token().await;
    let (status, body) = http_post(
        bridge.url(),
        Some(&token),
        &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {"protocolVersion": "2025-06-18", "capabilities": {}}}),
    )
    .await;
    assert_eq!(status, 200);
    let response: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(
        response
            .pointer("/result/serverInfo/name")
            .and_then(Value::as_str),
        Some("odori")
    );

    let (_, body) = http_post(
        bridge.url(),
        Some(&token),
        &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    )
    .await;
    let response: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(
        response
            .pointer("/result/tools/0/name")
            .and_then(Value::as_str),
        Some("deploy")
    );
}

#[tokio::test]
async fn tools_call_streams_result_with_harness_call_id() {
    let (bridge, token) = bridge_with_token().await;
    let (status, body) = http_post(
        bridge.url(),
        Some(&token),
        &json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                "params": {"name": "deploy", "arguments": {},
                           "_meta": {"claudecode/toolUseId": "toolu_test1", "progressToken": 2}}}),
    )
    .await;
    assert_eq!(status, 200);
    // SSE frames: the final one carries the JSON-RPC response with the
    // update's result — proving the harness call id crossed into the
    // invocation identity.
    let last = body
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .next_back()
        .expect("final SSE frame");
    let response: Value = serde_json::from_str(last).expect("json frame");
    let text = response
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .expect("text block");
    assert_eq!(text, "ran deploy for toolu_test1");
}
