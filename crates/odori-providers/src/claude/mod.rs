//! The Claude subscription provider: headless Claude Code as a supervised
//! subprocess, one `claude -p` invocation per harness turn.
//!
//! Supervision model (ground truth: the retired claude-driver spike,
//! preserved in `.kiro/specs/mcp-bridge/requirements.md` § Evidence):
//! spawn with a scrubbed environment, parse the stream-json JSONL from
//! stdout with unknown-event tolerance, emit liveness into the turn's
//! event sink (the runner records these as activity heartbeats), capture
//! the terminal `result` event, **drain to EOF** (the result is not always
//! the last line), join the exit, and classify by the 4-tuple —
//! (exit code, result-event-arrived, terminal reason, stderr) — extended
//! with *died awaiting MCP*: bridge `tool_use` events with no answering
//! `tool_result` at death.
//!
//! Session semantics: `Start` spawns fresh; `Resume` passes `--resume`;
//! `ResumeForked` adds `--fork-session` (retry isolation). The runner's
//! turn activity chooses the directive — including heartbeat-recovered
//! resume on retry — and this provider only executes it.
//!
//! The harness is a **pinned dependency**: [`PINNED_VERSION`] is what this
//! provider is conformance-tested against. Version drift warns loudly but
//! does not fail (protocol tolerance is built in); a missing binary fails
//! with install guidance, because "the harness is not installed" is a
//! launch-risk error whose text is the mitigation.

pub mod events;

use std::{
    collections::HashSet,
    path::PathBuf,
    process::Stdio,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use odori_agents::provider::{
    Provider, SessionDirective, TurnError, TurnEvent, TurnEventSink, TurnOutcome, TurnRequest,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    process::Command,
    sync::OnceCell,
};

use crate::claude_flags::render_tooling;
use events::{ContentBlock, ResultEvent, StreamEvent, UserBlock};

/// The Claude Code version this provider is conformance-tested against.
pub const PINNED_VERSION: &str = "2.1.220";

/// Vendor transport/credential variables scrubbed before spawn: a harness
/// inheriting another agent session's `ANTHROPIC_BASE_URL` fails auth hard
/// (spike finding), and the harness must authenticate from its own store.
const SCRUBBED_ENV: &[&str] = &[
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_API_KEY",
];

/// Configuration for [`ClaudeProvider`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ClaudeConfig {
    /// The harness binary; `"claude"` resolves via `PATH`. Point it at a
    /// test double for scripted runs.
    pub binary: PathBuf,
    /// Fallback turn deadline when the request carries none.
    pub default_deadline: Duration,
    /// Extra environment entries for the spawned harness (applied after
    /// scrubbing, so a deliberate operator override wins).
    pub extra_env: Vec<(String, String)>,
}

impl Default for ClaudeConfig {
    fn default() -> Self {
        Self {
            binary: PathBuf::from("claude"),
            default_deadline: Duration::from_secs(600),
            extra_env: Vec::new(),
        }
    }
}

impl ClaudeConfig {
    /// Use a specific harness binary.
    pub fn with_binary(mut self, binary: impl Into<PathBuf>) -> Self {
        self.binary = binary.into();
        self
    }

    /// Add an environment entry for the spawned harness.
    pub fn with_env(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra_env.push((name.into(), value.into()));
        self
    }
}

/// The provider. One instance serves any number of concurrent turns.
#[derive(Debug)]
pub struct ClaudeProvider {
    config: ClaudeConfig,
    version: OnceCell<String>,
}

impl ClaudeProvider {
    /// A provider over the `claude` binary on `PATH`.
    pub fn new() -> Self {
        Self::with_config(ClaudeConfig::default())
    }

    /// A provider with explicit configuration.
    pub fn with_config(config: ClaudeConfig) -> Self {
        Self {
            config,
            version: OnceCell::new(),
        }
    }

    /// The detected harness version, once a turn (or an explicit probe)
    /// has run.
    pub fn detected_version(&self) -> Option<&str> {
        self.version.get().map(String::as_str)
    }

