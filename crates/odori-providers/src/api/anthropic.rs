//! The Anthropic Messages API provider (`api-anthropic`): the secondary
//! tier for operators without a subscription seat.
//!
//! One turn = the internal completion-and-tool loop run to quiescence
//! (see the `api` module docs for the tier's shape and v0 posture).
//! Streaming SSE events become heartbeats; `tool_use` stop reasons
//! dispatch through the bridge; typed outputs use the Messages API's
//! native `output_config.format` JSON-Schema mechanism. Conversations are
//! continuity-within-process: sessions live in the provider's memory,
//! keyed by generated ids.

use async_trait::async_trait;
use odori_agents::provider::{
    Provider, SessionDirective, TurnError, TurnEvent, TurnEventSink, TurnOutcome, TurnRequest,
    TurnUsage,
};
use serde_json::{Value, json};

use super::{
    BridgeTools, MAX_HTTP_ATTEMPTS, SessionStore, api_key, backoff_delay, is_transient, read_sse,
    retry_after_hint, wait_out,
};

/// Configuration for [`AnthropicProvider`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct AnthropicConfig {
    /// Model id (default: `claude-opus-5`, the current general default per
    /// the Claude API reference; `claude-fable-5` is available for the
    /// most demanding work at a higher price and different API behaviour).
    pub model: String,
    /// `max_tokens` per model request.
    pub max_tokens: u32,
    /// API origin, overridable for tests and gateways.
    pub base_url: String,
    /// Ceiling on completion-and-tool iterations within one turn.
    pub max_loop_iterations: u32,
}

impl Default for AnthropicConfig {
    fn default() -> Self {
        Self {
            model: "claude-opus-5".to_owned(),
            max_tokens: 16_000,
            base_url: "https://api.anthropic.com".to_owned(),
            max_loop_iterations: 16,
        }
    }
}

impl AnthropicConfig {
    /// Select the model.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// Override the API origin (tests, gateways).
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }
}

/// The provider.
#[derive(Debug, Default)]
pub struct AnthropicProvider {
    config: AnthropicConfig,
    sessions: SessionStore,
    http: reqwest::Client,
}

impl AnthropicProvider {
    /// A provider with default configuration.
    pub fn new() -> Self {
        Self::default()
    }

    /// A provider with explicit configuration.
    pub fn with_config(config: AnthropicConfig) -> Self {
        Self {
            config,
            ..Self::default()
        }
    }
}

/// What one streamed Messages exchange produced.
#[derive(Debug, Default)]
struct Exchange {
    content: Vec<Value>,
    stop_reason: Option<String>,
    input_tokens: u64,
    output_tokens: u64,
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn name(&self) -> &str {
        "anthropic-api"
    }

