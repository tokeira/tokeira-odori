//! Subscription-backed Codex provider over the app-server JSON-RPC protocol.
//!
//! One [`Provider::execute_turn`] call owns one supervised app-server process.
//! The process starts or resumes one persisted Codex thread, executes one turn,
//! streams app-server notifications as activity liveness, then exits after the
//! terminal `turn/completed` notification. The protocol and MCP configuration
//! are conformance-tested against [`EXPECTED_CODEX_CLI_VERSION`].
//!
//! Codex's MCP tool timeout is a fixed wall-clock ceiling at this pin: progress
//! notifications do not extend it. Set [`TurnTooling::mcp_timeout`] above the
//! longest expected end-to-end tool execution. Per-server MCP allowlists are
//! enforced in app-server config; the broader native-tool boundary is also
//! supplied as a developer instruction because this app-server version does
//! not expose a global native-tool allowlist.

use std::{
    collections::{HashSet, VecDeque},
    io,
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    time::Duration,
};

use async_trait::async_trait;
use odori_agents::provider::{
    McpTransport, Provider, SessionDirective, TurnError, TurnEvent, TurnEventSink, TurnOutcome,
    TurnRequest, TurnTooling, TurnUsage,
};
use serde_json::{Map, Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout, Command},
};

/// Exact CLI pin used for the live O4 app-server and MCP conformance probes.
pub const EXPECTED_CODEX_CLI_VERSION: &str = "codex-cli 0.148.0-alpha.15";

const APP_SERVER_SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
const PROVIDER_NAME: &str = "codex";

/// A subscription-authenticated Codex app-server provider.
///
/// The default executable is `codex`. Use [`CodexProvider::with_command`] for
/// installations outside `PATH` and scripted harness tests. Version drift is
/// detected on every turn and warned through `tracing`; the actual thread also
/// records its creating CLI version in Codex's persisted metadata.
#[derive(Debug, Clone)]
pub struct CodexProvider {
    command: PathBuf,
}

impl Default for CodexProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl CodexProvider {
    /// Use the `codex` executable found on `PATH`.
    pub fn new() -> Self {
        Self {
            command: PathBuf::from("codex"),
        }
    }

    /// Use a specific Codex executable (also useful for scripted harnesses).
    pub fn with_command(command: impl Into<PathBuf>) -> Self {
        Self {
            command: command.into(),
        }
    }

    async fn execute(
        &self,
        request: TurnRequest,
        events: TurnEventSink,
    ) -> Result<TurnOutcome, TurnError> {
        self.check_version().await?;
        let deadline = request.deadline;
        let turn = run_app_server(self, request, events);
        match deadline {
            Some(deadline) => match tokio::time::timeout(deadline, turn).await {
                Ok(result) => result,
                Err(_) => Err(TurnError::Timeout { elapsed: deadline }),
            },
            None => turn.await,
        }
    }

    async fn check_version(&self) -> Result<(), TurnError> {
        let output = command_with_hygiene(&self.command)
            .arg("--version")
            .output()
            .await
            .map_err(|error| command_error(&self.command, "inspect its version", error))?;
        if !output.status.success() {
            return Err(TurnError::Config {
                message: format!(
                    "`{}` --version exited with {:?}; install Codex CLI {EXPECTED_CODEX_CLI_VERSION} and run `codex login`",
                    self.command.display(),
                    output.status.code()
                ),
            });
        }
        let actual = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if actual != EXPECTED_CODEX_CLI_VERSION {
            tracing::warn!(
                expected = EXPECTED_CODEX_CLI_VERSION,
                actual,
                "Codex CLI version drift; app-server protocol compatibility is not guaranteed"
            );
        }
        Ok(())
    }
}

#[async_trait]
impl Provider for CodexProvider {
    fn name(&self) -> &str {
        PROVIDER_NAME
    }

    async fn execute_turn(
        &self,
        request: TurnRequest,
        events: TurnEventSink,
    ) -> Result<TurnOutcome, TurnError> {
        self.execute(request, events).await
    }
}