    /// Detect the harness version, caching the answer. Missing or broken
    /// binaries produce the operator-empathy error this launch risk calls
    /// for.
    pub async fn ensure_version(&self) -> Result<&str, TurnError> {
        self.version
            .get_or_try_init(|| async {
                let mut probe = Command::new(&self.config.binary);
                probe.arg("--version").stdin(Stdio::null());
                for (name, value) in &self.config.extra_env {
                    probe.env(name, value);
                }
                let output = probe
                    .output()
                    .await
                    .map_err(|error| missing_harness(&self.config.binary, &error))?;
                if !output.status.success() {
                    return Err(TurnError::Config {
                        message: format!(
                            "`{} --version` failed (exit {:?}): {}",
                            self.config.binary.display(),
                            output.status.code(),
                            String::from_utf8_lossy(&output.stderr)
                                .lines()
                                .next()
                                .unwrap_or(""),
                        ),
                    });
                }
                let stdout = String::from_utf8_lossy(&output.stdout);
                let version = stdout.split_whitespace().next().unwrap_or("").to_owned();
                if version.is_empty() {
                    return Err(TurnError::Config {
                        message: format!(
                            "`{} --version` produced no parsable version: {stdout:?}",
                            self.config.binary.display()
                        ),
                    });
                }
                if version != PINNED_VERSION {
                    tracing::warn!(
                        detected = %version,
                        pinned = %PINNED_VERSION,
                        "Claude Code version drift: this provider is conformance-tested \
                         against the pinned version; the stream protocol is tolerant, but \
                         re-verify before relying on new behaviour"
                    );
                }
                tracing::info!(version = %version, "claude harness detected");
                Ok(version)
            })
            .await
            .map(String::as_str)
    }

    fn build_command(&self, request: &TurnRequest) -> Command {
        let mut cmd = Command::new(&self.config.binary);
        match &request.session {
            SessionDirective::Start => {}
            SessionDirective::Resume { session_id } => {
                cmd.arg("--resume").arg(session_id);
            }
            SessionDirective::ResumeForked { session_id } => {
                cmd.arg("--resume").arg(session_id).arg("--fork-session");
            }
        }
        cmd.arg("-p")
            .arg(&request.input)
            .args(["--output-format", "stream-json", "--verbose"]);
        if !request.directives.instructions.is_empty() {
            cmd.arg("--append-system-prompt")
                .arg(&request.directives.instructions);
        }
        if let Some(model) = &request.directives.model {
            cmd.arg("--model").arg(model);
        }
        if let Some(schema) = &request.directives.output_schema {
            cmd.arg("--json-schema").arg(schema.to_string());
        }
        let tooling = render_tooling(&request.tooling);
        cmd.args(&tooling.args);
        for (name, value) in &tooling.env {
            cmd.env(name, value);
        }
        for var in SCRUBBED_ENV {
            cmd.env_remove(var);
        }
        for (name, value) in &self.config.extra_env {
            cmd.env(name, value);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        cmd
    }
}

impl Default for ClaudeProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for ClaudeProvider {
    fn name(&self) -> &str {
        "claude"
    }

