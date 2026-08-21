//! Typed views of headless Claude Code's stream-json protocol.
//!
//! Shaped by observation (the retired claude-driver spike, claude 2.1.220;
//! findings preserved in `.kiro/specs/mcp-bridge/requirements.md` §
//! Evidence), not by a published schema: the protocol is vendor-versioned,
//! new event kinds appear mid-stream on ordinary runs (`rate_limit_event`,
//! `system:thinking_tokens`), and every type here therefore keeps an
//! untagged escape hatch. Unknown events are liveness, never errors.

use serde::Deserialize;
use serde_json::Value;

/// One line of `--output-format stream-json` stdout.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    /// Session lifecycle and transport notices (`init`, `api_retry`,
    /// `task_summary`, …).
    System(SystemEvent),
    /// One assistant message; `tool_use` blocks carry the call ids the
    /// mcp-bridge fences by.
    Assistant(AssistantEvent),
    /// Tool results echoed back into the transcript.
    User(UserEvent),
    /// The terminal result: exactly one per run, present even on failures,
    /// but **not always the last line** — drain to EOF after it.
    Result(ResultEvent),
    /// Anything this pin does not know (`rate_limit_event`, future kinds).
    #[serde(untagged)]
    Other(Value),
}

/// `{"type":"system", ...}`.
#[derive(Debug, Deserialize)]
pub struct SystemEvent {
    /// `init`, `api_retry`, `task_summary`, `post_turn_summary`, ….
    pub subtype: String,
    /// Present on `init`: the session id every later resume quotes.
    #[serde(default)]
    pub session_id: Option<String>,
    /// The rest of the payload, kept raw for diagnostics.
    #[serde(flatten)]
    pub rest: Value,
}

/// `{"type":"assistant","message":{...}}`.
#[derive(Debug, Deserialize)]
pub struct AssistantEvent {
    /// The message body.
    pub message: AssistantMessage,
}

/// The assistant message payload.
#[derive(Debug, Deserialize)]
pub struct AssistantMessage {
    /// `"<synthetic>"` marks CLI-fabricated messages on API failures —
    /// filtered out of anything user-facing.
    #[serde(default)]
    pub model: Option<String>,
    /// Content blocks; tool calls appear here before they execute.
    #[serde(default)]
    pub content: Vec<ContentBlock>,
}

/// One content block of an assistant message.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// Final or interstitial text.
    Text {
        /// The text itself.
        text: String,
    },
    /// A tool invocation; `id` is the call id the bridge receives in
    /// `_meta["claudecode/toolUseId"]`.
    ToolUse {
        /// Harness-assigned tool-use id.
        id: String,
        /// Tool name (`mcp__{server}__{tool}` for bridge tools).
        name: String,
    },
    /// Thinking blocks and future kinds.
    #[serde(untagged)]
    Other(Value),
}

/// `{"type":"user", ...}` — tool results flowing back.
#[derive(Debug, Deserialize)]
pub struct UserEvent {
    /// The message body, kept raw except for tool-result correlation.
    #[serde(default)]
    pub message: Option<UserMessage>,
}

/// The user-side message payload (tool results).
#[derive(Debug, Deserialize)]
pub struct UserMessage {
    /// Content blocks; `tool_result` blocks resolve pending tool calls.
    #[serde(default)]
    pub content: Vec<UserBlock>,
}

/// One content block of a user message.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserBlock {
    /// A tool call's result, closing the loop on its `tool_use`.
    ToolResult {
        /// The `tool_use` id this result answers.
        tool_use_id: String,
    },
    /// Anything else.
    #[serde(untagged)]
    Other(Value),
}

/// The terminal `{"type":"result", ...}` line.
#[derive(Debug, Deserialize)]
pub struct ResultEvent {
    /// `success` or `error_during_execution`. **Trap:** can read
    /// `"success"` with `is_error: true` — key on `is_error`.
    pub subtype: String,
    /// The authoritative success flag.
    pub is_error: bool,
    /// On a fresh run, the session's id; on a failed resume it echoes the
    /// *requested* id (spike trap — not proof the session exists).
    pub session_id: String,
    /// Final text on success; an error sentence on failure.
    #[serde(default)]
    pub result: Option<String>,
    /// `"completed"` on success, `"api_error"` on API failure.
    #[serde(default)]
    pub terminal_reason: Option<String>,
    /// Dollar cost as the CLI reported it.
    #[serde(default)]
    pub total_cost_usd: Option<f64>,
    /// Wall-clock duration in milliseconds.
    #[serde(default)]
    pub duration_ms: Option<u64>,
    /// Token accounting.
    #[serde(default)]
    pub usage: Option<ResultUsage>,
}

/// The `usage` object of a result event.
#[derive(Debug, Deserialize)]
pub struct ResultUsage {
    /// Input tokens consumed.
    #[serde(default)]
    pub input_tokens: Option<u64>,
    /// Output tokens produced.
    #[serde(default)]
    pub output_tokens: Option<u64>,
}