async fn run_app_server(
    provider: &CodexProvider,
    request: TurnRequest,
    events: TurnEventSink,
) -> Result<TurnOutcome, TurnError> {
    let tooling = render_tooling(&request.tooling)?;
    let mut process = AppServerProcess::spawn(&provider.command, &tooling.env).await?;

    let initialize = process
        .request(
            "initialize",
            json!({
                "clientInfo": {
                    "name": "odori",
                    "title": "Odori Codex provider",
                    "version": env!("CARGO_PKG_VERSION")
                },
                // Required by `responsesapiClientMetadata`, which stamps the
                // durable run/turn/attempt coordinates onto upstream and MCP
                // turn metadata. App-server itself is experimental at this
                // CLI pin, and the generated schema gates this field too.
                "capabilities": {"experimentalApi": true}
            }),
            &events,
        )
        .await;
    if let Err(error) = initialize {
        return Err(finish_driver_error(process, error, DriverPhase::Initialize).await);
    }
    if let Err(error) = process.notify("initialized", Value::Null).await {
        return Err(finish_driver_error(process, error, DriverPhase::Initialize).await);
    }

    let current_dir = std::env::current_dir().map_err(|error| TurnError::Config {
        message: format!("cannot resolve the Codex turn working directory: {error}"),
    })?;
    let mut session_params = json!({
        "cwd": current_dir,
        "approvalPolicy": "never",
        "sandbox": "workspace-write",
        "developerInstructions": developer_instructions(&request),
        "serviceName": "odori",
        "ephemeral": false,
        "config": tooling.config
    });
    if let Some(model) = &request.directives.model {
        session_params["model"] = json!(model);
    }

    let (session_method, phase) = match &request.session {
        SessionDirective::Start => ("thread/start", DriverPhase::Start),
        SessionDirective::Resume { session_id } => {
            session_params["threadId"] = json!(session_id);
            ("thread/resume", DriverPhase::Resume(session_id.clone()))
        }
        SessionDirective::ResumeForked { session_id } => {
            session_params["threadId"] = json!(session_id);
            ("thread/fork", DriverPhase::Resume(session_id.clone()))
        }
    };
    let session_result = match process
        .request(session_method, session_params, &events)
        .await
    {
        Ok(result) => result,
        Err(error) => return Err(finish_driver_error(process, error, phase).await),
    };
    let session_id = match session_result.pointer("/thread/id").and_then(Value::as_str) {
        Some(id) => id.to_owned(),
        None => {
            let error = DriverError::Protocol(format!(
                "{session_method} response did not contain thread.id"
            ));
            return Err(finish_driver_error(process, error, DriverPhase::Protocol).await);
        }
    };
    events.emit(TurnEvent::SessionStarted {
        session_id: session_id.clone(),
    });

    let mut turn_params = json!({
        "threadId": session_id,
        "input": [{"type": "text", "text": request.input, "text_elements": []}],
        "responsesapiClientMetadata": {
            "odori_run_id": request.identity.run_id,
            "odori_turn": request.identity.turn.to_string(),
            "odori_attempt": request.identity.attempt.to_string()
        }
    });
    if let Some(schema) = request.directives.output_schema {
        turn_params["outputSchema"] = schema;
    }
    let turn_result = match process.request("turn/start", turn_params, &events).await {
        Ok(result) => result,
        Err(error) => return Err(finish_driver_error(process, error, DriverPhase::Turn).await),
    };
    let turn_id = match turn_result.pointer("/turn/id").and_then(Value::as_str) {
        Some(id) => id.to_owned(),
        None => {
            let error = DriverError::Protocol("turn/start response did not contain turn.id".into());
            return Err(finish_driver_error(process, error, DriverPhase::Protocol).await);
        }
    };

    let mut final_text = None;
    let mut usage = TurnUsage::default();
    loop {
        let message = match process.next_message().await {
            Ok(message) => message,
            Err(error) => {
                return Err(finish_driver_error(process, error, DriverPhase::Turn).await);
            }
        };
        events.emit(TurnEvent::Liveness);
        if let Err(error) = process.reject_server_request(&message).await {
            return Err(finish_driver_error(process, error, DriverPhase::Protocol).await);
        }

        let method = message.get("method").and_then(Value::as_str);
        if method == Some("item/started")
            && let Some(name) = tool_name(&message)
        {
            events.emit(TurnEvent::ToolUse { name });
        }
        if method == Some("item/completed")
            && message.pointer("/params/item/type").and_then(Value::as_str) == Some("agentMessage")
        {
            let phase = message
                .pointer("/params/item/phase")
                .and_then(Value::as_str);
            if phase != Some("commentary")
                && let Some(text) = message.pointer("/params/item/text").and_then(Value::as_str)
            {
                final_text = Some(text.to_owned());
            }
        }
        if method == Some("thread/tokenUsage/updated")
            && message.pointer("/params/turnId").and_then(Value::as_str) == Some(&turn_id)
        {
            usage.input_tokens = message
                .pointer("/params/tokenUsage/last/inputTokens")
                .and_then(Value::as_u64);
            usage.output_tokens = message
                .pointer("/params/tokenUsage/last/outputTokens")
                .and_then(Value::as_u64);
        }
        if method == Some("turn/completed")
            && message.pointer("/params/turn/id").and_then(Value::as_str) == Some(&turn_id)
        {
            usage.duration = message
                .pointer("/params/turn/durationMs")
                .and_then(Value::as_u64)
                .map(Duration::from_millis);
            let status = message
                .pointer("/params/turn/status")
                .and_then(Value::as_str)
                .unwrap_or("failed");
            if status == "completed" {
                let mut outcome = TurnOutcome::new(session_id, final_text.unwrap_or_default());
                outcome.usage = usage;
                let (exit, stderr) = process.finish(false).await;
                if !exit.as_ref().is_some_and(ExitStatus::success) {
                    tracing::warn!(
                        exit_code = ?exit.and_then(|status| status.code()),
                        stderr_head = stderr_head(&stderr),
                        "Codex app-server exited non-zero after terminal turn completion"
                    );
                }
                return Ok(outcome);
            }
            let terminal_error = message.pointer("/params/turn/error").cloned();
            let error = classify_terminal_error(status, terminal_error.as_ref());
            let _ = process.finish(false).await;
            return Err(error);
        }
    }
}

