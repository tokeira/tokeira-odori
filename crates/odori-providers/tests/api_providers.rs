//! Unguarded mock-server tests for the API-backed provider tier: request
//! shapes, retry/backoff with server hints, streaming assembly,
//! typed-output plumbing, session semantics, and the bridge-as-tool-path
//! loop against the real mcp-bridge. Live smokes live in the embedded
//! harness, keyed on real keys.
#![cfg(any(feature = "api-anthropic", feature = "api-openai"))]
// Test-harness ergonomics: the recorded-request tuple stays literal, and
// helper items are shared across cfg-gated modules.
#![allow(clippy::type_complexity, unreachable_pub, dead_code)]

use std::sync::{Arc, Mutex};

use odori_agents::provider::{
    AgentDirectives, SessionDirective, TurnEvent, TurnEventSink, TurnIdentity, TurnRequest,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// nextest runs each test in its own process; these run on the main test
/// thread before any other thread exists, which is what makes the env
/// mutation sound.
#[allow(unsafe_code)]
fn set_test_key(variable: &str) {
    // SAFETY: single-threaded at call time (see above); the pointer/alias
    // hazards of set_var require concurrent readers, which do not exist yet.
    unsafe { std::env::set_var(variable, "odori-test-key") }
}

#[allow(unsafe_code)]
fn clear_key(variable: &str) {
    // SAFETY: as `set_test_key`.
    unsafe { std::env::remove_var(variable) }
}

/// One scripted mock response.
#[derive(Debug, Clone)]
enum Scripted {
    Json {
        status: u16,
        headers: Vec<(String, String)>,
        body: Value,
    },
    Sse(Vec<(String, Value)>),
}

/// A minimal scripted HTTP server: pops one response per request, records
/// every request body and selected headers.
#[derive(Debug)]
struct MockApi {
    address: std::net::SocketAddr,
    requests: Arc<Mutex<Vec<(String, Value, Vec<(String, String)>)>>>,
}

impl MockApi {
    async fn start(script: Vec<Scripted>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock");
        let address = listener.local_addr().expect("mock addr");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let script = Arc::new(Mutex::new(script));
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let recorded = recorded.clone();
                let script = script.clone();
                tokio::spawn(async move {
                    loop {
                        let Some((path, body, headers)) = read_request(&mut stream).await else {
                            return;
                        };
                        recorded.lock().expect("lock").push((path, body, headers));
                        let next = {
                            let mut script = script.lock().expect("lock");
                            if script.is_empty() {
                                Scripted::Json {
                                    status: 500,
                                    headers: Vec::new(),
                                    body: json!({"error": "mock script exhausted"}),
                                }
                            } else {
                                script.remove(0)
                            }
                        };
                        write_response(&mut stream, next).await;
                    }
                });
            }
        });
        Self { address, requests }
    }

    fn url(&self) -> String {
        format!("http://{}", self.address)
    }

    fn requests(&self) -> Vec<(String, Value, Vec<(String, String)>)> {
        self.requests.lock().expect("lock").clone()
    }
}

async fn read_request(
    stream: &mut tokio::net::TcpStream,
) -> Option<(String, Value, Vec<(String, String)>)> {
    let mut raw = Vec::new();
    let mut buf = [0_u8; 4096];
    let header_end = loop {
        if let Some(position) = raw.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
        let read = stream.read(&mut buf).await.ok()?;
        if read == 0 {
            return None;
        }
        raw.extend_from_slice(&buf[..read]);
    };
    let head = String::from_utf8_lossy(&raw[..header_end]).to_string();
    let path = head.split_whitespace().nth(1).unwrap_or("/").to_owned();
    let headers: Vec<(String, String)> = head
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_lowercase(), value.trim().to_owned()))
        .collect();
    let content_length: usize = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0);
    let mut body = raw[header_end..].to_vec();
    while body.len() < content_length {
        let read = stream.read(&mut buf).await.ok()?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&buf[..read]);
    }
    let body = serde_json::from_slice(&body).unwrap_or(Value::Null);
    Some((path, body, headers))
}

