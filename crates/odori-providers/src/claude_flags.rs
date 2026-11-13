//! Pure flag rendering for the headless Claude Code harness: the
//! spawn-time tool configuration for the Claude provider, implemented against
//! the `ToolConfig` contract. No process is spawned here.

use odori_agents::provider::{McpTransport, ToolConfig};
use serde_json::{Map, Value, json};

/// The CLI arguments a turn's tool configuration adds to a `claude -p` tool call:
/// `--mcp-config <inline json>`, `--allowedTools <list>`, and the MCP
/// timeout pin via environment. Claude 2.1.220 pins MCP timeouts through
/// `MCP_TIMEOUT` and `MCP_TOOL_TIMEOUT`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClaudeToolArgs {
    /// Arguments appended verbatim to the CLI invocation.
    pub args: Vec<String>,
    /// Environment entries set on the spawned process.
    pub env: Vec<(String, String)>,
}

/// Render one turn's tool configuration. Deterministic and total: unknown transports
/// render to their nearest CLI equivalent, and empty tool configuration renders to
/// nothing.
pub fn render_tool_args(tool_config: &ToolConfig) -> ClaudeToolArgs {
    let mut rendered = ClaudeToolArgs::default();

    if !tool_config.mcp_servers.is_empty() {
        let mut servers = Map::new();
        for server in &tool_config.mcp_servers {
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

    if let Some(allowed) = &tool_config.allowed_tools
        && !allowed.is_empty()
    {
        rendered.args.push("--allowedTools".to_owned());
        rendered.args.push(allowed.join(","));
    }

    if let Some(timeout) = tool_config.mcp_timeout {
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
    fn mcp_bootstrap_renders_to_config_allowlist_and_pins() {
        let mut tool_config = ToolConfig::default();
        tool_config.mcp_servers.push(McpServerConfig {
            name: "odori".to_owned(),
            transport: McpTransport::Http {
                url: "http://127.0.0.1:9999/mcp".to_owned(),
                headers: vec![("Authorization".to_owned(), "Bearer tok".to_owned())],
            },
        });
        tool_config.allowed_tools = Some(vec!["mcp__odori__deploy".to_owned()]);
        tool_config.mcp_timeout = Some(Duration::from_secs(120));

        let rendered = render_tool_args(&tool_config);
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
    fn empty_tool_config_renders_to_nothing() {
        assert_eq!(
            render_tool_args(&ToolConfig::default()),
            ClaudeToolArgs::default()
        );
    }

    #[test]
    fn stdio_shim_renders_as_command_config() {
        let mut tool_config = ToolConfig::default();
        tool_config.mcp_servers.push(McpServerConfig {
            name: "odori".to_owned(),
            transport: McpTransport::Stdio {
                command: "odori-shim".to_owned(),
                args: vec!["--forward".to_owned()],
                env: vec![("ODORI_BRIDGE".to_owned(), "http://x/mcp".to_owned())],
            },
        });
        let rendered = render_tool_args(&tool_config);
        let config: Value = serde_json::from_str(&rendered.args[1]).expect("inline json");
        assert_eq!(
            config
                .pointer("/mcpServers/odori/command")
                .and_then(Value::as_str),
            Some("odori-shim")
        );
    }
}
