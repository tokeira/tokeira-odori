//! Feature: mcp-bridge, Property 7: `preview`-off inertness.
//!
//! The same `Tool`-bearing program compiles and behaves identically under
//! both feature configurations (run the suite with and without
//! `--features preview`); with `preview` off the bridge contributes no
//! listener, no attachment, and no code path — there is nothing to call.
//! The additional preview-only test proves the bridge exists and attaches
//! when (and only when) explicitly wired.

use odori::{Agent, AgentRegistry, Tool};
use serde_json::json;

/// The shared program: identical source under both configurations
/// (Requirement 8.4 — the `Tool` API surface does not change).
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
            provider::{AttachmentSource, McpTransport, TurnIdentity},
            run::{ToolInvocation, ToolInvocationReply},
        },
        mcp_bridge::{Bridge, BridgeConfig, BridgeError, UpdateClient},
    };

    #[derive(Debug)]
    struct NoopUpdateClient;

    #[async_trait]
    impl UpdateClient for NoopUpdateClient {
        async fn tool_invoked(
            &self,
            _workflow_id: &str,
            _invocation: ToolInvocation,
        ) -> Result<ToolInvocationReply, BridgeError> {
            Ok(ToolInvocationReply::Completed(
                odori::agents::ToolCallResult::text("noop"),
            ))
        }
    }

    #[tokio::test]
    async fn bridge_attaches_only_when_wired() {
        let registry = Arc::new(super::tool_bearing_agent());
        let bridge = Bridge::start(
            registry,
            Arc::new(NoopUpdateClient),
            BridgeConfig::default(),
        )
        .await
        .expect("bridge start");
        let identity = TurnIdentity {
            run_id: "r".into(),
            turn: 0,
            attempt: 1,
        };
        let attachment = bridge
            .attachment_for("wf", &identity, "ops")
            .expect("tool-bearing agent attaches");
        assert!(matches!(
            attachment.mcp_server.transport,
            McpTransport::Http { .. }
        ));
        assert_eq!(
            attachment.allowed_tools,
            vec!["mcp__odori__deploy".to_owned()]
        );
        // An agent without framework tools gets no attachment at all.
        assert!(bridge.attachment_for("wf", &identity, "missing").is_none());
    }

    #[test]
    fn keepalive_must_sit_below_the_timeout_pin() {
        let mut config = BridgeConfig::default();
        config.keepalive = std::time::Duration::from_secs(120);
        config.mcp_timeout_pin = Some(std::time::Duration::from_secs(60));
        assert!(config.validate().is_err(), "spec invariant I6");
    }
}