#[derive(Debug, Default)]
struct RenderedTooling {
    config: Map<String, Value>,
    env: Vec<(String, String)>,
}

fn render_tooling(tooling: &TurnTooling) -> Result<RenderedTooling, TurnError> {
    let mut rendered = RenderedTooling::default();
    let mut names = HashSet::new();
    for (server_index, server) in tooling.mcp_servers.iter().enumerate() {
        if !valid_server_name(&server.name) {
            return Err(TurnError::Config {
                message: format!(
                    "Codex MCP server name {:?} must contain only ASCII letters, digits, `_`, or `-`",
                    server.name
                ),
            });
        }
        if !names.insert(server.name.as_str()) {
            return Err(TurnError::Config {
                message: format!("duplicate Codex MCP server name {:?}", server.name),
            });
        }
        let base = format!("mcp_servers.{}", server.name);
        rendered
            .config
            .insert(format!("{base}.required"), Value::Bool(true));
        // Framework authorization and durability live behind the bridge;
        // app-server has no interactive approval channel in a provider turn.
        rendered.config.insert(
            format!("{base}.default_tools_approval_mode"),
            Value::String("approve".to_owned()),
        );
        match &server.transport {
            McpTransport::Http { url, headers } => {
                rendered
                    .config
                    .insert(format!("{base}.url"), Value::String(url.clone()));
                let mut env_headers = Map::new();
                for (header_index, (name, value)) in headers.iter().enumerate() {
                    if name.eq_ignore_ascii_case("authorization")
                        && let Some(token) = value.strip_prefix("Bearer ")
                    {
                        let variable = format!("ODORI_CODEX_MCP_BEARER_{server_index}");
                        rendered.env.push((variable.clone(), token.to_owned()));
                        rendered.config.insert(
                            format!("{base}.bearer_token_env_var"),
                            Value::String(variable),
                        );
                    } else {
                        let variable =
                            format!("ODORI_CODEX_MCP_HEADER_{server_index}_{header_index}");
                        rendered.env.push((variable.clone(), value.clone()));
                        env_headers.insert(name.clone(), Value::String(variable));
                    }
                }
                if !env_headers.is_empty() {
                    rendered.config.insert(
                        format!("{base}.env_http_headers"),
                        Value::Object(env_headers),
                    );
                }
            }
            McpTransport::Stdio { command, args, env } => {
                rendered
                    .config
                    .insert(format!("{base}.command"), Value::String(command.clone()));
                rendered.config.insert(
                    format!("{base}.args"),
                    Value::Array(args.iter().cloned().map(Value::String).collect()),
                );
                rendered.config.insert(
                    format!("{base}.env"),
                    Value::Object(
                        env.iter()
                            .map(|(name, value)| (name.clone(), Value::String(value.clone())))
                            .collect(),
                    ),
                );
            }
        }
        if let Some(timeout) = tooling.mcp_timeout {
            rendered.config.insert(
                format!("{base}.tool_timeout_sec"),
                json!(timeout.as_secs_f64()),
            );
        }
        if let Some(allowed) = &tooling.allowed_native_tools {
            let prefix = format!("mcp__{}__", server.name);
            let enabled: Vec<Value> = allowed
                .iter()
                .filter_map(|name| name.strip_prefix(&prefix))
                .map(|name| Value::String(name.to_owned()))
                .collect();
            rendered
                .config
                .insert(format!("{base}.enabled_tools"), Value::Array(enabled));
        }
    }
    Ok(rendered)
}