async fn write_response(stream: &mut tokio::net::TcpStream, response: Scripted) {
    let payload = match response {
        Scripted::Json {
            status,
            headers,
            body,
        } => {
            let body = body.to_string();
            let extra: String = headers
                .iter()
                .map(|(name, value)| format!("{name}: {value}\r\n"))
                .collect();
            format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\n{extra}content-length: {}\r\n\r\n{body}",
                body.len()
            )
        }
        Scripted::Sse(frames) => {
            let mut body = String::new();
            for (event, data) in frames {
                body.push_str(&format!("event: {event}\ndata: {data}\n\n"));
            }
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            )
        }
    };
    let _ = stream.write_all(payload.as_bytes()).await;
}

fn request(input: &str, session: SessionDirective) -> TurnRequest {
    TurnRequest::new(
        TurnIdentity {
            run_id: "run-api".to_owned(),
            turn: 0,
            attempt: 1,
        },
        AgentDirectives::new("api-agent", "answer plainly"),
        input,
        session,
    )
}

fn sink() -> (TurnEventSink, tokio::sync::mpsc::Receiver<TurnEvent>) {
    let (sender, receiver) = tokio::sync::mpsc::channel(256);
    (TurnEventSink::new(sender), receiver)
}

/// A real bridge with a recording update client, for the tool-path tests.
#[cfg(any(feature = "api-anthropic", feature = "api-openai"))]
mod bridge_support {
    use super::*;
    use async_trait::async_trait;
    use odori_agents::{
        Agent, AgentRegistry, Tool, ToolCallResult,
        provider::AttachmentSource,
        run::{ToolInvocation, ToolInvocationReply},
    };
    use odori_mcp_bridge::{Bridge, BridgeConfig, BridgeError, UpdateClient};

    #[derive(Debug, Default)]
    pub struct Recording {
        pub calls: Mutex<Vec<(String, String)>>,
    }

    #[derive(Debug)]
    pub struct RecordingUpdateClient(pub Arc<Recording>);

    #[async_trait]
    impl UpdateClient for RecordingUpdateClient {
        async fn tool_invoked(
            &self,
            _workflow_id: &str,
            invocation: ToolInvocation,
        ) -> Result<ToolInvocationReply, BridgeError> {
            self.0
                .calls
                .lock()
                .expect("lock")
                .push((invocation.tool.clone(), invocation.identity.call_id.clone()));
            Ok(ToolInvocationReply::Completed(ToolCallResult::text(
                format!(
                    "bridged:{}:{}",
                    invocation.tool, invocation.identity.call_id
                ),
            )))
        }
    }

    pub async fn bridge_attachment() -> (Arc<Recording>, odori_agents::provider::TurnAttachment) {
        let mut registry = AgentRegistry::new();
        registry.register(
            Agent::new("api-agent", "answer plainly").with_tool(Tool::new(
                "deploy",
                "Deploy the target",
                json!({"type": "object", "properties": {"target": {"type": "string"}}}),
                |_context, _args| async { Ok(json!("unused")) },
            )),
        );
        let recording = Arc::new(Recording::default());
        let bridge = Bridge::start(
            Arc::new(registry),
            Arc::new(RecordingUpdateClient(recording.clone())),
            BridgeConfig::default(),
        )
        .await
        .expect("bridge start");
        let attachment = bridge
            .attachment_for(
                "wf-api",
                &TurnIdentity {
                    run_id: "r".into(),
                    turn: 0,
                    attempt: 1,
                },
                "api-agent",
            )
            .expect("attachment");
        // Keep the bridge alive for the test's duration.
        std::mem::forget(bridge);
        (recording, attachment)
    }
}

#[cfg(feature = "api-anthropic")]
mod anthropic {
    use super::*;
    use odori_agents::provider::{Effort, Provider, TurnError};
    use odori_providers::{AnthropicConfig, AnthropicProvider};

