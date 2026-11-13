//! The OpenAI Responses API provider (`api-openai`): the secondary tier's
//! second seat.
//!
//! One turn = the internal completion-and-tool loop run to quiescence (see
//! the `api` module docs). Session semantics differ from the Anthropic
//! tier: the Responses API stores responses server-side, so conversations
//! chain by `previous_response_id` — `Resume`/`ResumeForked` both continue
//! from the given response id (each response mints a new id, so forks are
//! the natural shape), and the turn's outcome session id is the last
//! response id. Continuity is therefore as durable as OpenAI's response
//! retention, not process-bound.

use async_trait::async_trait;
use odori_agents::provider::{
    Provider, ReasoningEffort, SessionDirective, TurnError, TurnEvent, TurnEventSink, TurnOutcome,
    TurnRequest, TurnUsage,
};
use serde_json::{Value, json};

use super::{
    MAX_HTTP_ATTEMPTS, McpToolClient, api_key, backoff_delay, header_limit_status, is_transient,
    read_sse, retry_after_hint, wait_out,
};

/// Configuration for [`OpenAiProvider`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct OpenAiConfig {
    /// Model id (default: `gpt-5.6`, the Responses API reference's current
    /// flagship).
    pub model: String,
    /// `max_output_tokens` per model request.
    pub max_output_tokens: u32,
    /// API origin, overridable for tests and gateways.
    pub base_url: String,
    /// Ceiling on completion-and-tool iterations within one turn.
    pub max_loop_iterations: u32,
}

impl Default for OpenAiConfig {
    fn default() -> Self {
        Self {
            model: "gpt-5.6".to_owned(),
            max_output_tokens: 16_000,
            base_url: "https://api.openai.com".to_owned(),
            max_loop_iterations: 16,
        }
    }
}

impl OpenAiConfig {
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
pub struct OpenAiProvider {
    config: OpenAiConfig,
    http: reqwest::Client,
}

impl OpenAiProvider {
    /// A provider with default configuration.
    pub fn new() -> Self {
        Self::default()
    }

    /// A provider with explicit configuration.
    pub fn with_config(config: OpenAiConfig) -> Self {
        Self {
            config,
            ..Self::default()
        }
    }
}

/// What one streamed Responses API response produced.
#[derive(Debug, Default)]
struct ModelResponse {
    response_id: String,
    text: String,
    function_calls: Vec<(String, String, Value)>,
    input_tokens: u64,
    output_tokens: u64,
    cached_input_tokens: u64,
    reasoning_output_tokens: u64,
    status_error: Option<String>,
}

#[async_trait]
impl Provider for OpenAiProvider {
    fn name(&self) -> &str {
        "openai-api"
    }