fn developer_instructions(request: &TurnRequest) -> String {
    let mut instructions = request.directives.instructions.clone();
    if let Some(allowed) = &request.tooling.allowed_native_tools {
        instructions
            .push_str("\n\nOdori tool boundary: use only these named tools for this turn: ");
        if allowed.is_empty() {
            instructions.push_str("none. Do not invoke native or MCP tools.");
        } else {
            instructions.push_str(&allowed.join(", "));
            instructions.push('.');
        }
    }
    instructions
}

fn valid_server_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn tool_name(message: &Value) -> Option<String> {
    let item = message.pointer("/params/item")?;
    match item.get("type").and_then(Value::as_str)? {
        "mcpToolCall" => Some(format!(
            "mcp__{}__{}",
            item.get("server").and_then(Value::as_str)?,
            item.get("tool").and_then(Value::as_str)?
        )),
        "dynamicToolCall" => item.get("tool").and_then(Value::as_str).map(str::to_owned),
        "commandExecution" => Some("commandExecution".to_owned()),
        "fileChange" => Some("fileChange".to_owned()),
        "webSearch" => Some("webSearch".to_owned()),
        "imageView" => Some("imageView".to_owned()),
        _ => None,
    }
}

fn classify_terminal_error(status: &str, error: Option<&Value>) -> TurnError {
    let message = error
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .unwrap_or(status)
        .to_owned();
    let info = error.and_then(|error| error.get("codexErrorInfo"));
    let info_name = info.and_then(Value::as_str).or_else(|| {
        info.and_then(Value::as_object)
            .and_then(|object| object.keys().next().map(String::as_str))
    });
    match info_name {
        Some("unauthorized") => auth_error(&message),
        Some("usageLimitExceeded" | "sessionBudgetExceeded" | "serverOverloaded") => {
            TurnError::Api { message }
        }
        Some(
            "httpConnectionFailed"
            | "responseStreamConnectionFailed"
            | "responseStreamDisconnected"
            | "responseTooManyFailedAttempts"
            | "internalServerError",
        ) => TurnError::Api { message },
        Some("sandboxError") => TurnError::Tooling { message },
        Some("contextWindowExceeded" | "cyberPolicy" | "badRequest" | "threadRollbackFailed") => {
            TurnError::Config { message }
        }
        _ if looks_like_auth_error(&message) => auth_error(&message),
        _ if looks_like_rate_limit(&message) => TurnError::Api { message },
        _ => TurnError::Api { message },
    }
}

