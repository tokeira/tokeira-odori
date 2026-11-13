//! `preview`-off inertness.
//!
//! The same `Tool`-bearing program compiles and behaves identically under
//! both feature configurations (run the suite with and without
//! `--features preview`); with `preview` off the MCP tool server contributes no
//! listener, no attachment, and no code path — there is nothing to call.
//! The additional preview-only test proves the MCP tool server exists and attaches
//! when (and only when) explicitly wired.

use odori::{Agent, AgentRegistry, Tool};
use serde_json::json;

/// The shared program: identical source under both configurations
fn tool_bearing_agent() -> AgentRegistry {
    let mut registry = AgentRegistry::new();
    registry.register(Agent::new("ops", "operate carefully").with_tool(Tool::new(
        "deploy",
        "Deploy the thing",
        json!({"type": "object", "properties": {"target": {"type": "string"}}}),
        |context, args| async move {
            Ok(json!(format!(
                "deployed {} for {}",
                args.pointer("/target")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?"),
                context.invocation_id,
            )))
        },
    )));
    registry
}

#[test]
fn tool_bearing_program_is_identical_under_both_configurations() {
    let registry = tool_bearing_agent();
    let agent = registry.get("ops").expect("registered");
    assert_eq!(agent.tools().len(), 1);
    assert_eq!(agent.tools()[0].name(), "deploy");
}

#[cfg(feature = "preview")]
mod preview_on {
    use std::sync::Arc;

    use async_trait::async_trait;
    use odori::{
        agents::{
            provider::{McpBootstrapper, McpTransport, TurnId},
            run::{ToolCallReply, ToolCallRequest},
        },
        mcp_bridge::{McpToolServer, McpToolServerConfig, McpToolServerError, ToolCallDispatcher},
    };

    #[derive(Debug)]
    struct NoopToolCallDispatcher;

    #[async_trait]
    impl ToolCallDispatcher for NoopToolCallDispatcher {
        async fn dispatch(
            &self,
            _workflow_id: &str,
            _invocation: ToolCallRequest,
        ) -> Result<ToolCallReply, McpToolServerError> {
            Ok(ToolCallReply::Completed(
                odori::agents::ToolCallResult::text("noop"),
            ))
        }
    }

    #[tokio::test]
    async fn mcp_bootstrap_runs_only_when_wired() {
        let registry = Arc::new(super::tool_bearing_agent());
        let mcp_server = McpToolServer::start(
            registry,
            Arc::new(NoopToolCallDispatcher),
            McpToolServerConfig::default(),
        )
        .await
        .expect("start MCP tool server");
        let turn_id = TurnId {
            run_id: "r".into(),
            turn: 0,
            attempt: 1,
        };
        let mcp_config = mcp_server
            .bootstrap_mcp("wf", &turn_id, "ops")
            .expect("tool-bearing agent attaches");
        assert!(matches!(
            mcp_config.mcp_server.transport,
            McpTransport::Http { .. }
        ));
        assert_eq!(
            mcp_config.allowed_tools,
            vec!["mcp__odori__deploy".to_owned()]
        );
        // An agent without framework tools gets no attachment at all.
        assert!(
            mcp_server
                .bootstrap_mcp("wf", &turn_id, "missing")
                .is_none()
        );
    }

    #[test]
    fn keepalive_must_sit_below_the_timeout_pin() {
        let mut config = McpToolServerConfig::default();
        config.keepalive = std::time::Duration::from_secs(120);
        config.mcp_timeout_pin = Some(std::time::Duration::from_secs(60));
        assert!(config.validate().is_err(), "spec invariant I6");
    }
}
