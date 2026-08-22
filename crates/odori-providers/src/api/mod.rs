//! Shared machinery for the API-backed provider tier (`api-anthropic` /
//! `api-openai`): key handling, retry/backoff with server hints, SSE
//! reading, the bridge-as-tool-path MCP client, and the in-process session
//! store.
//!
//! ## The tier's shape
//!
//! One turn = one activity: `execute_turn` runs the provider's internal
//! completion-and-tool loop to quiescence, streaming chunks as heartbeats.
//! Tools execute **through the mcp-bridge attachment**, exactly like the
//! harness tier: each model-requested call POSTs `tools/call` with the
//! API's tool-use id as the call id, flowing through the registry /
//! dedupe / `execute_tool` path. Tools present with no bridge attached is
//! a configuration error at spawn, never a silent toolless run.
//!
//! ## v0 posture (statements, not TODOs)
//!
//! - **Whole-loop retry re-issues model calls**: a retried turn re-spends
//!   tokens on the model requests; tool results replay from the bridge's
//!   registry, so tool side effects do not repeat.
//! - **Multi-turn conversations are continuity-within-process** for the
//!   Anthropic tier (no server-side conversation store); the OpenAI tier
//!   chains `previous_response_id` server-side. Durable multi-turn is the
//!   subscription tier's story.

#[cfg(feature = "api-anthropic")]
pub mod anthropic;
#[cfg(feature = "api-openai")]
pub mod openai;

use std::time::Duration;

use odori_agents::provider::{McpTransport, TurnError, TurnEvent, TurnEventSink, TurnTooling};
use serde_json::{Value, json};

/// Read a provider's API key from its environment variable, with the
/// operator-empathy error the briefing requires.
pub(crate) fn api_key(variable: &str, provider: &str) -> Result<String, TurnError> {
    match std::env::var(variable) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        _ => Err(TurnError::Config {
            message: format!(
                "the {provider} provider needs an API key: set the {variable} environment \
                 variable (never a config file) for the process running the Odori worker"
            ),
        }),
    }
}

/// Internal retry policy for one HTTP exchange: bounded attempts, honoring
/// `retry-after` when the server sends it, exponential backoff otherwise.
/// Liveness heartbeats are emitted before each wait so the turn activity
/// never looks dead while backing off.
pub(crate) const MAX_HTTP_ATTEMPTS: u32 = 4;

pub(crate) fn backoff_delay(attempt: u32, retry_after: Option<Duration>) -> Duration {
    retry_after
        .unwrap_or_else(|| Duration::from_millis(250_u64.saturating_mul(1 << attempt.min(6))))
        .min(Duration::from_secs(30))
}

pub(crate) fn retry_after_hint(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

/// Whether an HTTP status is the transient class (retry with backoff).
pub(crate) fn is_transient(status: reqwest::StatusCode) -> bool {
    status.as_u16() == 429 || status.as_u16() == 529 || status.is_server_error()
}

/// One parsed SSE frame.
#[derive(Debug, Clone)]
pub(crate) struct SseFrame {
    /// The `event:` field. The Responses tier dispatches on it; the
    /// Messages tier keys on the payload's `type` instead, so a build with
    /// only `api-anthropic` never reads it.
    #[cfg_attr(not(feature = "api-openai"), allow(dead_code))]
    pub(crate) event: String,
    pub(crate) data: String,
}

/// Collect a streaming response's SSE frames, invoking `on_frame` per
/// frame (heartbeat + assembly). Tolerant: unknown fields and comment
/// lines are skipped; a transport error mid-stream surfaces as `Err`.
pub(crate) async fn read_sse(
    response: reqwest::Response,
    mut on_frame: impl FnMut(SseFrame),
) -> Result<(), TurnError> {
    use futures::StreamExt as _;
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut event = String::new();
    let mut data = String::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| TurnError::Api {
            message: format!("stream interrupted: {error}"),
        })?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(newline) = buffer.find('\n') {
            let line = buffer[..newline].trim_end_matches('\r').to_owned();
            buffer.drain(..=newline);
            if let Some(rest) = line.strip_prefix("event:") {
                event = rest.trim().to_owned();
            } else if let Some(rest) = line.strip_prefix("data:") {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(rest.trim_start());
            } else if line.is_empty() && !data.is_empty() {
                on_frame(SseFrame {
                    event: std::mem::take(&mut event),
                    data: std::mem::take(&mut data),
                });
            }
        }
    }
    if !data.is_empty() {
        on_frame(SseFrame { event, data });
    }
    Ok(())
}

/// The bridge attachment as the API tier's tool path.
#[derive(Debug, Clone)]
pub(crate) struct BridgeTools {
    url: String,
    headers: Vec<(String, String)>,
    http: reqwest::Client,
}

/// One tool the bridge advertises.
#[derive(Debug, Clone)]
pub(crate) struct BridgeTool {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) schema: Value,
}

/// The outcome of one bridged call, model-facing.
#[derive(Debug, Clone)]
pub(crate) struct BridgeCallResult {
    pub(crate) text: String,
    pub(crate) is_error: bool,
}

