//! Unguarded (CI-safe) tests of the Claude provider against the scripted
//! test double (`odori-fake-claude`), covering supervision, argument
//! rendering, the exit-classification taxonomy — including
//! died-awaiting-MCP — and a real bridged `tools/call` through the
//! bridge's loopback server, all without a subscription. Fake modes are
//! selected per provider instance via `ClaudeConfig::with_env`, so tests
//! stay process-state-free.

use std::{sync::Arc, time::Duration};

use odori_agents::provider::{
    AgentDirectives, Provider, SessionDirective, TurnError, TurnEvent, TurnEventSink, TurnIdentity,
    TurnRequest,
};
use odori_providers::{ClaudeConfig, ClaudeProvider, PINNED_VERSION};
use serde_json::json;
use tokio::sync::mpsc;

fn provider_with(env: &[(&str, &str)]) -> ClaudeProvider {
    let mut config = ClaudeConfig::default().with_binary(env!("CARGO_BIN_EXE_odori-fake-claude"));
    for (name, value) in env {
        config = config.with_env(*name, *value);
    }
    ClaudeProvider::with_config(config)
}

fn request(input: &str, session: SessionDirective) -> TurnRequest {
    let mut request = TurnRequest::new(
        TurnIdentity {
            run_id: "run-test".to_owned(),
            turn: 0,
            attempt: 1,
        },
        AgentDirectives::new("tester", "be brief"),
        input,
        session,
    );
    request.deadline = Some(Duration::from_secs(30));
    request
}

fn sink() -> (TurnEventSink, mpsc::Receiver<TurnEvent>) {
    let (sender, receiver) = mpsc::channel(64);
    (TurnEventSink::new(sender), receiver)
}

fn drain(mut receiver: mpsc::Receiver<TurnEvent>) -> Vec<TurnEvent> {
    let mut collected = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        collected.push(event);
    }
    collected
}

#[tokio::test]
async fn echo_turn_captures_result_session_usage_and_events() {
    let provider = provider_with(&[]);
    let (events, receiver) = sink();
    let outcome = provider
        .execute_turn(request("hello there", SessionDirective::Start), events)
        .await
        .expect("echo turn succeeds");

    assert!(
        outcome.text.starts_with("echo: hello there"),
        "{}",
        outcome.text
    );
    assert!(outcome.text.contains("system=be brief"));
    assert_eq!(outcome.session_id, "sess-fake");
    assert_eq!(outcome.usage.total_cost_usd, Some(0.02));
    assert_eq!(outcome.usage.input_tokens, Some(7));
    assert_eq!(outcome.usage.output_tokens, Some(3));
    assert_eq!(outcome.usage.duration, Some(Duration::from_millis(40)));
    assert_eq!(provider.detected_version(), Some(PINNED_VERSION));

    let events = drain(receiver);
    assert!(
        events.iter().any(|event| matches!(
            event,
            TurnEvent::SessionStarted { session_id } if session_id == "sess-fake"
        )),
        "init must surface as SessionStarted: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, TurnEvent::Liveness))
    );
}

#[tokio::test]
async fn version_drift_is_tolerated_and_recorded() {
    let provider = provider_with(&[("FAKE_CLAUDE_VERSION", "9.9.9 (Claude Code)")]);
    // Drift warns (tracing) but does not fail: detection records the
    // drifted version and turns proceed against the tolerant protocol.
    let version = provider.ensure_version().await.expect("version detect");
    assert_eq!(version, "9.9.9");
    let (events, _receiver) = sink();
    provider
        .execute_turn(request("still works", SessionDirective::Start), events)
        .await
        .expect("drifted harness still executes turns");
}

#[tokio::test]
async fn session_directives_render_resume_and_fork() {
    let provider = provider_with(&[]);
    let (events, _receiver) = sink();
    let outcome = provider
        .execute_turn(
            request(
                "continue",
                SessionDirective::ResumeForked {
                    session_id: "sess-parent".to_owned(),
                },
            ),
            events,
        )
        .await
        .expect("resume turn succeeds");
    assert!(
        outcome.text.contains("resumed=sess-parent"),
        "{}",
        outcome.text
    );
    assert!(outcome.text.contains("forked"), "{}", outcome.text);

    let (events, _receiver) = sink();
    let outcome = provider
        .execute_turn(
            request(
                "continue",
                SessionDirective::Resume {
                    session_id: "sess-parent".to_owned(),
                },
            ),
            events,
        )
        .await
        .expect("plain resume succeeds");
    assert!(outcome.text.contains("resumed=sess-parent"));
    assert!(!outcome.text.contains("forked"));
}

#[tokio::test]
async fn model_and_schema_directives_reach_the_harness() {
    let provider = provider_with(&[]);
    let (events, _receiver) = sink();
    let mut req = request("typed", SessionDirective::Start);
    req.directives.model = Some("opus".to_owned());
    req.directives.output_schema = Some(json!({"type": "object"}));
    let outcome = provider
        .execute_turn(req, events)
        .await
        .expect("turn succeeds");
    assert!(outcome.text.contains("model=opus"));
    assert!(outcome.text.contains("schema"));
}