    fn provider(base_url: &str) -> AnthropicProvider {
        set_test_key("ANTHROPIC_API_KEY");
        AnthropicProvider::with_config(AnthropicConfig::default().with_base_url(base_url))
    }

    fn text_exchange(text: &str, stop: &str) -> Scripted {
        Scripted::Sse(vec![
            (
                "message_start".into(),
                json!({"type": "message_start",
                       "message": {"usage": {"input_tokens": 11}}}),
            ),
            (
                "content_block_start".into(),
                json!({"type": "content_block_start", "index": 0,
                       "content_block": {"type": "text", "text": ""}}),
            ),
            (
                "content_block_delta".into(),
                json!({"type": "content_block_delta", "index": 0,
                       "delta": {"type": "text_delta", "text": text}}),
            ),
            (
                "content_block_stop".into(),
                json!({"type": "content_block_stop", "index": 0}),
            ),
            (
                "message_delta".into(),
                json!({"type": "message_delta", "delta": {"stop_reason": stop},
                       "usage": {"output_tokens": 5}}),
            ),
            ("message_stop".into(), json!({"type": "message_stop"})),
        ])
    }

    #[tokio::test]
    async fn request_shape_streaming_assembly_and_usage() {
        let mock = MockApi::start(vec![text_exchange("Hello from the mock", "end_turn")]).await;
        let provider = provider(&mock.url());
        let (events, mut receiver) = sink();
        let mut req = request("say hello", SessionDirective::Start);
        req.directives.output_schema = Some(json!({"type": "object"}));
        req.directives.effort = Some(Effort::High);
        let outcome = provider.execute_turn(req, events).await.expect("turn");

        assert_eq!(outcome.text, "Hello from the mock");
        assert_eq!(outcome.usage.input_tokens, Some(11));
        assert_eq!(outcome.usage.output_tokens, Some(5));
        assert!(outcome.session_id.starts_with("anthropic-"));

        let requests = mock.requests();
        assert_eq!(requests.len(), 1);
        let (path, body, headers) = &requests[0];
        assert_eq!(path, "/v1/messages");
        assert_eq!(body["model"], "claude-opus-5");
        assert_eq!(body["system"], "answer plainly");
        assert_eq!(
            body["thinking"],
            json!({"type": "enabled", "budget_tokens": 8192}),
            "effort high maps to the documented thinking budget"
        );
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "say hello");
        assert_eq!(body["stream"], true);
        assert_eq!(body["output_config"]["format"]["type"], "json_schema");
        assert!(body.get("tools").is_none(), "toolless turn sends no tools");
        assert!(
            headers
                .iter()
                .any(|(name, value)| name == "x-api-key" && value == "odori-test-key")
        );
        assert!(
            headers
                .iter()
                .any(|(name, value)| name == "anthropic-version" && value == "2023-06-01")
        );
        assert!(receiver.try_recv().is_ok(), "streaming emitted heartbeats");
    }

    #[tokio::test]
    async fn provider_default_budget_applies_and_agent_effort_overrides_it() {
        let mock = MockApi::start(vec![
            text_exchange("with default budget", "end_turn"),
            text_exchange("with explicit none", "end_turn"),
        ])
        .await;
        set_test_key("ANTHROPIC_API_KEY");
        let provider = AnthropicProvider::with_config(
            AnthropicConfig::default()
                .with_base_url(mock.url())
                .with_thinking_budget(3000),
        );

        let (events, _r) = sink();
        provider
            .execute_turn(request("no effort set", SessionDirective::Start), events)
            .await
            .expect("default-budget turn");

        let (events, _r) = sink();
        let mut req = request("explicit none", SessionDirective::Start);
        req.directives.effort = Some(Effort::None);
        provider.execute_turn(req, events).await.expect("none turn");

        let requests = mock.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[0].1["thinking"],
            json!({"type": "enabled", "budget_tokens": 3000}),
            "the provider-level raw budget applies when the agent sets no effort"
        );
        assert!(
            requests[1].1.get("thinking").is_none(),
            "agent-level Effort::None overrides the provider default and disables thinking"
        );
    }

    #[tokio::test]
    async fn retry_honors_retry_after_then_succeeds() {
        let mock = MockApi::start(vec![
            Scripted::Json {
                status: 429,
                headers: vec![("retry-after".into(), "1".into())],
                body: json!({"error": {"type": "rate_limit_error"}}),
            },
            Scripted::Json {
                status: 529,
                headers: Vec::new(),
                body: json!({"error": {"type": "overloaded_error"}}),
            },
            text_exchange("recovered", "end_turn"),
        ])
        .await;
        let provider = provider(&mock.url());
        let (events, _receiver) = sink();
        let started = std::time::Instant::now();
        let outcome = provider
            .execute_turn(request("retry me", SessionDirective::Start), events)
            .await
            .expect("recovers after transient failures");
        assert_eq!(outcome.text, "recovered");
        assert_eq!(mock.requests().len(), 3, "429 then 529 then success");
        assert!(
            started.elapsed() >= std::time::Duration::from_secs(1),
            "the retry-after hint was honored"
        );
    }

    #[tokio::test]
    async fn session_resume_and_fork_reuse_history() {
        let mock = MockApi::start(vec![
            text_exchange("first answer", "end_turn"),
            text_exchange("second answer", "end_turn"),
        ])
        .await;
        let provider = provider(&mock.url());
        let (events, _r) = sink();
        let outcome = provider
            .execute_turn(request("first", SessionDirective::Start), events)
            .await
            .expect("first turn");

        let (events, _r) = sink();
        let forked = provider
            .execute_turn(
                request(
                    "second",
                    SessionDirective::ResumeForked {
                        session_id: outcome.session_id.clone(),
                    },
                ),
                events,
            )
            .await
            .expect("forked turn");
        assert_ne!(forked.session_id, outcome.session_id, "forks mint new ids");

        let requests = mock.requests();
        let history = &requests[1].1["messages"];
        assert_eq!(history[0]["content"], "first", "history was replayed");
        assert_eq!(history[1]["role"], "assistant");
        assert_eq!(history[2]["content"], "second");

        // Unknown session: the taxonomy's SessionNotFound.
        let (events, _r) = sink();
        let error = provider
            .execute_turn(
                request(
                    "lost",
                    SessionDirective::Resume {
                        session_id: "anthropic-missing".into(),
                    },
                ),
                events,
            )
            .await
            .expect_err("unknown session must fail");
        assert!(matches!(error, TurnError::SessionNotFound { .. }));
    }

    #[tokio::test]
    async fn tool_loop_executes_through_the_real_bridge() {
        let (recording, attachment) = bridge_support::bridge_attachment().await;
        let mock = MockApi::start(vec![
            Scripted::Sse(vec![
                (
                    "message_start".into(),
                    json!({"type": "message_start", "message": {"usage": {"input_tokens": 9}}}),
                ),
                (
                    "content_block_start".into(),
                    json!({"type": "content_block_start", "index": 0,
                           "content_block": {"type": "tool_use", "id": "toolu_api1",
                                             "name": "deploy", "input": {}}}),
                ),
                (
                    "content_block_delta".into(),
                    json!({"type": "content_block_delta", "index": 0,
                           "delta": {"type": "input_json_delta",
                                     "partial_json": "{\"target\":\"prod\"}"}}),
                ),
                (
                    "content_block_stop".into(),
                    json!({"type": "content_block_stop", "index": 0}),
                ),
                (
                    "message_delta".into(),
                    json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"},
                           "usage": {"output_tokens": 3}}),
                ),
            ]),
            text_exchange("deployment reported", "end_turn"),
        ])
        .await;
        let provider = provider(&mock.url());
        let (events, mut receiver) = sink();
        let mut req = request("deploy prod please", SessionDirective::Start);
        req.tooling.mcp_servers.push(attachment.mcp_server);
        req.tooling.framework_tools = vec!["deploy".to_owned()];

        let outcome = provider.execute_turn(req, events).await.expect("tool turn");
        assert_eq!(outcome.text, "deployment reported");

        // The bridge executed the call under the API's tool-use id.
        assert_eq!(
            recording.calls.lock().expect("lock").clone(),
            vec![("deploy".to_owned(), "toolu_api1".to_owned())]
        );
        // The second model request carried the declared tool and the result.
        let requests = mock.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].1["tools"][0]["name"], "deploy");
        let second = &requests[1].1["messages"];
        let results = second[second.as_array().expect("array").len() - 1]["content"].clone();
        assert_eq!(results[0]["type"], "tool_result");
        assert_eq!(results[0]["tool_use_id"], "toolu_api1");
        assert_eq!(results[0]["content"], "bridged:deploy:toolu_api1");
        // Tool use surfaced as an event.
        let mut saw_tool = false;
        while let Ok(event) = receiver.try_recv() {
            if matches!(&event, TurnEvent::ToolUse { name } if name == "deploy") {
                saw_tool = true;
            }
        }
        assert!(saw_tool);
    }

    #[tokio::test]
    async fn thinking_blocks_survive_assembly_for_the_tool_loop() {
        let (recording, attachment) = bridge_support::bridge_attachment().await;
        let mock = MockApi::start(vec![
            Scripted::Sse(vec![
                (
                    "message_start".into(),
                    json!({"type": "message_start", "message": {"usage": {"input_tokens": 9}}}),
                ),
                (
                    "content_block_start".into(),
                    json!({"type": "content_block_start", "index": 0,
                           "content_block": {"type": "thinking", "thinking": ""}}),
                ),
                (
                    "content_block_delta".into(),
                    json!({"type": "content_block_delta", "index": 0,
                           "delta": {"type": "thinking_delta", "thinking": "weighing the "}}),
                ),
                (
                    "content_block_delta".into(),
                    json!({"type": "content_block_delta", "index": 0,
                           "delta": {"type": "thinking_delta", "thinking": "deploy target"}}),
                ),
                (
                    "content_block_delta".into(),
                    json!({"type": "content_block_delta", "index": 0,
                           "delta": {"type": "signature_delta", "signature": "sig-abc123"}}),
                ),
                (
                    "content_block_stop".into(),
                    json!({"type": "content_block_stop", "index": 0}),
                ),
                (
                    "content_block_start".into(),
                    json!({"type": "content_block_start", "index": 1,
                           "content_block": {"type": "tool_use", "id": "toolu_think1",
                                             "name": "deploy", "input": {}}}),
                ),
                (
                    "content_block_delta".into(),
                    json!({"type": "content_block_delta", "index": 1,
                           "delta": {"type": "input_json_delta",
                                     "partial_json": "{\"target\":\"prod\"}"}}),
                ),
                (
                    "content_block_stop".into(),
                    json!({"type": "content_block_stop", "index": 1}),
                ),
                (
                    "message_delta".into(),
                    json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"},
                           "usage": {"output_tokens": 3}}),
                ),
            ]),
            text_exchange("thought and deployed", "end_turn"),
        ])
        .await;
        let provider = provider(&mock.url());
        let (events, _receiver) = sink();
        let mut req = request("deploy prod thoughtfully", SessionDirective::Start);
        req.directives.effort = Some(Effort::Medium);
        req.tooling.mcp_servers.push(attachment.mcp_server);
        req.tooling.framework_tools = vec!["deploy".to_owned()];

        let outcome = provider.execute_turn(req, events).await.expect("tool turn");
        assert_eq!(outcome.text, "thought and deployed");
        assert_eq!(
            recording.calls.lock().expect("lock").clone(),
            vec![("deploy".to_owned(), "toolu_think1".to_owned())]
        );

        // The second request's echoed assistant turn carries the thinking
        // block assembled exactly — text and signature — as the API
        // requires for a thinking-enabled tool loop.
        let requests = mock.requests();
        assert_eq!(requests.len(), 2);
        for body in [&requests[0].1, &requests[1].1] {
            assert_eq!(
                body["thinking"],
                json!({"type": "enabled", "budget_tokens": 4096})
            );
        }
        let echoed = &requests[1].1["messages"][1];
        assert_eq!(echoed["role"], "assistant");
        assert_eq!(
            echoed["content"][0],
            json!({"type": "thinking", "thinking": "weighing the deploy target",
                   "signature": "sig-abc123"})
        );
        assert_eq!(echoed["content"][1]["type"], "tool_use");
    }

    #[tokio::test]
    async fn missing_key_and_toolless_misconfigurations_speak_clearly() {
        clear_key("ANTHROPIC_API_KEY");
        let provider =
            AnthropicProvider::with_config(AnthropicConfig::default().with_base_url("http://x"));
        let (events, _r) = sink();
        let error = provider
            .execute_turn(request("hi", SessionDirective::Start), events)
            .await
            .expect_err("missing key must fail");
        let TurnError::Config { message } = &error else {
            panic!("missing key must be Config: {error:?}");
        };
        assert!(message.contains("ANTHROPIC_API_KEY"), "{message}");

        // Framework tools with no bridge: the operator ruling's explicit
        // configuration error, not a silent toolless run.
        set_test_key("ANTHROPIC_API_KEY");
        let provider =
            AnthropicProvider::with_config(AnthropicConfig::default().with_base_url("http://x"));
        let (events, _r) = sink();
        let mut req = request("hi", SessionDirective::Start);
        req.tooling.framework_tools = vec!["deploy".to_owned()];
        let error = provider
            .execute_turn(req, events)
            .await
            .expect_err("tools without a bridge must fail");
        let TurnError::Config { message } = &error else {
            panic!("must be Config: {error:?}");
        };
        assert!(message.contains("preview"), "{message}");

        // Native-tool scoping is impossible at this tier.
        let (events, _r) = sink();
        let mut req = request("hi", SessionDirective::Start);
        req.tooling.allowed_native_tools = Some(vec!["Bash".to_owned()]);
        let error = provider
            .execute_turn(req, events)
            .await
            .expect_err("native tools must fail");
        assert!(matches!(error, TurnError::Config { .. }));
    }
}