    async fn execute_turn(
        &self,
        request: TurnRequest,
        events: TurnEventSink,
    ) -> Result<TurnOutcome, TurnError> {
        self.ensure_version().await?;
        let deadline = request.deadline.unwrap_or(self.config.default_deadline);
        let started = Instant::now();

        let mut child = self
            .build_command(&request)
            .spawn()
            .map_err(|error| missing_harness(&self.config.binary, &error))?;
        let stdout = child
            .stdout
            .take()
            .unwrap_or_else(|| unreachable!("stdout piped"));
        let mut stderr = child
            .stderr
            .take()
            .unwrap_or_else(|| unreachable!("stderr piped"));
        let stderr_task = tokio::spawn(async move {
            let mut captured = String::new();
            let _ = stderr.read_to_string(&mut captured).await;
            captured
        });

        // Stream state: the terminal result, the session id, and the bridge
        // tool calls still awaiting their `tool_result` (for the
        // died-awaiting-MCP classification).
        let mut terminal: Option<ResultEvent> = None;
        let mut session_id: Option<String> = None;
        let mut pending_bridge_calls: HashSet<String> = HashSet::new();
        let mut lines = BufReader::new(stdout).lines();
        let timed_out = loop {
            let remaining = deadline.saturating_sub(started.elapsed());
            let line = match tokio::time::timeout(remaining, lines.next_line()).await {
                Err(_elapsed) => {
                    child.start_kill().ok();
                    break true;
                }
                Ok(Err(_read_error)) => break false,
                Ok(Ok(None)) => break false,
                Ok(Ok(Some(line))) => line,
            };
            if line.trim().is_empty() {
                continue;
            }
            events.emit(TurnEvent::Liveness);
            let Ok(event) = serde_json::from_str::<StreamEvent>(&line) else {
                // Protocol drift tolerance: unparsable lines are liveness.
                continue;
            };
            match event {
                StreamEvent::System(system) => {
                    if system.subtype == "init"
                        && let Some(id) = system.session_id
                    {
                        events.emit(TurnEvent::SessionStarted {
                            session_id: id.clone(),
                        });
                        session_id = Some(id);
                    }
                }
                StreamEvent::Assistant(assistant) => {
                    for block in assistant.message.content {
                        if let ContentBlock::ToolUse { id, name } = block {
                            if name.starts_with("mcp__") {
                                pending_bridge_calls.insert(id);
                            }
                            events.emit(TurnEvent::ToolUse { name });
                        }
                    }
                }
                StreamEvent::User(user) => {
                    if let Some(message) = user.message {
                        for block in message.content {
                            if let UserBlock::ToolResult { tool_use_id } = block {
                                pending_bridge_calls.remove(&tool_use_id);
                            }
                        }
                    }
                }
                StreamEvent::Result(result) => {
                    // Terminal semantically, but not always the last line:
                    // keep draining to EOF for trailing summaries.
                    terminal = Some(result);
                }
                StreamEvent::Other(_) => {}
            }
        };

        let status = child.wait().await.map_err(|error| TurnError::Config {
            message: format!("failed to join the harness process: {error}"),
        })?;
        let stderr_text = stderr_task.await.unwrap_or_default();
        let stderr_head = stderr_text.lines().next().unwrap_or_default().to_owned();

        if timed_out {
            return Err(TurnError::Timeout {
                elapsed: started.elapsed(),
            });
        }
        classify(
            terminal,
            status.code(),
            session_id,
            stderr_head,
            pending_bridge_calls,
        )
    }
}

/// The 4-tuple classification — (exit code, result-event-arrived, terminal
/// reason, stderr) — extended with died-awaiting-MCP. Everything non-zero
/// exits 1, so the tuple, not the code, separates the classes.
fn classify(
    terminal: Option<ResultEvent>,
    exit_code: Option<i32>,
    session_id: Option<String>,
    stderr_head: String,
    pending_bridge_calls: HashSet<String>,
) -> Result<TurnOutcome, TurnError> {
    let Some(result) = terminal else {
        // No terminal result: the harness died (or was killed by a signal).
        // Died-awaiting-MCP is its own class — the bridge's in-flight
        // executions run to completion and the resumed replay dedupes.
        if !pending_bridge_calls.is_empty() {
            let mut pending: Vec<String> = pending_bridge_calls.into_iter().collect();
            pending.sort();
            return Err(TurnError::HarnessDiedAwaitingTools {
                exit_code,
                stderr_head,
                pending_calls: pending,
            });
        }
        // Usage-error shape from the spike: instant exit, empty stdout.
        if stderr_head.contains("error: unknown option")
            || stderr_head.contains("error: missing required argument")
        {
            return Err(TurnError::Config {
                message: format!("the harness rejected its invocation: {stderr_head}"),
            });
        }
        return Err(TurnError::HarnessDied {
            exit_code,
            stderr_head,
        });
    };

    // A result event arrived; `is_error` is authoritative (subtype lies).
    if !result.is_error {
        let mut outcome = TurnOutcome::new(
            session_id.unwrap_or_else(|| result.session_id.clone()),
            result.result.clone().unwrap_or_default(),
        );
        outcome.usage.total_cost_usd = result.total_cost_usd;
        outcome.usage.input_tokens = result.usage.as_ref().and_then(|usage| usage.input_tokens);
        outcome.usage.output_tokens = result.usage.as_ref().and_then(|usage| usage.output_tokens);
        outcome.usage.duration = result.duration_ms.map(Duration::from_millis);
        return Ok(outcome);
    }

    let text = result.result.clone().unwrap_or_default();
    let haystack = format!("{text} {stderr_head}").to_lowercase();

    // Resume of a session the harness no longer has: non-retryable; the
    // run loop decides (the reason arrives on stderr, spike trap).
    if haystack.contains("no conversation found") {
        return Err(TurnError::SessionNotFound {
            session_id: result.session_id,
        });
    }
    // Auth failures are terminal with re-auth guidance: retrying cannot
    // mint credentials.
    if haystack.contains("authenticate") || haystack.contains("oauth") || haystack.contains("401") {
        return Err(TurnError::Config {
            message: format!(
                "the Claude harness is not authenticated: {text}. Run `claude login` (or \
                 `claude setup-token` for headless machines) with the operator's \
                 subscription, then retry the run"
            ),
        });
    }
    // Subscription usage caps are terminal until the window resets —
    // retry-with-backoff inside a turn budget would burn the whole budget
    // against a wall. Rate limits (below) are the retryable class.
    if haystack.contains("usage limit") || haystack.contains("out of extended usage") {
        return Err(TurnError::Config {
            message: format!(
                "the subscription's usage cap is exhausted: {text}. Wait for the window \
                 to reset (or raise the plan) and re-run"
            ),
        });
    }
    // Everything else that produced an error result is API-side and
    // retryable with backoff — including rate limits, which the CLI also
    // retries internally (api_retry events, max 10).
    Err(TurnError::Api {
        message: format!(
            "harness reported an API failure ({}): {text}",
            result.terminal_reason.as_deref().unwrap_or("unspecified"),
        ),
    })
}

/// The operator-empathy error for a missing or unspawnable harness.
fn missing_harness(binary: &std::path::Path, error: &std::io::Error) -> TurnError {
    TurnError::Config {
        message: format!(
            "could not run the Claude Code harness `{}`: {error}. Odori's Claude provider \
             drives the Claude Code CLI as a subprocess — install it with \
             `npm install -g @anthropic-ai/claude-code` (or see \
             https://claude.com/claude-code), make sure `claude` is on PATH (or set \
             ClaudeConfig::binary), and authenticate it once with `claude login`",
            binary.display(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use odori_agents::provider::{AgentDirectives, SessionDirective, TurnIdentity};
    use proptest::prelude::*;

    use super::*;

    // Feature: mcp-bridge, Property 6: failure classes are preserved
    // (provider leg; the tool- and bridge-failure legs are proven in the
    // O6 suites). For any synthetic exit shape — result-event presence,
    // error flags, reason text, exit code, stderr, pending bridge calls —
    // classification lands in exactly the taxonomy's class for that shape,
    // never a neighbour's.
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn p6_classification_partitions_exit_shapes(
            has_result in proptest::bool::ANY,
            is_error in proptest::bool::ANY,
            reason in prop_oneof![
                Just("completed"), Just("api_error"),
                Just("no conversation found"), Just("authenticate"),
                Just("usage limit"), Just("transient blip"),
            ],
            exit_code in proptest::option::of(0i32..255),
            pending in proptest::collection::vec("[a-z]{4}", 0..3),
        ) {
            let terminal = has_result.then(|| ResultEvent {
                subtype: "success".into(),
                is_error,
                session_id: "sess-p".into(),
                result: Some(reason.to_owned()),
                terminal_reason: Some(reason.to_owned()),
                total_cost_usd: None,
                duration_ms: None,
                usage: None,
            });
            let pending: std::collections::HashSet<String> =
                pending.into_iter().collect();
            let outcome = classify(
                terminal,
                exit_code,
                Some("sess-p".into()),
                String::new(),
                pending.clone(),
            );
            match outcome {
                Ok(_) => prop_assert!(has_result && !is_error),
                Err(TurnError::HarnessDiedAwaitingTools { pending_calls, .. }) => {
                    prop_assert!(!has_result && !pending.is_empty());
                    prop_assert_eq!(pending_calls.len(), pending.len());
                }
                Err(TurnError::HarnessDied { .. }) => {
                    prop_assert!(!has_result && pending.is_empty());
                }
                Err(TurnError::SessionNotFound { .. }) => {
                    prop_assert!(has_result && is_error);
                    prop_assert!(reason.contains("no conversation found"));
                }
                Err(TurnError::Config { .. }) => {
                    prop_assert!(has_result && is_error);
                    prop_assert!(
                        reason.contains("authenticate") || reason.contains("usage limit")
                    );
                }
                Err(TurnError::Api { .. }) => {
                    prop_assert!(has_result && is_error);
                    prop_assert!(
                        !reason.contains("no conversation found")
                            && !reason.contains("authenticate")
                            && !reason.contains("usage limit")
                    );
                }
                Err(other) => prop_assert!(false, "unexpected class: {other:?}"),
            }
        }
    }

    #[test]
    fn vendor_transport_env_is_scrubbed_at_spawn() {
        let provider = ClaudeProvider::new();
        let request = TurnRequest::new(
            TurnIdentity {
                run_id: "r".into(),
                turn: 0,
                attempt: 1,
            },
            AgentDirectives::new("a", "i"),
            "hello",
            SessionDirective::Start,
        );
        let cmd = provider.build_command(&request);
        let removed: Vec<&str> = cmd
            .as_std()
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .filter_map(|(name, _)| name.to_str())
            .collect();
        for var in SCRUBBED_ENV {
            assert!(removed.contains(var), "{var} must be scrubbed: {removed:?}");
        }
    }
}