fn looks_like_auth_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("401 unauthorized")
        || lower.contains("missing bearer")
        || lower.contains("not logged in")
        || lower.contains("authentication")
}

fn looks_like_rate_limit(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("429")
        || lower.contains("rate limit")
        || lower.contains("usage limit")
        || lower.contains("usage cap")
        || lower.contains("quota")
}

fn auth_error(detail: &str) -> TurnError {
    TurnError::Config {
        message: format!(
            "Codex is not authenticated for subscription use; run `codex login` and verify with `codex login status` ({detail})"
        ),
    }
}

fn command_with_hygiene(command: &Path) -> Command {
    let mut process = Command::new(command);
    // Subscription auth must come from CODEX_HOME's credential store, not an
    // API key or endpoint accidentally inherited from the parent agent.
    for variable in [
        "OPENAI_API_KEY",
        "OPENAI_BASE_URL",
        "OPENAI_API_BASE",
        "OPENAI_ORG_ID",
        "OPENAI_PROJECT_ID",
        "CODEX_API_KEY",
    ] {
        process.env_remove(variable);
    }
    process
}

fn command_error(command: &Path, action: &str, error: io::Error) -> TurnError {
    let detail = if error.kind() == io::ErrorKind::NotFound {
        format!(
            "Codex CLI was not found at `{}`; install Codex CLI {EXPECTED_CODEX_CLI_VERSION}, ensure it is on PATH, then run `codex login`",
            command.display()
        )
    } else {
        format!("could not {action} using `{}`: {error}", command.display())
    };
    TurnError::Config { message: detail }
}

#[derive(Debug)]
enum DriverError {
    Io(io::Error),
    Json(serde_json::Error),
    Rpc(Value),
    Protocol(String),
    Eof,
}

impl From<io::Error> for DriverError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for DriverError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

#[derive(Debug)]
enum DriverPhase {
    Initialize,
    Start,
    Resume(String),
    Turn,
    Protocol,
}

struct AppServerProcess {
    child: Child,
    input: Option<ChildStdin>,
    lines: Lines<BufReader<ChildStdout>>,
    stderr_task: tokio::task::JoinHandle<String>,
    pending: VecDeque<Value>,
    next_id: u64,
}

