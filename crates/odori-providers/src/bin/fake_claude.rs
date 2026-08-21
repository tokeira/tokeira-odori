//! The scripted Claude Code test double: speaks just enough stream-json
//! for the provider's supervision, classification, and bridge-attachment
//! paths to be tested without a subscription — including replaying the
//! real protocol's traps (a `task_summary` after `result`, synthetic
//! assistant messages on API failure, the reason-on-stderr resume miss).
//!
//! Modes via `FAKE_CLAUDE_MODE`: `echo` (default), `api_error`, `auth`,
//! `usage_cap`, `resume_missing`, `die`, `mcp`. The `mcp` mode reads the
//! `--mcp-config` argument and performs a real `tools/call` against the
//! bridge over loopback HTTP, like the harness it stands in for.

// A CLI test double writes its protocol to stdout (and its scripted
// failure reasons to stderr) by definition; the workspace's
// tracing-not-println rule is for library and service code.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::{
    io::{Read, Write},
    net::TcpStream,
};

use serde_json::{Value, json};

fn arg_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn emit(value: &Value) {
    println!("{value}");
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--version") {
        let version = std::env::var("FAKE_CLAUDE_VERSION")
            .unwrap_or_else(|_| "2.1.220 (Claude Code)".to_owned());
        println!("{version}");
        return;
    }

    let mode = std::env::var("FAKE_CLAUDE_MODE").unwrap_or_else(|_| "echo".to_owned());
    let session = std::env::var("FAKE_CLAUDE_SESSION").unwrap_or_else(|_| "sess-fake".to_owned());
    let prompt = arg_value(&args, "-p").unwrap_or_default();

    emit(&json!({"type": "system", "subtype": "init", "session_id": session}));

    match mode.as_str() {
        "api_error" => {
            // The real CLI fabricates an assistant message before an API
            // failure; classification must ignore it.
            emit(
                &json!({"type": "assistant", "message": {"model": "<synthetic>",
                "content": [{"type": "text", "text": "api failure"}]}}),
            );
            emit(
                &json!({"type": "result", "subtype": "success", "is_error": true,
                "session_id": session, "result": "API error after retries",
                "terminal_reason": "api_error"}),
            );
            std::process::exit(1);
        }
        "auth" => {
            emit(
                &json!({"type": "result", "subtype": "success", "is_error": true,
                "session_id": session,
                "result": "Failed to authenticate: OAuth session expired",
                "terminal_reason": "api_error"}),
            );
            std::process::exit(1);
        }
        "usage_cap" => {
            emit(
                &json!({"type": "result", "subtype": "success", "is_error": true,
                "session_id": session,
                "result": "usage limit reached for this window",
                "terminal_reason": "api_error"}),
            );
            std::process::exit(1);
        }
        "resume_missing" => {
            emit(
                &json!({"type": "result", "subtype": "error_during_execution",
                "is_error": true, "session_id": session, "num_turns": 0}),
            );
            eprintln!("No conversation found with session ID: {session}");
            std::process::exit(1);
        }
        "die" => {
            if let Ok(tool) = std::env::var("FAKE_CLAUDE_PENDING_TOOL") {
                emit(&json!({"type": "assistant", "message": {"content": [
                    {"type": "tool_use", "id": "tu-pending-1", "name": tool}]}}));
            }
            eprintln!("killed mid-turn");
            std::process::exit(1);
        }
        "mcp" => {
            let config: Value = arg_value(&args, "--mcp-config")
                .and_then(|raw| serde_json::from_str(&raw).ok())
                .unwrap_or_else(|| {
                    eprintln!("mcp mode requires --mcp-config");
                    std::process::exit(1);
                });
            let (_name, server) = config["mcpServers"]
                .as_object()
                .and_then(|servers| servers.iter().next())
                .map(|(name, server)| (name.clone(), server.clone()))
                .unwrap_or_else(|| {
                    eprintln!("mcp config held no servers");
                    std::process::exit(1);
                });
            let url = server["url"].as_str().unwrap_or_default().to_owned();
            let auth = server["headers"]["Authorization"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            let tool = std::env::var("FAKE_CLAUDE_TOOL").unwrap_or_else(|_| "deploy".to_owned());
            let call_id =
                std::env::var("FAKE_CLAUDE_CALL_ID").unwrap_or_else(|_| "tu-fake-1".to_owned());
            emit(&json!({"type": "assistant", "message": {"content": [
                {"type": "tool_use", "id": call_id, "name": format!("mcp__odori__{tool}")}]}}));
            let reply = mcp_tools_call(&url, &auth, &tool, &call_id);
            let text = reply
                .pointer("/result/content/0/text")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| format!("bridge error: {reply}"));
            if std::env::var("FAKE_CLAUDE_DIE_AFTER_CALL").is_ok() {
                // Death between execution and delivery: the crash-mid-turn
                // shape, this time through the real provider.
                eprintln!("killed after tool call");
                std::process::exit(1);
            }
            emit(&json!({"type": "user", "message": {"content": [
                {"type": "tool_result", "tool_use_id": call_id}]}}));
            emit(&json!({"type": "assistant", "message": {"content": [
                {"type": "text", "text": text}]}}));
            emit(
                &json!({"type": "result", "subtype": "success", "is_error": false,
                "session_id": session, "result": text, "terminal_reason": "completed",
                "total_cost_usd": 0.01, "duration_ms": 42,
                "usage": {"input_tokens": 10, "output_tokens": 5}}),
            );
            emit(&json!({"type": "system", "subtype": "task_summary"}));
        }
        _ => {
            // Echo mode: reflect the invocation so tests assert the
            // provider's argument rendering from the outside.
            if std::env::var("FAKE_CLAUDE_REQUIRE_SCRUBBED").is_ok()
                && std::env::var("ANTHROPIC_BASE_URL").is_ok()
            {
                emit(
                    &json!({"type": "result", "subtype": "success", "is_error": true,
                    "session_id": session,
                    "result": "Failed to authenticate: inherited ANTHROPIC_BASE_URL",
                    "terminal_reason": "api_error"}),
                );
                std::process::exit(1);
            }
            let mut markers = Vec::new();
            if let Some(resumed) = arg_value(&args, "--resume") {
                markers.push(format!("resumed={resumed}"));
            }
            if args.iter().any(|arg| arg == "--fork-session") {
                markers.push("forked".to_owned());
            }
            if let Some(system) = arg_value(&args, "--append-system-prompt") {
                markers.push(format!("system={system}"));
            }
            if let Some(model) = arg_value(&args, "--model") {
                markers.push(format!("model={model}"));
            }
            if arg_value(&args, "--json-schema").is_some() {
                markers.push("schema".to_owned());
            }
            let text = format!("echo: {prompt} [{}]", markers.join(" "));
            emit(&json!({"type": "assistant", "message": {"content": [
                {"type": "text", "text": text}]}}));
            emit(
                &json!({"type": "result", "subtype": "success", "is_error": false,
                "session_id": session, "result": text, "terminal_reason": "completed",
                "total_cost_usd": 0.02, "duration_ms": 40,
                "usage": {"input_tokens": 7, "output_tokens": 3}}),
            );
            // Real protocol trap: a trailing frame after the result.
            emit(&json!({"type": "system", "subtype": "task_summary"}));
        }
    }
}

/// One blocking `tools/call` over loopback HTTP, SSE response parsed to its
/// final frame — the shape the real harness's MCP client produces.
fn mcp_tools_call(url: &str, auth: &str, tool: &str, call_id: &str) -> Value {
    let address = url
        .strip_prefix("http://")
        .and_then(|rest| rest.split_once('/'))
        .map(|(address, _)| address.to_owned())
        .unwrap_or_default();
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": tool, "arguments": {"target": "prod"},
                   "_meta": {"claudecode/toolUseId": call_id, "progressToken": 1}}})
    .to_string();
    let request = format!(
        "POST /mcp HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nAuthorization: {auth}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let mut stream = TcpStream::connect(&address).unwrap_or_else(|error| {
        eprintln!("bridge unreachable at {address}: {error}");
        std::process::exit(1);
    });
    stream.write_all(request.as_bytes()).ok();
    let mut response = String::new();
    stream.read_to_string(&mut response).ok();
    response
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .next_back()
        .and_then(|frame| serde_json::from_str(frame).ok())
        .unwrap_or_else(|| {
            eprintln!("no final SSE frame from the bridge");
            std::process::exit(1);
        })
}