    async fn execute_turn(
        &self,
        request: TurnRequest,
        events: TurnEventSink,
    ) -> Result<TurnOutcome, TurnError> {
        let key = api_key("OPENAI_API_KEY", "OpenAI Responses API")?;
        let mcp_client = McpToolClient::resolve(&request.tool_config)?;
        let tools = match &mcp_client {
            Some(mcp_client) => mcp_client.list().await?,
            None => Vec::new(),
        };
        let tool_defs: Vec<Value> = tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.schema,
                })
            })
            .collect();

        // Server-side chaining: the previous response id is the session.
        let mut previous_response_id = match &request.session {
            SessionDirective::Start => None,
            SessionDirective::Resume { session_id }
            | SessionDirective::ResumeForked { session_id } => Some(session_id.clone()),
        };

        let mut input: Value = json!(request.input);
        // ReasoningEffort validates before the first request leaves the process.
        let reasoning_effort = request.directives.effort.map(openai_effort).transpose()?;
        let mut total_input = 0_u64;
        let mut total_output = 0_u64;
        let mut total_cached_input = 0_u64;
        let mut total_reasoning_output = 0_u64;
        let started = std::time::Instant::now();

        for _iteration in 0..self.config.max_loop_iterations {
            let mut body = json!({
                "model": request.directives.model.as_deref().unwrap_or(&self.config.model),
                "max_output_tokens": self.config.max_output_tokens,
                "stream": true,
                "input": input,
            });
            if !request.directives.instructions.is_empty() {
                body["instructions"] = json!(request.directives.instructions);
            }
            if let Some(previous) = &previous_response_id {
                body["previous_response_id"] = json!(previous);
            }
            if let Some(effort) = reasoning_effort {
                body["reasoning"] = json!({"effort": effort});
            }
            if !tool_defs.is_empty() {
                body["tools"] = json!(tool_defs);
            }
            if let Some(schema) = &request.directives.output_schema {
                body["text"] = json!({"format": {
                    "type": "json_schema",
                    "name": "odori_output",
                    "strict": true,
                    "schema": schema,
                }});
            }

            let response = self
                .stream_once(&key, &body, &events, previous_response_id.as_deref())
                .await?;
            total_input += response.input_tokens;
            total_output += response.output_tokens;
            total_cached_input += response.cached_input_tokens;
            total_reasoning_output += response.reasoning_output_tokens;
            let mut usage = TurnUsage::default();
            usage.input_tokens = Some(total_input);
            usage.output_tokens = Some(total_output);
            usage.cached_input_tokens = Some(total_cached_input);
            usage.reasoning_output_tokens = Some(total_reasoning_output);
            usage.duration = Some(started.elapsed());
            events.report_usage(usage);
            if let Some(error) = response.status_error {
                return Err(TurnError::Api {
                    message: format!("the response ended in error: {error}"),
                });
            }
            previous_response_id = Some(response.response_id.clone());

            if response.function_calls.is_empty() {
                let mut outcome =
                    TurnOutcome::new(response.response_id.clone(), response.text.clone());
                outcome.usage.input_tokens = Some(total_input);
                outcome.usage.output_tokens = Some(total_output);
                outcome.usage.cached_input_tokens = Some(total_cached_input);
                outcome.usage.reasoning_output_tokens = Some(total_reasoning_output);
                outcome.usage.duration = Some(started.elapsed());
                return Ok(outcome);
            }

            let Some(mcp_client) = &mcp_client else {
                return Err(TurnError::Tooling {
                    message: "the model requested a tool but no mcp_server is configured"
                        .to_owned(),
                });
            };
            let mut outputs = Vec::new();
            for (call_id, name, arguments) in &response.function_calls {
                events.emit(TurnEvent::ToolUse { name: name.clone() });
                let result = mcp_client.call(name, call_id, arguments).await?;
                let output = if result.is_error {
                    format!("ERROR: {}", result.text)
                } else {
                    result.text
                };
                outputs.push(json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": output,
                }));
            }
            input = json!(outputs);
        }
        Err(TurnError::Api {
            message: format!(
                "the completion-and-tool loop exceeded {} iterations without quiescing",
                self.config.max_loop_iterations
            ),
        })
    }
}