impl AppServerProcess {
    async fn spawn(command: &Path, env: &[(String, String)]) -> Result<Self, TurnError> {
        let mut command_line = command_with_hygiene(command);
        command_line
            .args(["app-server", "--listen", "stdio://", "--strict-config"])
            .envs(env.iter().cloned())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command_line
            .spawn()
            .map_err(|error| command_error(command, "start app-server", error))?;
        let input = child.stdin.take().ok_or_else(|| TurnError::Config {
            message: "Codex app-server stdin was not piped".into(),
        })?;
        let stdout = child.stdout.take().ok_or_else(|| TurnError::Config {
            message: "Codex app-server stdout was not piped".into(),
        })?;
        let mut stderr = child.stderr.take().ok_or_else(|| TurnError::Config {
            message: "Codex app-server stderr was not piped".into(),
        })?;
        let stderr_task = tokio::spawn(async move {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text).await;
            text
        });
        Ok(Self {
            child,
            input: Some(input),
            lines: BufReader::new(stdout).lines(),
            stderr_task,
            pending: VecDeque::new(),
            next_id: 1,
        })
    }

    async fn send(&mut self, value: &Value) -> Result<(), DriverError> {
        let mut bytes = serde_json::to_vec(value)?;
        bytes.push(b'\n');
        let input = self
            .input
            .as_mut()
            .ok_or_else(|| DriverError::Protocol("app-server stdin is already closed".into()))?;
        input.write_all(&bytes).await?;
        input.flush().await?;
        Ok(())
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<(), DriverError> {
        let notification = if params.is_null() {
            json!({"method": method})
        } else {
            json!({"method": method, "params": params})
        };
        self.send(&notification).await
    }

    async fn request(
        &mut self,
        method: &str,
        params: Value,
        events: &TurnEventSink,
    ) -> Result<Value, DriverError> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"method": method, "id": id, "params": params}))
            .await?;
        loop {
            // Read the wire directly while waiting for this response. A
            // notification may precede it; retain that message so the turn
            // loop observes the complete event stream in arrival order.
            let message = self.read_wire_message().await?;
            events.emit(TurnEvent::Liveness);
            if message.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(error) = message.get("error") {
                    return Err(DriverError::Rpc(error.clone()));
                }
                return message.get("result").cloned().ok_or_else(|| {
                    DriverError::Protocol(format!("{method} response had no result"))
                });
            }
            if message.get("method").is_some() && message.get("id").is_some() {
                self.reject_server_request(&message).await?;
            } else {
                self.pending.push_back(message);
            }
        }
    }

    async fn next_message(&mut self) -> Result<Value, DriverError> {
        if let Some(message) = self.pending.pop_front() {
            return Ok(message);
        }
        self.read_wire_message().await
    }

    async fn read_wire_message(&mut self) -> Result<Value, DriverError> {
        let line = self.lines.next_line().await?.ok_or(DriverError::Eof)?;
        Ok(serde_json::from_str(&line)?)
    }

    async fn reject_server_request(&mut self, message: &Value) -> Result<(), DriverError> {
        if message.get("method").is_some()
            && let Some(id) = message.get("id")
        {
            self.send(&json!({
                "id": id,
                "error": {
                    "code": -32601,
                    "message": "Odori's non-interactive provider does not support this server request"
                }
            }))
            .await?;
        }
        Ok(())
    }

    async fn finish(mut self, force_kill: bool) -> (Option<ExitStatus>, String) {
        self.input.take();
        let status = if force_kill {
            self.child.start_kill().ok();
            self.child.wait().await.ok()
        } else {
            match tokio::time::timeout(APP_SERVER_SHUTDOWN_GRACE, self.child.wait()).await {
                Ok(status) => status.ok(),
                Err(_) => {
                    self.child.start_kill().ok();
                    self.child.wait().await.ok()
                }
            }
        };
        let stderr = self.stderr_task.await.unwrap_or_default();
        (status, stderr)
    }
}

async fn finish_driver_error(
    process: AppServerProcess,
    error: DriverError,
    phase: DriverPhase,
) -> TurnError {
    let (status, stderr) = process.finish(true).await;
    match error {
        DriverError::Rpc(error) => classify_rpc_error(error, phase),
        DriverError::Protocol(message) => TurnError::Config {
            message: format!("Codex app-server protocol drift: {message}"),
        },
        DriverError::Json(error) => TurnError::Config {
            message: format!("Codex app-server emitted invalid JSON: {error}"),
        },
        DriverError::Io(error) if error.kind() == io::ErrorKind::BrokenPipe => {
            TurnError::HarnessDied {
                exit_code: status.and_then(|status| status.code()),
                stderr_head: stderr_head(&stderr).to_owned(),
            }
        }
        DriverError::Io(error) => TurnError::HarnessDied {
            exit_code: status.and_then(|status| status.code()),
            stderr_head: format!("{}: {error}", stderr_head(&stderr)),
        },
        DriverError::Eof => TurnError::HarnessDied {
            exit_code: status.and_then(|status| status.code()),
            stderr_head: stderr_head(&stderr).to_owned(),
        },
    }
}

