//! The provider seam: one implementation per model backend, one call per
//! **harness turn**.
//!
//! This module is the frozen boundary between the runner and every model
//! backend (freeze: EOD 2026-08-22). The unit of work is deliberately a
//! *turn*, not a model completion: a subscription provider (headless Claude
//! Code, Codex app-server) spawns or resumes its harness, lets the harness
//! run its own inner tool loop, streams liveness out, and returns the
//! terminal result. An API-backed provider (secondary tier, behind
//! features) implements the same shape by running its internal
//! completion-and-tool loop to quiescence inside `execute_turn`.
//!
//! The trait lives in `odori-agents` — not `odori-providers` — because both
//! sides of the seam need types from this crate: the runner calls the trait,
//! and implementations read the agent's directives. Placing it here keeps
//! the dependency graph acyclic (`odori-providers` → `odori-agents`) and
//! keeps this crate free of subprocess machinery.
//!
//! Everything a provider needs at spawn time rides [`TurnRequest`], per the
//! mcp-bridge spec's attachment contract (`.kiro/specs/mcp-bridge/`):
//! MCP server injection ([`McpServerConfig`]), MCP timeout pinning
//! ([`TurnTooling::mcp_timeout`]), and the exit-classification taxonomy
//! ([`TurnError`], from the claude-driver spike's 4-tuple).

use std::{fmt, time::Duration};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::mpsc;

/// Executes one harness turn for one agent.
///
/// Implementations are supervisors, not model clients: spawn or resume the
/// backend, pump its event stream into `events` (the runner records these as
/// activity heartbeats — an implementation that never emits starves the
/// heartbeat timeout, by design), and return the turn's terminal outcome.
///
/// Contract:
///
/// - **One call, one turn.** No retries inside the provider beyond what the
///   backend itself performs; retry policy belongs to the turn activity.
/// - **Honor [`TurnRequest::session`].** `Start` must not silently attach to
///   prior state; `Resume`/`ResumeForked` must fail with
///   [`TurnError::SessionNotFound`] when the named session is gone.
/// - **Classify exits.** Failures map onto [`TurnError`]'s taxonomy — never
///   a stringly-typed catch-all — so the activity layer can decide
///   retryability mechanically via [`TurnError::is_retryable`].
/// - **Scrub the environment.** Subprocess providers must clear inherited
///   vendor transport/credential variables before spawn (claude-driver
///   spike finding: an inherited `ANTHROPIC_BASE_URL` fails auth hard).
#[async_trait]
pub trait Provider: fmt::Debug + Send + Sync + 'static {
    /// Stable identifier used in agent configuration and diagnostics
    /// (e.g. `"claude"`, `"codex"`, `"anthropic-api"`).
    fn name(&self) -> &str;

    /// Run one turn to its terminal result.
    async fn execute_turn(
        &self,
        request: TurnRequest,
        events: TurnEventSink,
    ) -> Result<TurnOutcome, TurnError>;
}

/// Everything a provider receives for one turn.
///
/// Constructed by the runner's turn activity; providers only read it.
/// `#[non_exhaustive]` so the seam can grow additively after the freeze.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TurnRequest {
    /// Durable identity of this turn execution. Providers stamp it on
    /// everything they emit (logs, MCP `_meta`, exit reports) so the
    /// workflow side can correlate and fence.
    pub identity: TurnIdentity,
    /// The agent's declarative direction for the backend.
    pub directives: AgentDirectives,
    /// The user-visible input for this turn (the prompt).
    pub input: String,
    /// Start a fresh backend session, or resume an existing one.
    pub session: SessionDirective,
    /// Tool exposure for the turn: native-tool scoping and MCP attachment.
    pub tooling: TurnTooling,
    /// Provider-side deadline for the whole turn. The turn activity's
    /// start-to-close timeout sits above this; the provider should fail
    /// with [`TurnError::Timeout`] rather than be killed from outside.
    pub deadline: Option<Duration>,
}

impl TurnRequest {
    /// Assemble a request. Arguments beyond these have defaults; set them
    /// through the public fields.
    pub fn new(
        identity: TurnIdentity,
        directives: AgentDirectives,
        input: impl Into<String>,
        session: SessionDirective,
    ) -> Self {
        Self {
            identity,
            directives,
            input: input.into(),
            session,
            tooling: TurnTooling::default(),
            deadline: None,
        }
    }
}

/// Durable coordinates of one turn execution attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnIdentity {
    /// The run (workflow execution) this turn belongs to.
    pub run_id: String,
    /// Zero-based turn index within the run.
    pub turn: u32,
    /// The activity attempt executing this turn (1-based, monotonic).
    /// Superseded attempts are fenced by the mcp-bridge registry.
    pub attempt: u32,
}

/// The declarative slice of an agent a backend needs: what to be, not how
/// to run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct AgentDirectives {
    /// Agent name (diagnostics and session labeling).
    pub name: String,
    /// System-level instructions for the backend.
    pub instructions: String,
    /// Backend model selector, verbatim (e.g. a `--model` value). `None`
    /// means the backend's default.
    pub model: Option<String>,
    /// JSON Schema the final result must satisfy, where the backend can
    /// enforce it (headless Claude Code: `--json-schema`). Typed-output
    /// parsing happens runner-side either way.
    pub output_schema: Option<Value>,
}