    async fn execute_turn(
        &self,
        request: TurnRequest,
        events: TurnEventSink,
    ) -> Result<TurnOutcome, TurnError> {
        let key = api_key("ANTHROPIC_API_KEY", "Anthropic Messages API")?;
        let bridge = BridgeTools::resolve(&request.tooling)?;
        let tools = match &bridge {
            Some(bridge) => bridge.list().await?,
            None => Vec::new(),
        };
        let tool_defs: Vec<Value> = tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "input_schema": tool.schema,
                })
            })
            .collect();

        // Session semantics (v0: in-process continuity; module docs).
        let (session_id, mut messages) = match &request.session {
            SessionDirective::Start => (format!("anthropic-{}", uuid::Uuid::new_v4()), Vec::new()),
            SessionDirective::Resume { session_id } => {
                (session_id.clone(), self.sessions.resume(session_id)?)
            }
            SessionDirective::ResumeForked { session_id } => (
                format!("anthropic-{}", uuid::Uuid::new_v4()),
                self.sessions.resume(session_id)?,
            ),
        };
        messages.push(json!({"role": "user", "content": request.input}));

        let mut total_input = 0_u64;
        let mut total_output = 0_u64;
        let started = std::time::Instant::now();
        let final_text = 'turn: {
            for _iteration in 0..self.config.max_loop_iterations {
                let mut body = json!({
                    "model": request.directives.model.as_deref().unwrap_or(&self.config.model),
                    "max_tokens": self.config.max_tokens,
                    "stream": true,
                    "messages": messages,
                });
                if !request.directives.instructions.is_empty() {
                    body["system"] = json!(request.directives.instructions);
                }
                if !tool_defs.is_empty() {
                    body["tools"] = json!(tool_defs);
                }
                if let Some(schema) = &request.directives.output_schema {
                    body["output_config"] =
                        json!({"format": {"type": "json_schema", "schema": schema}});
                }

                let exchange = self.stream_once(&key, &body, &events).await?;
                total_input += exchange.input_tokens;
                total_output += exchange.output_tokens;
                let mut usage = TurnUsage::default();
                usage.input_tokens = Some(total_input);
                usage.output_tokens = Some(total_output);
                usage.duration = Some(started.elapsed());
                events.report_usage(usage);
                messages.push(json!({"role": "assistant", "content": exchange.content}));

                let tool_calls: Vec<(String, String, Value)> = exchange
                    .content
                    .iter()
                    .filter(|block| block["type"] == "tool_use")
                    .map(|block| {
                        (
                            block["id"].as_str().unwrap_or_default().to_owned(),
                            block["name"].as_str().unwrap_or_default().to_owned(),
                            block["input"].clone(),
                        )
                    })
                    .collect();

                if exchange.stop_reason.as_deref() == Some("tool_use") && !tool_calls.is_empty() {
                    let Some(bridge) = &bridge else {
                        break 'turn Err(TurnError::Tooling {
                            message: "the model requested a tool but no bridge is attached"
                                .to_owned(),
                        });
                    };
                    let mut results = Vec::new();
                    for (call_id, name, input) in tool_calls {
                        events.emit(TurnEvent::ToolUse { name: name.clone() });
                        let result = bridge.call(&name, &call_id, &input).await?;
                        results.push(json!({
                            "type": "tool_result",
                            "tool_use_id": call_id,
                            "content": result.text,
                            "is_error": result.is_error,
                        }));
                    }
                    messages.push(json!({"role": "user", "content": results}));
                    continue;
                }

                // Quiescence: concatenate the final message's text blocks.
                let text = exchange
                    .content
                    .iter()
                    .filter(|block| block["type"] == "text")
                    .filter_map(|block| block["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("");
                break 'turn Ok(text);
            }
            Err(TurnError::Api {
                message: format!(
                    "the completion-and-tool loop exceeded {} iterations without quiescing",
                    self.config.max_loop_iterations
                ),
            })
        }?;

        self.sessions.save(&session_id, messages);
        let mut outcome = TurnOutcome::new(session_id, final_text);
        outcome.usage.input_tokens = Some(total_input);
        outcome.usage.output_tokens = Some(total_output);
        outcome.usage.duration = Some(started.elapsed());
        Ok(outcome)
    }
}