#[tokio::test]
async fn classification_maps_the_taxonomy() {
    type Matcher = fn(&TurnError) -> bool;
    let cases: &[(&str, Matcher, bool)] = &[
        ("api_error", |e| matches!(e, TurnError::Api { .. }), true),
        ("auth", |e| matches!(e, TurnError::Config { .. }), false),
        (
            "usage_cap",
            |e| matches!(e, TurnError::Config { .. }),
            false,
        ),
        (
            "resume_missing",
            |e| matches!(e, TurnError::SessionNotFound { .. }),
            false,
        ),
        ("die", |e| matches!(e, TurnError::HarnessDied { .. }), true),
    ];
    for (mode, matcher, retryable) in cases {
        let provider = provider_with(&[("FAKE_CLAUDE_MODE", mode)]);
        let (events, _receiver) = sink();
        let error = provider
            .execute_turn(request("fail please", SessionDirective::Start), events)
            .await
            .expect_err("mode must fail");
        assert!(matcher(&error), "mode {mode}: unexpected class {error:?}");
        assert_eq!(
            error.is_retryable(),
            *retryable,
            "mode {mode}: retryability"
        );
    }
}

#[tokio::test]
async fn death_with_pending_bridge_calls_is_its_own_class() {
    let provider = provider_with(&[
        ("FAKE_CLAUDE_MODE", "die"),
        ("FAKE_CLAUDE_PENDING_TOOL", "mcp__odori__deploy"),
    ]);
    let (events, _receiver) = sink();
    let error = provider
        .execute_turn(request("die awaiting", SessionDirective::Start), events)
        .await
        .expect_err("death must fail the turn");
    let TurnError::HarnessDiedAwaitingTools { pending_calls, .. } = &error else {
        panic!("expected HarnessDiedAwaitingTools, got {error:?}");
    };
    assert_eq!(pending_calls, &vec!["tu-pending-1".to_owned()]);
    assert!(error.is_retryable());
    assert_eq!(
        error.error_type(),
        "odori::turn::harness_died_awaiting_tools"
    );
}

#[tokio::test]
async fn auth_failure_text_carries_reauth_guidance() {
    let provider = provider_with(&[("FAKE_CLAUDE_MODE", "auth")]);
    let (events, _receiver) = sink();
    let error = provider
        .execute_turn(request("auth", SessionDirective::Start), events)
        .await
        .expect_err("auth mode fails");
    let TurnError::Config { message } = &error else {
        panic!("auth must be terminal Config: {error:?}");
    };
    assert!(message.contains("claude login"), "{message}");
}

#[tokio::test]
async fn missing_binary_says_how_to_install() {
    let provider =
        ClaudeProvider::with_config(ClaudeConfig::default().with_binary("/definitely/not/claude"));
    let (events, _receiver) = sink();
    let error = provider
        .execute_turn(request("hi", SessionDirective::Start), events)
        .await
        .expect_err("missing binary must fail");
    let TurnError::Config { message } = &error else {
        panic!("missing binary must be terminal Config: {error:?}");
    };
    assert!(
        message.contains("npm install -g @anthropic-ai/claude-code")
            && message.contains("claude login"),
        "operator-empathy text missing: {message}"
    );
    assert!(!error.is_retryable());
}

mod bridged {
    //! A real `tools/call` from the scripted harness through the real
    //! bridge server — the provider-side half of the live E2E, quota-free.

    use super::*;
    use async_trait::async_trait;
    use odori_agents::{
        Agent, AgentRegistry, Tool, ToolCallResult,
        provider::AttachmentSource,
        run::{ToolInvocation, ToolInvocationReply},
    };
    use odori_mcp_bridge::{Bridge, BridgeConfig, BridgeError, UpdateClient};

    #[derive(Debug)]
    struct RecordingUpdateClient;

    #[async_trait]
    impl UpdateClient for RecordingUpdateClient {
        async fn tool_invoked(
            &self,
            _workflow_id: &str,
            invocation: ToolInvocation,
        ) -> Result<ToolInvocationReply, BridgeError> {
            Ok(ToolInvocationReply::Completed(ToolCallResult::text(
                format!(
                    "bridged:{}:{}",
                    invocation.tool, invocation.identity.call_id
                ),
            )))
        }
    }

    #[tokio::test]
    async fn scripted_harness_completes_a_bridged_tool_call() {
        let mut registry = AgentRegistry::new();
        registry.register(Agent::new("ops", "operate").with_tool(Tool::new(
            "deploy",
            "Deploy",
            json!({"type": "object"}),
            |_context, _args| async { Ok(json!("unused: the update client answers")) },
        )));
        let bridge = Bridge::start(
            Arc::new(registry),
            Arc::new(RecordingUpdateClient),
            BridgeConfig::default(),
        )
        .await
        .expect("bridge start");
        let attachment = bridge
            .attachment_for(
                "wf-o3",
                &TurnIdentity {
                    run_id: "r".into(),
                    turn: 0,
                    attempt: 1,
                },
                "ops",
            )
            .expect("attachment");

        let provider = provider_with(&[("FAKE_CLAUDE_MODE", "mcp")]);
        let (events, receiver) = sink();
        let mut req = request("use the tool", SessionDirective::Start);
        req.tooling.mcp_servers.push(attachment.mcp_server);
        req.tooling.mcp_timeout = attachment.mcp_timeout;
        req.tooling.allowed_native_tools = Some(attachment.allowed_tools);

        let outcome = provider
            .execute_turn(req, events)
            .await
            .expect("bridged turn");
        assert!(
            outcome.text.contains("bridged:deploy:tu-fake-1"),
            "the bridge's reply must round-trip into the turn result: {}",
            outcome.text
        );
        let events = drain(receiver);
        assert!(
            events.iter().any(|event| matches!(
                event,
                TurnEvent::ToolUse { name } if name == "mcp__odori__deploy"
            )),
            "tool use must surface as an event: {events:?}"
        );
    }
}