impl AgentDirectives {
    /// Directives with only the required fields set.
    pub fn new(name: impl Into<String>, instructions: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            instructions: instructions.into(),
            model: None,
            output_schema: None,
        }
    }
}

/// Whether the turn opens a fresh backend session or continues one.
///
/// The runner chooses; the provider executes. Within-turn retry recovery
/// (a retried attempt resuming the dead attempt's session) also flows
/// through here — the turn activity rewrites the directive from recovered
/// heartbeat details before calling the provider again.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SessionDirective {
    /// Open a fresh session.
    Start,
    /// Continue the named session in place (same session id).
    Resume {
        /// Backend session id, as reported by a prior [`TurnOutcome`].
        session_id: String,
    },
    /// Continue from the named session's history under a **new** session id
    /// (headless Claude Code: `--fork-session`). The retry-isolation
    /// primitive: a retried attempt's divergence never contaminates the
    /// recorded lineage.
    ResumeForked {
        /// Backend session id to fork from.
        session_id: String,
    },
}

/// Tool exposure for one turn.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TurnTooling {
    /// Backend-native tools the harness may use, in the backend's own
    /// naming (headless Claude Code: `--allowedTools` values). `None`
    /// leaves the backend's defaults untouched; `Some(vec![])` disallows
    /// native tooling.
    pub allowed_native_tools: Option<Vec<String>>,
    /// MCP servers to attach at spawn. With the mcp-bridge active this
    /// carries the bridge endpoint; it accepts any MCP server config.
    pub mcp_servers: Vec<McpServerConfig>,
    /// Pin for the backend's MCP client timeout, so the bridge's keepalive
    /// cadence has a known bound to stay under (mcp-bridge Requirement
    /// 5.3). Ignored when the backend has no such knob.
    pub mcp_timeout: Option<Duration>,
}

/// One MCP server attachment, in the two transports the mcp-bridge spec
/// commits to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Server name as the model sees it (`mcp__{name}__{tool}`).
    pub name: String,
    /// How the backend reaches the server.
    pub transport: McpTransport,
}

/// Transport for one MCP server attachment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum McpTransport {
    /// Streamable HTTP endpoint (the bridge's primary transport).
    Http {
        /// Endpoint URL.
        url: String,
        /// Headers to send on every request (e.g. the bridge's per-run
        /// bearer token).
        headers: Vec<(String, String)>,
    },
    /// A stdio server the backend spawns itself (the bridge's committed
    /// fallback shim, and the general MCP case).
    Stdio {
        /// Executable to spawn.
        command: String,
        /// Arguments, verbatim.
        args: Vec<String>,
        /// Environment entries set for the spawned server.
        env: Vec<(String, String)>,
    },
}

/// Sink for in-flight turn events.
///
/// Lossy by design: emission never blocks the provider, and dropped events
/// cost at most one heartbeat interval. The terminal result never travels
/// this channel — it is `execute_turn`'s return value.
#[derive(Debug, Clone)]
pub struct TurnEventSink {
    sender: mpsc::Sender<TurnEvent>,
}

impl TurnEventSink {
    /// Wrap a channel sender. The receiving half belongs to the runner's
    /// turn activity, which forwards events into activity heartbeats.
    pub fn new(sender: mpsc::Sender<TurnEvent>) -> Self {
        Self { sender }
    }

    /// Emit an event, dropping it if the receiver is full or gone.
    pub fn emit(&self, event: TurnEvent) {
        let _ = self.sender.try_send(event);
    }
}

/// In-flight observations from a running turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum TurnEvent {
    /// The backend session exists and is identified. Providers must emit
    /// this as soon as the id is known: it is the recovery anchor a retried
    /// attempt uses to resume the session (via heartbeat details).
    SessionStarted {
        /// Backend session id.
        session_id: String,
    },
    /// The backend showed liveness (a stream event arrived). Cheap and
    /// frequent; the activity throttles as needed.
    Liveness,
    /// The backend invoked a tool (native or MCP).
    ToolUse {
        /// Tool name as the backend reported it.
        name: String,
    },
}

/// The terminal result of a successful turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TurnOutcome {
    /// Backend session id the turn ran under (fresh, resumed, or the new id
    /// of a fork). This is the id a later turn resumes.
    pub session_id: String,
    /// The turn's final text. Typed-output parsing happens runner-side.
    pub text: String,
    /// Cost and volume accounting, best-effort per backend.
    pub usage: TurnUsage,
}

impl TurnOutcome {
    /// An outcome with empty usage; fill [`TurnOutcome::usage`] in place.
    pub fn new(session_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            text: text.into(),
            usage: TurnUsage::default(),
        }
    }
}

/// Best-effort turn accounting, used by the runner's budget guardrails.
///
/// `None` means the backend did not report the figure — the budget
/// enforcement treats unknown as zero and says so in its docs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TurnUsage {
    /// Dollar cost of the turn as the backend reported it.
    pub total_cost_usd: Option<f64>,
    /// Input tokens consumed.
    pub input_tokens: Option<u64>,
    /// Output tokens produced.
    pub output_tokens: Option<u64>,
    /// Wall-clock duration of the turn.
    pub duration: Option<Duration>,
}