impl OpenAiProvider {
    async fn stream_once(
        &self,
        key: &str,
        body: &Value,
        events: &TurnEventSink,
        resumed_from: Option<&str>,
    ) -> Result<ModelResponse, TurnError> {
        let mut attempt = 0;
        let http_response = loop {
            events.emit(TurnEvent::Liveness);
            let sent = self
                .http
                .post(format!("{}/v1/responses", self.config.base_url))
                .header("authorization", format!("Bearer {key}"))
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
                    return Err(classify_status(status, &detail, resumed_from));
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

        let limit = header_limit_status(
            http_response.headers(),
            "x-ratelimit-remaining-requests",
            "x-ratelimit-remaining-tokens",
        );
        if !limit.is_empty() {
            events.emit(TurnEvent::LimitObserved { limit });
        }

        let mut response = ModelResponse::default();
        read_sse(http_response, |frame| {
            let Ok(data) = serde_json::from_str::<Value>(&frame.data) else {
                return;
            };
            match frame.event.as_str() {
                "response.output_text.delta" => {
                    if let Some(delta) = data["delta"].as_str() {
                        response.text.push_str(delta);
                    }
                }
                "response.completed" => {
                    let payload = &data["response"];
                    response.response_id = payload["id"].as_str().unwrap_or_default().to_owned();
                    response.input_tokens = payload
                        .pointer("/usage/input_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    response.output_tokens = payload
                        .pointer("/usage/output_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    response.cached_input_tokens = payload
                        .pointer("/usage/input_tokens_details/cached_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    response.reasoning_output_tokens = payload
                        .pointer("/usage/output_tokens_details/reasoning_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    for item in payload["output"].as_array().into_iter().flatten() {
                        if item["type"] == "function_call" {
                            let arguments = item["arguments"]
                                .as_str()
                                .and_then(|raw| serde_json::from_str(raw).ok())
                                .unwrap_or_else(|| item["arguments"].clone());
                            response.function_calls.push((
                                item["call_id"].as_str().unwrap_or_default().to_owned(),
                                item["name"].as_str().unwrap_or_default().to_owned(),
                                arguments,
                            ));
                        }
                    }
                }
                "error" => {
                    response.status_error =
                        Some(data["message"].as_str().unwrap_or("unspecified").to_owned());
                }
                _ => {}
            }
            events.emit(TurnEvent::Liveness);
        })
        .await?;
        Ok(response)
    }
}

/// Map a terminal HTTP status onto the taxonomy. A 404 while resuming a
/// response chain means the stored response is gone — the session, as far
/// as this tier is concerned.
fn classify_status(
    status: reqwest::StatusCode,
    detail: &str,
    resumed_from: Option<&str>,
) -> TurnError {
    let head: String = detail.chars().take(300).collect();
    match status.as_u16() {
        401 | 403 => TurnError::Config {
            message: format!(
                "the OpenAI API rejected the credentials ({status}): check that \
                 OPENAI_API_KEY is set to a valid key for this project. {head}"
            ),
        },
        404 if resumed_from.is_some() => TurnError::SessionNotFound {
            session_id: resumed_from.unwrap_or_default().to_owned(),
        },
        400 | 404 | 422 => TurnError::Config {
            message: format!("the OpenAI API rejected the request ({status}): {head}"),
        },
        _ => TurnError::Api {
            message: format!("OpenAI API failure ({status}) after retries: {head}"),
        },
    }
}

/// Map the neutral effort ladder onto Responses `reasoning.effort`. The
/// levels `none` through `xhigh` pass verbatim — which of them a given
/// model accepts is the API's per-model contract, and a rejection comes
/// back as a typed configuration error through [`classify_status`]. `max`
/// exists on no OpenAI surface, so it fails typed before any request.
fn openai_effort(effort: ReasoningEffort) -> Result<&'static str, TurnError> {
    match effort {
        ReasoningEffort::None
        | ReasoningEffort::Minimal
        | ReasoningEffort::Low
        | ReasoningEffort::Medium
        | ReasoningEffort::High
        | ReasoningEffort::XHigh => Ok(effort.as_str()),
        ReasoningEffort::Max => Err(TurnError::Config {
            message: "the OpenAI Responses API has no `max` reasoning effort; its ladder \
                      tops out at xhigh (model-dependent) — configure the agent with \
                      ReasoningEffort::XHigh or leave effort unset for the model default"
                .to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effort_passes_the_ladder_verbatim_and_rejects_max_typed() {
        for (level, wire) in [
            (ReasoningEffort::None, "none"),
            (ReasoningEffort::Minimal, "minimal"),
            (ReasoningEffort::Low, "low"),
            (ReasoningEffort::Medium, "medium"),
            (ReasoningEffort::High, "high"),
            (ReasoningEffort::XHigh, "xhigh"),
        ] {
            assert_eq!(openai_effort(level).expect("level passes"), wire);
        }
        match openai_effort(ReasoningEffort::Max) {
            Err(TurnError::Config { message }) => assert!(message.contains("xhigh")),
            other => panic!("expected a typed configuration error, got {other:?}"),
        }
    }
}
