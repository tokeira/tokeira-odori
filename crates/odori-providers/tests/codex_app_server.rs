//! Scripted app-server supervision regression plus the quota-gated live turn.

use std::time::Duration;

use odori_agents::provider::{
    AgentDirectives, McpServerConfig, McpTransport, Provider, SessionDirective, TurnEvent,
    TurnEventSink, TurnIdentity, TurnRequest,
};
use odori_providers::{CodexProvider, EXPECTED_CODEX_CLI_VERSION};
use tokio::sync::mpsc;

fn request(input: &str) -> TurnRequest {
    TurnRequest::new(
        TurnIdentity {
            run_id: "run-fake".into(),
            turn: 0,
            attempt: 1,
        },
        AgentDirectives::new("fake-agent", "Return the requested result."),
        input,
        SessionDirective::Start,
    )
}

#[cfg(unix)]
fn scripted_codex() -> std::path::PathBuf {
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        time::{SystemTime, UNIX_EPOCH},
    };

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "odori-fake-codex-{}-{nonce}.py",
        std::process::id()
    ));
    let script = format!(
        r#"#!/usr/bin/env python3
import json
import os
import sys

if sys.argv[1:] == ["--version"]:
    print({expected:?})
    raise SystemExit(0)

if "app-server" not in sys.argv:
    print("expected app-server", file=sys.stderr)
    raise SystemExit(2)

def emit(value):
    print(json.dumps(value, separators=(",", ":")), flush=True)

for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    request_id = message.get("id")
    if method == "initialize":
        emit({{"id": request_id, "result": {{"userAgent": "fake"}}}})
    elif method == "initialized":
        pass
    elif method == "thread/start":
        config = message["params"]["config"]
        token_variable = config["mcp_servers.odori.env_http_headers"]["Authorization"]
        if os.environ.get(token_variable) != "Bearer test-token":
            emit({{"id": request_id, "error": {{"code": -32600, "message": "missing MCP token env"}}}})
            continue
        emit({{"method": "thread/started", "params": {{"thread": {{"id": "thread-fake"}}}}}})
        emit({{"id": request_id, "result": {{"thread": {{"id": "thread-fake"}}}}}})
    elif method == "turn/start":
        emit({{"method": "turn/started", "params": {{"threadId": "thread-fake", "turn": {{"id": "turn-fake", "status": "inProgress"}}}}}})
        emit({{"method": "item/started", "params": {{"threadId": "thread-fake", "turnId": "turn-fake", "item": {{"type": "mcpToolCall", "id": "exec-fake", "server": "odori", "tool": "deploy"}}}}}})
        emit({{"id": request_id, "result": {{"turn": {{"id": "turn-fake"}}}}}})
        emit({{"method": "item/completed", "params": {{"threadId": "thread-fake", "turnId": "turn-fake", "item": {{"type": "agentMessage", "id": "msg-fake", "text": "fake-ok", "phase": "final_answer"}}}}}})
        emit({{"method": "thread/tokenUsage/updated", "params": {{"threadId": "thread-fake", "turnId": "turn-fake", "tokenUsage": {{"last": {{"inputTokens": 12, "outputTokens": 3}}}}}}}})
        emit({{"method": "turn/completed", "params": {{"threadId": "thread-fake", "turn": {{"id": "turn-fake", "status": "completed", "durationMs": 42, "error": None}}}}}})
"#,
        expected = EXPECTED_CODEX_CLI_VERSION
    );
    fs::write(&path, script).expect("write fake Codex executable");
    let mut permissions = fs::metadata(&path).expect("fake metadata").permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&path, permissions).expect("make fake executable");
    path
}

#[cfg(unix)]
#[tokio::test]
async fn scripted_app_server_turn_streams_events_and_result() {
    let command = scripted_codex();
    let provider = CodexProvider::with_command(&command);
    let (sender, mut receiver) = mpsc::channel(32);
    let mut turn = request("run the fake turn");
    turn.tooling.mcp_servers.push(McpServerConfig {
        name: "odori".into(),
        transport: McpTransport::Http {
            url: "http://127.0.0.1:9999/mcp".into(),
            headers: vec![("Authorization".into(), "Bearer test-token".into())],
        },
    });
    turn.tooling.allowed_native_tools = Some(vec!["mcp__odori__deploy".into()]);
    turn.tooling.mcp_timeout = Some(Duration::from_secs(120));

    let outcome = provider
        .execute_turn(turn, TurnEventSink::new(sender))
        .await
        .expect("scripted turn succeeds");
    assert_eq!(outcome.session_id, "thread-fake");
    assert_eq!(outcome.text, "fake-ok");
    assert_eq!(outcome.usage.input_tokens, Some(12));
    assert_eq!(outcome.usage.output_tokens, Some(3));
    assert_eq!(outcome.usage.duration, Some(Duration::from_millis(42)));

    let mut events = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        events.push(event);
    }
    assert!(events.iter().any(|event| matches!(
        event,
        TurnEvent::SessionStarted { session_id } if session_id == "thread-fake"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        TurnEvent::ToolUse { name } if name == "mcp__odori__deploy"
    )));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, TurnEvent::Liveness))
    );

    std::fs::remove_file(command).expect("remove fake Codex executable");
}

#[tokio::test]
#[ignore = "requires an authenticated Codex subscription and consumes quota"]
async fn real_codex_turn() {
    let provider = CodexProvider::new();
    let (sender, mut receiver) = mpsc::channel(64);
    let outcome = provider
        .execute_turn(
            request("Reply with exactly: odori-live-ok"),
            TurnEventSink::new(sender),
        )
        .await
        .expect("live Codex turn succeeds");
    assert_eq!(outcome.text.trim(), "odori-live-ok");
    assert!(matches!(
        receiver.recv().await,
        Some(TurnEvent::Liveness | TurnEvent::SessionStarted { .. })
    ));
}