/// Why a turn failed, in the classes the activity layer retries by.
///
/// Grounded in the claude-driver spike's finding that exit codes alone
/// separate nothing: classification keys on the 4-tuple (exit code,
/// terminal-result-arrived, terminal reason, stderr). Retryability is a
/// property of the class, exposed via [`TurnError::is_retryable`] and
/// [`TurnError::error_type`] so the activity maps it mechanically onto the
/// engine's retry machinery.
#[derive(Debug, Clone, Serialize, Deserialize, Error)]
#[non_exhaustive]
pub enum TurnError {
    /// The backend reported an API-level failure (its own retries
    /// exhausted). Retryable with backoff.
    #[error("backend api failure: {message}")]
    Api {
        /// Backend-reported reason.
        message: String,
    },
    /// A resume named a session the backend no longer has. Non-retryable:
    /// the run loop must decide (typically: replay the turn fresh).
    #[error("backend session not found: {session_id}")]
    SessionNotFound {
        /// The session id that failed to resume.
        session_id: String,
    },
    /// The backend process died without a terminal result. Retryable; the
    /// retried attempt resumes the session recovered from heartbeats.
    #[error("harness died without a terminal result (exit {exit_code:?})")]
    HarnessDied {
        /// Process exit code; `None` when killed by a signal.
        exit_code: Option<i32>,
        /// First line of captured stderr, for diagnostics.
        stderr_head: String,
    },
    /// Spawn or configuration defect (binary missing, bad flags, invalid
    /// attachment). Non-retryable: retrying reproduces it.
    #[error("provider configuration error: {message}")]
    Config {
        /// What was wrong.
        message: String,
    },
    /// The provider-side deadline elapsed. Retryable.
    #[error("turn exceeded its deadline after {elapsed:?}")]
    Timeout {
        /// Time spent before the provider gave up.
        elapsed: Duration,
    },
    /// The MCP attachment or another tooling seam failed mid-turn in a way
    /// the backend could not surface as a tool result. Retryable.
    #[error("tooling failure: {message}")]
    Tooling {
        /// What broke.
        message: String,
    },
}

impl TurnError {
    /// Whether the turn activity should let its retry policy re-run the
    /// turn.
    pub fn is_retryable(&self) -> bool {
        match self {
            TurnError::Api { .. }
            | TurnError::HarnessDied { .. }
            | TurnError::Timeout { .. }
            | TurnError::Tooling { .. } => true,
            TurnError::SessionNotFound { .. } | TurnError::Config { .. } => false,
        }
    }

    /// Stable machine-readable class name, recorded as the application
    /// failure's error type (usable in retry-policy
    /// `non_retryable_error_types` matching).
    pub fn error_type(&self) -> &'static str {
        match self {
            TurnError::Api { .. } => "odori::turn::api",
            TurnError::SessionNotFound { .. } => "odori::turn::session_not_found",
            TurnError::HarnessDied { .. } => "odori::turn::harness_died",
            TurnError::Config { .. } => "odori::turn::config",
            TurnError::Timeout { .. } => "odori::turn::timeout",
            TurnError::Tooling { .. } => "odori::turn::tooling",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryability_follows_the_taxonomy() {
        let cases = [
            (
                TurnError::Api {
                    message: "429".into(),
                },
                true,
            ),
            (
                TurnError::SessionNotFound {
                    session_id: "s".into(),
                },
                false,
            ),
            (
                TurnError::HarnessDied {
                    exit_code: Some(143),
                    stderr_head: String::new(),
                },
                true,
            ),
            (
                TurnError::Config {
                    message: "no binary".into(),
                },
                false,
            ),
            (
                TurnError::Timeout {
                    elapsed: Duration::from_secs(1),
                },
                true,
            ),
            (
                TurnError::Tooling {
                    message: "mcp".into(),
                },
                true,
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(error.is_retryable(), expected, "{error:?}");
            assert!(error.error_type().starts_with("odori::turn::"));
        }
    }

    #[test]
    fn event_sink_is_lossy_not_blocking() {
        let (sender, mut receiver) = mpsc::channel(1);
        let sink = TurnEventSink::new(sender);
        sink.emit(TurnEvent::Liveness);
        sink.emit(TurnEvent::Liveness); // dropped: capacity 1, nothing recv'd
        assert!(receiver.try_recv().is_ok());
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn turn_request_round_trips_through_serde() {
        let request = TurnRequest::new(
            TurnIdentity {
                run_id: "r".into(),
                turn: 3,
                attempt: 2,
            },
            AgentDirectives::new("a", "be helpful"),
            "hello",
            SessionDirective::ResumeForked {
                session_id: "s0".into(),
            },
        );
        let json = serde_json::to_string(&request).expect("serialize");
        let back: TurnRequest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.identity.turn, 3);
        assert!(matches!(
            back.session,
            SessionDirective::ResumeForked { .. }
        ));
    }
}
