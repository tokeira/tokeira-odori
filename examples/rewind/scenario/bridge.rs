//! The scripted harness's small HTTP MCP client.

use odori::agents::provider::{McpTransport, TurnError, TurnTooling};
use serde_json::{Value, json};

pub(super) fn tooling(error: impl std::fmt::Display) -> TurnError {
    TurnError::Tooling {
        message: error.to_string(),
    }
}

fn endpoint(tooling_config: &TurnTooling) -> Result<(String, String), TurnError> {
    let server = tooling_config
        .mcp_servers
        .first()
        .ok_or_else(|| tooling("the durable bridge was not attached"))?;
    let McpTransport::Http { url, headers } = &server.transport else {
        return Err(tooling("the example requires the HTTP bridge"));
    };
    let authorization = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
        .map(|(_, value)| value.clone())
        .ok_or_else(|| tooling("bridge attachment omitted authorization"))?;
    Ok((url.clone(), authorization))
}

pub(super) async fn call_tool(
    tooling_config: &TurnTooling,
    name: &str,
    arguments: Value,
    call_id: &str,
) -> Result<Value, TurnError> {
    let (url, authorization) = endpoint(tooling_config)?;
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
        .map_err(tooling)?
        .text()
        .await
        .map_err(tooling)?;
    let frame = body
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .next_back()
        .ok_or_else(|| tooling("bridge returned no final SSE frame"))?;
    serde_json::from_str(frame).map_err(tooling)
}

pub(super) fn tool_text(frame: &Value) -> Result<String, TurnError> {
    if let Some(error) = frame.pointer("/error/message").and_then(Value::as_str) {
        return Err(tooling(error));
    }
    frame
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| tooling(format!("tool response has no text result: {frame}")))
}