impl BridgeTools {
    /// Resolve the tier's tool posture from the turn's tooling (operator
    /// ruling): native-tool scoping is impossible here; framework tools
    /// without a bridge attachment is a configuration error; a bridge
    /// attachment yields the tool path.
    pub(crate) fn resolve(tooling: &TurnTooling) -> Result<Option<Self>, TurnError> {
        if tooling
            .allowed_native_tools
            .as_ref()
            .is_some_and(|tools| !tools.is_empty())
        {
            return Err(TurnError::Config {
                message: "API-backed providers have no harness-native tools to allow; \
                          register framework Tools on the agent instead (and enable the \
                          `preview` feature so they execute through the mcp-bridge)"
                    .to_owned(),
            });
        }
        let Some(server) = tooling.mcp_servers.first() else {
            if tooling.framework_tools.is_empty() {
                return Ok(None);
            }
            return Err(TurnError::Config {
                message: format!(
                    "the agent declares framework tools ({}) but no mcp-bridge attachment \
                     reached this turn: API-backed providers execute tools only through \
                     the bridge — enable the `preview` feature (odori/preview) and \
                     configure the runtime's bridge",
                    tooling.framework_tools.join(", ")
                ),
            });
        };
        let McpTransport::Http { url, headers } = &server.transport else {
            return Err(TurnError::Config {
                message: "the API tier consumes the bridge over HTTP; a stdio MCP \
                          attachment cannot be dialed from an in-process client"
                    .to_owned(),
            });
        };
        Ok(Some(Self {
            url: url.clone(),
            headers: headers.clone(),
            http: reqwest::Client::new(),
        }))
    }

    fn request(&self, body: &Value) -> reqwest::RequestBuilder {
        let mut request = self.http.post(&self.url).json(body);
        for (name, value) in &self.headers {
            request = request.header(name, value);
        }
        request
    }

    async fn rpc(&self, body: Value) -> Result<Value, TurnError> {
        let response = self
            .request(&body)
            .send()
            .await
            .map_err(|error| TurnError::Tooling {
                message: format!("bridge unreachable: {error}"),
            })?;
        let text = response.text().await.map_err(|error| TurnError::Tooling {
            message: format!("bridge response unreadable: {error}"),
        })?;
        // Plain JSON or an SSE stream whose final frame is the response.
        let frame = text
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .next_back()
            .unwrap_or(&text);
        serde_json::from_str(frame).map_err(|error| TurnError::Tooling {
            message: format!("bridge frame unparsable: {error}"),
        })
    }

    /// The bridge's tool catalogue for this turn.
    pub(crate) async fn list(&self) -> Result<Vec<BridgeTool>, TurnError> {
        let reply = self
            .rpc(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
            .await?;
        let tools = reply
            .pointer("/result/tools")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(tools
            .into_iter()
            .map(|tool| BridgeTool {
                name: tool
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                description: tool
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                schema: tool.get("inputSchema").cloned().unwrap_or(json!({})),
            })
            .collect())
    }

    /// Execute one model-requested call durably through the bridge; the
    /// API's tool-use id is the call id the registry dedupes by.
    pub(crate) async fn call(
        &self,
        tool: &str,
        call_id: &str,
        arguments: &Value,
    ) -> Result<BridgeCallResult, TurnError> {
        let reply = self
            .rpc(json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": {
                    "name": tool,
                    "arguments": arguments,
                    "_meta": {"claudecode/toolUseId": call_id},
                },
            }))
            .await?;
        if let Some(error) = reply.get("error") {
            return Err(TurnError::Tooling {
                message: format!(
                    "bridge rejected the call to {tool:?}: {}",
                    error.get("message").and_then(Value::as_str).unwrap_or("?")
                ),
            });
        }
        let text = reply
            .pointer("/result/content/0/text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let is_error = reply
            .pointer("/result/isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Ok(BridgeCallResult { text, is_error })
    }
}

/// Emit a liveness heartbeat and sleep out a backoff wait.
pub(crate) async fn wait_out(events: &TurnEventSink, delay: Duration) {
    events.emit(TurnEvent::Liveness);
    tokio::time::sleep(delay).await;
}

/// In-process conversation store for APIs without server-side sessions
/// (module docs: the v0 continuity boundary). The Responses tier chains
/// server-side and does not use it.
#[cfg(feature = "api-anthropic")]
#[derive(Debug, Default)]
pub(crate) struct SessionStore {
    sessions: std::sync::Mutex<std::collections::HashMap<String, Vec<Value>>>,
}

#[cfg(feature = "api-anthropic")]
impl SessionStore {
    pub(crate) fn resume(&self, session_id: &str) -> Result<Vec<Value>, TurnError> {
        self.sessions
            .lock()
            .expect("session lock")
            .get(session_id)
            .cloned()
            .ok_or_else(|| TurnError::SessionNotFound {
                session_id: session_id.to_owned(),
            })
    }

    pub(crate) fn save(&self, session_id: &str, messages: Vec<Value>) {
        self.sessions
            .lock()
            .expect("session lock")
            .insert(session_id.to_owned(), messages);
    }
}
