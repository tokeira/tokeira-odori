//! Pure flag rendering for the headless Claude Code harness: the
//! spawn-time attachment half of the O3 provider, implemented against the
//! frozen `TurnTooling` contract so the driver (lane A) consumes it
//! directly. No process is spawned here.

use odori_agents::provider::{McpTransport, TurnTooling};
use serde_json::{Map, Value, json};

/// The CLI arguments a turn's tooling adds to a `claude -p` invocation:
/// `--mcp-config <inline json>`, `--allowedTools <list>`, and the MCP
/// timeout pin via environment (spec Requirement 5.3; claude 2.1.220 pins
/// MCP timeouts through its environment, per the claude-driver spike's
/// flag-surface notes).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClaudeToolingArgs {
    /// Arguments appended verbatim to the CLI invocation.
    pub args: Vec<String>,
    /// Environment entries set on the spawned process.
    pub env: Vec<(String, String)>,
}

/// Render one turn's tooling. Deterministic and total: unknown transports
/// render to their nearest CLI equivalent, and empty tooling renders to
/// nothing.
pub fn render_tooling(tooling: &TurnTooling) -> ClaudeToolingArgs {
    let mut rendered = ClaudeToolingArgs::default();

    if !tooling.mcp_servers.is_empty() {
        let mut servers = Map::new();
        for server in &tooling.mcp_servers {
            let entry = match &server.transport {
                McpTransport::Http { url, headers } => {
                    let header_map: Map<String, Value> = headers
                        .iter()
                        .map(|(name, value)| (name.clone(), json!(value)))
                        .collect();
                    json!({ "type": "http", "url": url, "headers": header_map })
                }
                McpTransport::Stdio { command, args, env } => {
                    let env_map: Map<String, Value> = env
                        .iter()
                        .map(|(name, value)| (name.clone(), json!(value)))
                        .collect();
                    json!({ "command": command, "args": args, "env": env_map })
                }
            };
            servers.insert(server.name.clone(), entry);
        }
        rendered.args.push("--mcp-config".to_owned());
        rendered
            .args
            .push(json!({ "mcpServers": servers }).to_string());
    }

    if let Some(allowed) = &tooling.allowed_native_tools
        && !allowed.is_empty()
    {
        rendered.args.push("--allowedTools".to_owned());
        rendered.args.push(allowed.join(","));
    }

    if let Some(timeout) = tooling.mcp_timeout {
        let millis = timeout.as_millis().to_string();
        rendered
            .env
            .push(("MCP_TIMEOUT".to_owned(), millis.clone()));
        rendered.env.push(("MCP_TOOL_TIMEOUT".to_owned(), millis));
    }

    rendered
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use odori_agents::provider::McpServerConfig;

    use super::*;

    #[test]
    fn bridge_attachment_renders_to_config_allowlist_and_pins() {
        let mut tooling = TurnTooling::default();
        tooling.mcp_servers.push(McpServerConfig {
            name: "odori".to_owned(),
            transport: McpTransport::Http {
                url: "http://127.0.0.1:9999/mcp".to_owned(),
                headers: vec![("Authorization".to_owned(), "Bearer tok".to_owned())],
            },
        });
        tooling.allowed_native_tools = Some(vec!["mcp__odori__deploy".to_owned()]);
        tooling.mcp_timeout = Some(Duration::from_secs(120));

        let rendered = render_tooling(&tooling);
        assert_eq!(rendered.args[0], "--mcp-config");
        let config: Value = serde_json::from_str(&rendered.args[1]).expect("inline json");
        assert_eq!(
            config
                .pointer("/mcpServers/odori/url")
                .and_then(Value::as_str),
            Some("http://127.0.0.1:9999/mcp")
        );
        assert_eq!(
            config
                .pointer("/mcpServers/odori/headers/Authorization")
                .and_then(Value::as_str),
            Some("Bearer tok")
        );
        assert_eq!(rendered.args[2], "--allowedTools");
        assert_eq!(rendered.args[3], "mcp__odori__deploy");
        assert!(
            rendered
                .env
                .iter()
                .any(|(name, value)| name == "MCP_TIMEOUT" && value == "120000")
        );
    }

    #[test]
    fn empty_tooling_renders_to_nothing() {
        assert_eq!(
            render_tooling(&TurnTooling::default()),
            ClaudeToolingArgs::default()
        );
    }

    #[test]
    fn stdio_shim_renders_as_command_config() {
        let mut tooling = TurnTooling::default();
        tooling.mcp_servers.push(McpServerConfig {
            name: "odori".to_owned(),
            transport: McpTransport::Stdio {
                command: "odori-shim".to_owned(),
                args: vec!["--forward".to_owned()],
                env: vec![("ODORI_BRIDGE".to_owned(), "http://x/mcp".to_owned())],
            },
        });
        let rendered = render_tooling(&tooling);
        let config: Value = serde_json::from_str(&rendered.args[1]).expect("inline json");
        assert_eq!(
            config
                .pointer("/mcpServers/odori/command")
                .and_then(Value::as_str),
            Some("odori-shim")
        );
    }
}