#[cfg(feature = "api-openai")]
mod openai {
    use super::*;
    use odori_agents::provider::{Effort, Provider, TurnError};
    use odori_providers::{OpenAiConfig, OpenAiProvider};

    fn provider(base_url: &str) -> OpenAiProvider {
        set_test_key("OPENAI_API_KEY");
        OpenAiProvider::with_config(OpenAiConfig::default().with_base_url(base_url))
    }

    fn completed(id: &str, text: &str, calls: Vec<Value>) -> Scripted {
        let mut output = vec![json!({"type": "message", "content": [
            {"type": "output_text", "text": text}]})];
        output.extend(calls);
        Scripted::Sse(vec![
            (
                "response.output_text.delta".into(),
                json!({"type": "response.output_text.delta", "delta": text}),
            ),
            (
                "response.completed".into(),
                json!({"type": "response.completed",
                       "response": {"id": id, "output": output,
                                    "usage": {"input_tokens": 13, "output_tokens": 4}}}),
            ),
        ])
    }

    #[tokio::test]
    async fn request_shape_chaining_and_typed_output() {
        let mock = MockApi::start(vec![completed("resp_1", "chained answer", Vec::new())]).await;
        let provider = provider(&mock.url());
        let (events, _r) = sink();
        let mut req = request(
            "continue",
            SessionDirective::Resume {
                session_id: "resp_0".to_owned(),
            },
        );
        req.directives.output_schema = Some(json!({"type": "object"}));
        req.directives.effort = Some(Effort::Minimal);
        let outcome = provider.execute_turn(req, events).await.expect("turn");

        assert_eq!(outcome.text, "chained answer");
        assert_eq!(outcome.session_id, "resp_1", "session is the response id");
        assert_eq!(outcome.usage.input_tokens, Some(13));

        let requests = mock.requests();
        let (path, body, headers) = &requests[0];
        assert_eq!(path, "/v1/responses");
        assert_eq!(body["model"], "gpt-5.6");
        assert_eq!(body["previous_response_id"], "resp_0");
        assert_eq!(
            body["reasoning"],
            json!({"effort": "minimal"}),
            "effort passes to reasoning.effort verbatim"
        );
        assert_eq!(body["instructions"], "answer plainly");
        assert_eq!(body["input"], "continue");
        assert_eq!(body["text"]["format"]["type"], "json_schema");
        assert_eq!(body["text"]["format"]["strict"], true);
        assert!(
            headers
                .iter()
                .any(|(name, value)| name == "authorization" && value == "Bearer odori-test-key")
        );
    }

