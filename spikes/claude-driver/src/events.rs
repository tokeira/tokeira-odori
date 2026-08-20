//! Typed views of the headless Claude Code stream-json protocol, shaped by
//! observation (claude 2.1.220) rather than by any published schema — the
//! protocol is vendor-versioned, so every type here keeps an `Other`/extra
//! escape hatch and the provider must treat unknown events as skippable.

// The types document the full observed protocol surface for O3; the spike's
// probes read only part of it.
#![allow(dead_code)]

use serde::Deserialize;
use serde_json::Value;

/// One line of `--output-format stream-json` stdout.
///
/// Observed kinds: `system` (subtypes `init`, `api_retry`), `assistant`,
/// `user` (tool results echoed back), and the terminal `result`. Anything
/// else deserializes as `Other` and is logged, not fatal: harness protocol
/// drift is an expected failure mode, not a parse error.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    System(SystemEvent),
    Assistant(AssistantEvent),
    User(Value),
    Result(ResultEvent),
    #[serde(untagged)]
    Other(Value),
}

/// `{"type":"system", ...}` — session lifecycle and transport notices.
#[derive(Debug, Deserialize)]
pub struct SystemEvent {
    pub subtype: String,
    /// Present on `init`: the id every later resume must quote.
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// `api_retry` carries attempt/max_retries/error_status; kept raw.
    #[serde(flatten)]
    pub rest: Value,
}

/// `{"type":"assistant","message":{...}}` — one assistant message, complete
/// unless `--include-partial-messages` requested chunks.
#[derive(Debug, Deserialize)]
pub struct AssistantEvent {
    pub message: AssistantMessage,
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AssistantMessage {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub content: Vec<ContentBlock>,
    #[serde(default)]
    pub usage: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    #[serde(untagged)]
    Other(Value),
}

/// The terminal `{"type":"result", ...}` line — exactly one per run, last on
/// stdout, present even when the run fails (auth error, missing session).
#[derive(Debug, Deserialize)]
pub struct ResultEvent {
    /// `success` or `error_during_execution` — orthogonal to `is_error`
    /// (an api_error run observed `subtype: "success"` with
    /// `is_error: true`), so the provider must key on `is_error`.
    pub subtype: String,
    pub is_error: bool,
    /// On resume-of-missing-session this echoes the *requested* id, so it
    /// cannot be trusted as proof the session exists.
    pub session_id: String,
    #[serde(default)]
    pub num_turns: Option<u64>,
    /// Final text on success; an error sentence on failure. The
    /// human-readable cause of process-level failures lands on stderr, not
    /// here.
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default)]
    pub total_cost_usd: Option<f64>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub duration_api_ms: Option<u64>,
    /// e.g. `api_error`; absent on clean completion.
    #[serde(default)]
    pub terminal_reason: Option<String>,
    #[serde(default)]
    pub usage: Option<Value>,
}