fn classify_rpc_error(error: Value, phase: DriverPhase) -> TurnError {
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("unknown JSON-RPC error")
        .to_owned();
    match phase {
        DriverPhase::Resume(session_id)
            if message.contains("no rollout found") || message.contains("not found") =>
        {
            TurnError::SessionNotFound { session_id }
        }
        _ if message.contains("required MCP servers failed") || message.contains("MCP server") => {
            TurnError::Tooling { message }
        }
        _ if looks_like_auth_error(&message) => auth_error(&message),
        _ => TurnError::Config { message },
    }
}

fn stderr_head(stderr: &str) -> &str {
    stderr.lines().next().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use odori_agents::provider::{McpServerConfig, McpTransport};

    use super::*;

    #[test]
    fn tooling_renders_http_bearer_without_putting_token_in_config() {
        let mut tooling = TurnTooling::default();
        tooling.mcp_servers.push(McpServerConfig {
            name: "odori".into(),
            transport: McpTransport::Http {
                url: "http://127.0.0.1:1234/mcp".into(),
                headers: vec![("Authorization".into(), "Bearer secret".into())],
            },
        });
        tooling.allowed_native_tools = Some(vec!["mcp__odori__deploy".into()]);
        tooling.mcp_timeout = Some(Duration::from_secs(120));

        let rendered = render_tooling(&tooling).expect("valid tooling");
        assert_eq!(
            rendered.config.get("mcp_servers.odori.url"),
            Some(&json!("http://127.0.0.1:1234/mcp"))
        );
        assert_eq!(
            rendered
                .config
                .get("mcp_servers.odori.bearer_token_env_var"),
            Some(&json!("ODORI_CODEX_MCP_BEARER_0"))
        );
        assert_eq!(
            rendered.config.get("mcp_servers.odori.enabled_tools"),
            Some(&json!(["deploy"]))
        );
        assert_eq!(
            rendered
                .config
                .get("mcp_servers.odori.default_tools_approval_mode"),
            Some(&json!("approve"))
        );
        assert_eq!(
            rendered.config.get("mcp_servers.odori.tool_timeout_sec"),
            Some(&json!(120.0))
        );
        assert!(
            !Value::Object(rendered.config)
                .to_string()
                .contains("secret")
        );
        assert_eq!(
            rendered.env,
            vec![("ODORI_CODEX_MCP_BEARER_0".into(), "secret".into())]
        );
    }

    #[test]
    fn terminal_usage_cap_and_429_are_retryable_api_errors() {
        for error in [
            json!({"message": "usage exhausted", "codexErrorInfo": "usageLimitExceeded"}),
            json!({"message": "unexpected 429 rate limit", "codexErrorInfo": "other"}),
        ] {
            let classified = classify_terminal_error("failed", Some(&error));
            assert!(matches!(classified, TurnError::Api { .. }));
            assert!(classified.is_retryable());
        }
    }

    #[test]
    fn terminal_unauthorized_is_actionable_configuration_error() {
        let error = json!({
            "message": "unexpected status 401 Unauthorized: Missing bearer authentication",
            "codexErrorInfo": "other"
        });
        let classified = classify_terminal_error("failed", Some(&error));
        assert!(matches!(classified, TurnError::Config { .. }));
        assert!(classified.to_string().contains("codex login"));
    }

    #[test]
    fn missing_cli_is_an_actionable_configuration_error() {
        let classified = command_error(
            &PathBuf::from("missing-codex"),
            "inspect its version",
            io::Error::from(io::ErrorKind::NotFound),
        );
        assert!(matches!(classified, TurnError::Config { .. }));
        let message = classified.to_string();
        assert!(message.contains("was not found"));
        assert!(message.contains("codex login"));
        assert!(message.contains(EXPECTED_CODEX_CLI_VERSION));
    }
}