    #[tokio::test]
    async fn function_call_loop_executes_through_the_real_bridge() {
        let (recording, attachment) = bridge_support::bridge_attachment().await;
        let mock = MockApi::start(vec![
            completed(
                "resp_a",
                "",
                vec![json!({"type": "function_call", "call_id": "call_api1",
                            "name": "deploy", "arguments": "{\"target\":\"prod\"}"})],
            ),
            completed("resp_b", "deployed and reported", Vec::new()),
        ])
        .await;
        let provider = provider(&mock.url());
        let (events, _r) = sink();
        let mut req = request("deploy prod", SessionDirective::Start);
        req.tooling.mcp_servers.push(attachment.mcp_server);
        req.tooling.framework_tools = vec!["deploy".to_owned()];

        let outcome = provider.execute_turn(req, events).await.expect("tool turn");
        assert_eq!(outcome.text, "deployed and reported");
        assert_eq!(outcome.session_id, "resp_b");

        assert_eq!(
            recording.calls.lock().expect("lock").clone(),
            vec![("deploy".to_owned(), "call_api1".to_owned())]
        );
        let requests = mock.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].1["tools"][0]["type"], "function");
        assert_eq!(requests[0].1["tools"][0]["name"], "deploy");
        assert_eq!(requests[1].1["previous_response_id"], "resp_a");
        assert_eq!(requests[1].1["input"][0]["type"], "function_call_output");
        assert_eq!(requests[1].1["input"][0]["call_id"], "call_api1");
        assert_eq!(
            requests[1].1["input"][0]["output"],
            "bridged:deploy:call_api1"
        );
    }

    #[tokio::test]
    async fn retry_transients_and_classify_missing_chain() {
        let mock = MockApi::start(vec![
            Scripted::Json {
                status: 429,
                headers: vec![("retry-after".into(), "1".into())],
                body: json!({"error": {"message": "rate limited"}}),
            },
            completed("resp_r", "after backoff", Vec::new()),
        ])
        .await;
        let provider = provider(&mock.url());
        let (events, _r) = sink();
        let started = std::time::Instant::now();
        let outcome = provider
            .execute_turn(request("retry", SessionDirective::Start), events)
            .await
            .expect("recovers");
        assert_eq!(outcome.text, "after backoff");
        assert_eq!(mock.requests().len(), 2);
        assert!(started.elapsed() >= std::time::Duration::from_secs(1));

        // A 404 while resuming a chain is SessionNotFound.
        let mock = MockApi::start(vec![Scripted::Json {
            status: 404,
            headers: Vec::new(),
            body: json!({"error": {"message": "response not found"}}),
        }])
        .await;
        let fresh_provider =
            OpenAiProvider::with_config(OpenAiConfig::default().with_base_url(mock.url()));
        let (events, _r) = sink();
        let error = fresh_provider
            .execute_turn(
                request(
                    "resume",
                    SessionDirective::Resume {
                        session_id: "resp_gone".to_owned(),
                    },
                ),
                events,
            )
            .await
            .expect_err("missing chain must fail");
        assert!(
            matches!(&error, TurnError::SessionNotFound { session_id } if session_id == "resp_gone"),
            "{error:?}"
        );
    }
}
