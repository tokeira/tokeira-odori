//! Rust API renames must preserve payloads recorded by earlier releases.

use odori_agents::{
    provider::{McpConfig, TurnRequest},
    run::{ExecuteToolInput, ToolCallRequest},
};
use serde_json::json;

#[test]
fn renamed_turn_and_tool_config_fields_preserve_legacy_payloads() {
    let recorded = json!({
        "identity": { "run_id": "run-1", "turn": 2, "attempt": 3 },
        "directives": {
            "name": "ops", "instructions": "help", "model": null,
            "output_schema": null
        },
        "input": "hello", "session": "Start", "deadline": null,
        "tooling": {
            "allowed_native_tools": ["Bash", "mcp__odori__lookup"],
            "framework_tools": ["lookup"], "mcp_servers": [], "mcp_timeout": null
        }
    });
    let request: TurnRequest = serde_json::from_value(recorded.clone()).expect("legacy request");
    assert_eq!(request.turn_id.attempt, 3);
    assert_eq!(request.tool_config.framework_tool_names, ["lookup"]);
    assert_eq!(
        serde_json::to_value(request).expect("serialize request"),
        recorded
    );
}

#[test]
fn renamed_tool_call_fields_preserve_recorded_identifiers() {
    let recorded = json!({
        "identity": { "turn": 2, "attempt": 3, "call_id": "call-1" },
        "tool": "lookup", "arguments": { "order": 123 }
    });
    let request: ToolCallRequest =
        serde_json::from_value(recorded.clone()).expect("legacy tool call");
    assert_eq!(request.tool_call_id.call_id, "call-1");
    assert_eq!(
        serde_json::to_value(request).expect("serialize tool call"),
        recorded
    );

    let mut recorded_execution = recorded;
    recorded_execution["agent"] = json!("ops");
    let input: ExecuteToolInput =
        serde_json::from_value(recorded_execution.clone()).expect("legacy tool execution");
    assert_eq!(input.tool_call_id.attempt, 3);
    assert_eq!(
        serde_json::to_value(input).expect("serialize execution"),
        recorded_execution
    );
}

#[test]
fn mcp_bootstrap_config_keeps_its_existing_allowlist_key() {
    let recorded = json!({
        "mcp_server": {
            "name": "odori",
            "transport": { "Http": { "url": "http://127.0.0.1:1234/mcp", "headers": [] } }
        },
        "mcp_timeout": null, "allowed_tools": ["mcp__odori__lookup"]
    });
    let config: McpConfig = serde_json::from_value(recorded.clone()).expect("legacy MCP config");
    assert_eq!(config.allowed_tools, ["mcp__odori__lookup"]);
    assert_eq!(
        serde_json::to_value(config).expect("serialize MCP config"),
        recorded
    );
}