impl AnthropicProvider {
    /// One streamed Messages request, with transient-class retries that
    /// honor `retry-after`.
    async fn stream_once(
        &self,
        key: &str,
        body: &Value,
        events: &TurnEventSink,
    ) -> Result<Exchange, TurnError> {
        let mut attempt = 0;
        let response = loop {
            events.emit(TurnEvent::Liveness);
            let sent = self
                .http
                .post(format!("{}/v1/messages", self.config.base_url))
                .header("x-api-key", key)
                .header("anthropic-version", "2023-06-01")
                .json(body)
                .send()
                .await;
            match sent {
                Ok(response) if response.status().is_success() => break response,
                Ok(response) => {
                    let status = response.status();
                    let hint = retry_after_hint(response.headers());
                    let detail = response.text().await.unwrap_or_default();
                    if is_transient(status) && attempt + 1 < MAX_HTTP_ATTEMPTS {
                        wait_out(events, backoff_delay(attempt, hint)).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(classify_status(status, &detail));
                }
                Err(error) => {
                    if attempt + 1 < MAX_HTTP_ATTEMPTS {
                        wait_out(events, backoff_delay(attempt, None)).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(TurnError::Api {
                        message: format!("request failed after retries: {error}"),
                    });
                }
            }
        };

        let mut exchange = Exchange::default();
        // Streaming assembly: text deltas append to text blocks; tool_use
        // blocks accumulate partial JSON until their stop.
        let mut partial_json: Vec<(usize, String)> = Vec::new();
        read_sse(response, |frame| {
            let Ok(data) = serde_json::from_str::<Value>(&frame.data) else {
                return;
            };
            match data["type"].as_str().unwrap_or("") {
                "message_start" => {
                    exchange.input_tokens += data
                        .pointer("/message/usage/input_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                }
                "content_block_start" => {
                    let index = data["index"].as_u64().unwrap_or(0) as usize;
                    let block = data["content_block"].clone();
                    while exchange.content.len() <= index {
                        exchange.content.push(json!(null));
                    }
                    if block["type"] == "tool_use" {
                        partial_json.push((index, String::new()));
                    }
                    exchange.content[index] = block;
                }
                "content_block_delta" => {
                    let index = data["index"].as_u64().unwrap_or(0) as usize;
                    match data.pointer("/delta/type").and_then(Value::as_str) {
                        Some("text_delta") => {
                            if let Some(text) = data.pointer("/delta/text").and_then(Value::as_str)
                                && let Some(block) = exchange.content.get_mut(index)
                                && let Some(existing) = block["text"].as_str()
                            {
                                block["text"] = json!(format!("{existing}{text}"));
                            }
                        }
                        Some("input_json_delta") => {
                            if let Some(fragment) =
                                data.pointer("/delta/partial_json").and_then(Value::as_str)
                                && let Some((_, accumulated)) = partial_json
                                    .iter_mut()
                                    .find(|(block_index, _)| *block_index == index)
                            {
                                accumulated.push_str(fragment);
                            }
                        }
                        _ => {}
                    }
                }
                "content_block_stop" => {
                    let index = data["index"].as_u64().unwrap_or(0) as usize;
                    if let Some(position) = partial_json
                        .iter()
                        .position(|(block_index, _)| *block_index == index)
                    {
                        let (_, accumulated) = partial_json.remove(position);
                        if let Some(block) = exchange.content.get_mut(index) {
                            block["input"] =
                                serde_json::from_str(&accumulated).unwrap_or_else(|_| json!({}));
                        }
                    }
                }
                "message_delta" => {
                    if let Some(reason) = data.pointer("/delta/stop_reason").and_then(Value::as_str)
                    {
                        exchange.stop_reason = Some(reason.to_owned());
                    }
                    exchange.output_tokens += data
                        .pointer("/usage/output_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                }
                _ => {}
            }
            events.emit(TurnEvent::Liveness);
        })
        .await?;
        exchange.content.retain(|block| !block.is_null());
        Ok(exchange)
    }
}

/// Map a terminal HTTP status onto the taxonomy.
fn classify_status(status: reqwest::StatusCode, detail: &str) -> TurnError {
    let head: String = detail.chars().take(300).collect();
    match status.as_u16() {
        401 | 403 => TurnError::Config {
            message: format!(
                "the Anthropic API rejected the credentials ({status}): check that \
                 ANTHROPIC_API_KEY is set to a valid key for this workspace. {head}"
            ),
        },
        400 | 404 | 422 => TurnError::Config {
            message: format!("the Anthropic API rejected the request ({status}): {head}"),
        },
        _ => TurnError::Api {
            message: format!("Anthropic API failure ({status}) after retries: {head}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn backoff_honors_retry_after_and_caps() {
        assert_eq!(
            backoff_delay(0, Some(Duration::from_secs(7))),
            Duration::from_secs(7)
        );
        assert_eq!(
            backoff_delay(0, Some(Duration::from_secs(600))),
            Duration::from_secs(30),
            "hints are capped"
        );
        assert!(backoff_delay(3, None) >= Duration::from_secs(1));
    }
}
