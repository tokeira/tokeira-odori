//! Phase-1 probe for supervising `codex app-server` over newline-delimited
//! JSON-RPC. Findings, including the exact pinned CLI version, live in the
//! sibling README.

use std::{
    collections::BTreeMap,
    process::Stdio,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout, Command},
};

const TURN_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Default)]
struct TurnReport {
    thread_id: Option<String>,
    turn_id: Option<String>,
    final_text: Option<String>,
    turn_status: Option<String>,
    turn_error: Option<Value>,
    usage: Option<Value>,
    event_counts: BTreeMap<String, u32>,
    mcp_call_ids: Vec<String>,
    mcp_completions: Vec<Value>,
    first_event_ms: Option<u128>,
    total_ms: u128,
    exit_code: Option<i32>,
    stderr: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("turn") => {
            let prompt = args
                .get(1)
                .map(String::as_str)
                .unwrap_or("Reply with exactly the word: pirouette");
            let report = run_turn(Session::Start, prompt, None, false).await?;
            print_report("turn", &report);
        }
        Some("resume") => {
            let thread_id = args.get(1).context("resume needs a thread id")?;
            let prompt = args
                .get(2)
                .map(String::as_str)
                .unwrap_or("What word did you reply with before?");
            let report = run_turn(Session::Resume(thread_id), prompt, None, false).await?;
            print_report("resume", &report);
        }
        Some("fork") => {
            let thread_id = args.get(1).context("fork needs a thread id")?;
            let prompt = args
                .get(2)
                .map(String::as_str)
                .unwrap_or("What word did the source thread reply with?");
            let report = run_turn(Session::Fork(thread_id), prompt, None, false).await?;
            print_report("fork", &report);
        }
        Some("mcp") => {
            let url = args.get(1).context("mcp needs an HTTP endpoint URL")?;
            let token = args.get(2).context("mcp needs the expected bearer token")?;
            let config = mcp_config(url, token);
            let prompt = args.get(3).map(String::as_str).unwrap_or(
                "Call the odori_probe MCP tool exactly once, then reply with exactly its text result.",
            );
            let report = run_turn(Session::Start, prompt, Some(config), false).await?;
            print_report("mcp", &report);
        }
        Some("mcp-crash") => {
            let url = args
                .get(1)
                .context("mcp-crash needs an HTTP endpoint URL")?;
            let token = args
                .get(2)
                .context("mcp-crash needs the expected bearer token")?;
            let report = run_turn(
                Session::Start,
                "Call the odori_probe MCP tool exactly once, then reply with its result.",
                Some(mcp_config(url, token)),
                true,
            )
            .await?;
            print_report("mcp-crash", &report);
        }
        Some("mcp-resume") => {
            let thread_id = args.get(1).context("mcp-resume needs a thread id")?;
            let url = args
                .get(2)
                .context("mcp-resume needs an HTTP endpoint URL")?;
            let token = args
                .get(3)
                .context("mcp-resume needs the expected bearer token")?;
            let report = run_turn(
                Session::Resume(thread_id),
                "Retry the odori_probe MCP tool now, then reply with exactly its text result.",
                Some(mcp_config(url, token)),
                false,
            )
            .await?;
            print_report("mcp-resume", &report);
        }
        Some("missing") => {
            let missing = "00000000-0000-0000-0000-000000000000";
            let report = run_turn(Session::Resume(missing), "Reply with hi", None, false).await?;
            print_report("missing-session", &report);
        }
        Some("bad-config") => {
            let report = run_turn(
                Session::Start,
                "Reply with hi",
                Some(json!({"definitely_not_a_codex_key": true})),
                false,
            )
            .await?;
            print_report("bad-config", &report);
        }
        _ => bail!(
            "usage: codex-driver-spike <turn [prompt] | resume <thread> [prompt] | fork <thread> [prompt] | mcp <url> <token> [prompt] | mcp-crash <url> <token> | mcp-resume <thread> <url> <token> | missing | bad-config>"
        ),
    }
    Ok(())
}

fn mcp_config(url: &str, token: &str) -> Value {
    // App-server's per-thread `config` is the raw override map: keys use the
    // same dotted path syntax as repeated CLI `-c` flags.
    json!({
        "mcp_servers.odori_probe.url": url,
        "mcp_servers.odori_probe.http_headers": {
            "Authorization": format!("Bearer {token}")
        },
        "mcp_servers.odori_probe.required": true,
        "mcp_servers.odori_probe.tool_timeout_sec": 5,
        "mcp_servers.odori_probe.startup_timeout_sec": 5,
        "mcp_servers.odori_probe.enabled_tools": ["odori_probe"]
    })
}

enum Session<'a> {
    Start,
    Resume(&'a str),
    Fork(&'a str),
}

struct AppServer {
    child: Child,
    input: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    stderr_task: tokio::task::JoinHandle<String>,
    next_id: u64,
}

impl AppServer {
    async fn spawn() -> Result<Self> {
        let mut child = Command::new("codex")
            .args(["app-server", "--listen", "stdio://", "--strict-config"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("spawning `codex app-server` (is Codex installed and on PATH?)")?;
        let input = child
            .stdin
            .take()
            .context("app-server stdin was not piped")?;
        let stdout = child
            .stdout
            .take()
            .context("app-server stdout was not piped")?;
        let mut stderr = child
            .stderr
            .take()
            .context("app-server stderr was not piped")?;
        let stderr_task = tokio::spawn(async move {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text).await;
            text
        });
        Ok(Self {
            child,
            input,
            lines: BufReader::new(stdout).lines(),
            stderr_task,
            next_id: 1,
        })
    }

    async fn send(&mut self, value: &Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(value)?;
        bytes.push(b'\n');
        self.input.write_all(&bytes).await?;
        self.input.flush().await?;
        Ok(())
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"method": method, "id": id, "params": params}))
            .await?;
        loop {
            let message = self.next_message().await?;
            if message.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(error) = message.get("error") {
                    bail!("{method} failed: {error}");
                }
                return message
                    .get("result")
                    .cloned()
                    .with_context(|| format!("{method} response had no result: {message}"));
            }
            // Handshake requests do not normally race notifications. Log any
            // unexpected message so protocol drift remains visible.
            eprintln!("pre-response app-server message: {message}");
        }
    }

    async fn next_message(&mut self) -> Result<Value> {
        let line = self
            .lines
            .next_line()
            .await?
            .context("app-server stdout closed")?;
        serde_json::from_str(&line).with_context(|| format!("invalid app-server JSON: {line}"))
    }

    async fn initialize(&mut self) -> Result<()> {
        self.request(
            "initialize",
            json!({
                "clientInfo": {
                    "name": "odori_codex_driver_spike",
                    "title": "Odori Codex driver spike",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "capabilities": null
            }),
        )
        .await?;
        self.send(&json!({"method": "initialized"})).await
    }

    async fn shutdown(mut self) -> Result<(Option<i32>, String)> {
        drop(self.input);
        let status = match tokio::time::timeout(Duration::from_secs(2), self.child.wait()).await {
            Ok(status) => status?,
            Err(_) => {
                self.child.start_kill().ok();
                self.child.wait().await?
            }
        };
        let stderr = self.stderr_task.await.unwrap_or_default();
        Ok((status.code(), stderr))
    }

    async fn crash(mut self) -> Result<(Option<i32>, String)> {
        self.child.start_kill().ok();
        let status = self.child.wait().await?;
        let stderr = self.stderr_task.await.unwrap_or_default();
        Ok((status.code(), stderr))
    }
}

async fn run_turn(
    session: Session<'_>,
    prompt: &str,
    config: Option<Value>,
    crash_on_mcp: bool,
) -> Result<TurnReport> {
    let started = Instant::now();
    let mut server = AppServer::spawn().await?;
    server.initialize().await?;

    let mut params = json!({
        "cwd": std::env::current_dir()?,
        "approvalPolicy": "never",
        "sandbox": "read-only",
        "developerInstructions": "Complete only the user's bounded request. Do not use shell or file tools.",
        "ephemeral": false
    });
    if let Some(config) = config {
        params["config"] = config;
    }
    let (method, params) = match session {
        Session::Start => ("thread/start", params),
        Session::Resume(thread_id) => {
            params["threadId"] = json!(thread_id);
            ("thread/resume", params)
        }
        Session::Fork(thread_id) => {
            params["threadId"] = json!(thread_id);
            ("thread/fork", params)
        }
    };
    let session_result = match server.request(method, params).await {
        Ok(result) => result,
        Err(error) => {
            let (_, stderr) = server.shutdown().await?;
            bail!("{error:#}\napp-server stderr:\n{stderr}");
        }
    };
    let thread_id = session_result
        .pointer("/thread/id")
        .and_then(Value::as_str)
        .context("thread response did not carry thread.id")?
        .to_owned();

    let turn_result = server
        .request(
            "turn/start",
            json!({
                "threadId": thread_id,
                "input": [{"type": "text", "text": prompt, "text_elements": []}]
            }),
        )
        .await?;
    let turn_id = turn_result
        .pointer("/turn/id")
        .and_then(Value::as_str)
        .context("turn/start response did not carry turn.id")?
        .to_owned();

    let mut report = TurnReport {
        thread_id: Some(thread_id.clone()),
        turn_id: Some(turn_id.clone()),
        ..TurnReport::default()
    };
    let deadline = tokio::time::sleep(TURN_TIMEOUT);
    tokio::pin!(deadline);

    loop {
        let message = tokio::select! {
            message = server.next_message() => message?,
            _ = &mut deadline => {
                let _ = server.shutdown().await;
                bail!("turn exceeded {TURN_TIMEOUT:?}");
            }
        };
        report
            .first_event_ms
            .get_or_insert(started.elapsed().as_millis());
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("response");
        *report.event_counts.entry(method.to_owned()).or_default() += 1;

        if method == "item/agentMessage/delta"
            && message.pointer("/params/turnId").and_then(Value::as_str) == Some(&turn_id)
        {
            report.final_text.get_or_insert_with(String::new).push_str(
                message
                    .pointer("/params/delta")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            );
        }
        if method == "item/started"
            && message.pointer("/params/item/type").and_then(Value::as_str) == Some("mcpToolCall")
            && let Some(id) = message.pointer("/params/item/id").and_then(Value::as_str)
        {
            report.mcp_call_ids.push(id.to_owned());
            if crash_on_mcp {
                // Give the MCP request time to cross the loopback boundary,
                // then model abrupt app-server death while the call is in
                // flight. The persisted thread is resumed by `mcp-resume`.
                tokio::time::sleep(Duration::from_millis(500)).await;
                report.total_ms = started.elapsed().as_millis();
                let (exit_code, stderr) = server.crash().await?;
                report.exit_code = exit_code;
                report.stderr = stderr;
                return Ok(report);
            }
        }
        if method == "item/completed"
            && message.pointer("/params/item/type").and_then(Value::as_str) == Some("mcpToolCall")
        {
            report
                .mcp_completions
                .push(message["params"]["item"].clone());
        }
        if method == "thread/tokenUsage/updated" {
            report.usage = message.pointer("/params/tokenUsage/last").cloned();
        }
        if method == "turn/completed"
            && message.pointer("/params/turn/id").and_then(Value::as_str) == Some(&turn_id)
        {
            report.turn_status = message
                .pointer("/params/turn/status")
                .and_then(Value::as_str)
                .map(str::to_owned);
            report.turn_error = message.pointer("/params/turn/error").cloned();
            break;
        }

        // The spike does not permit commands, file changes, elicitation, or
        // dynamic tools. Deny any server request explicitly so a drifted run
        // fails instead of hanging.
        if message.get("id").is_some() && message.get("method").is_some() {
            let id = message["id"].clone();
            server
                .send(&json!({
                    "id": id,
                    "result": {"decision": "decline", "reason": "probe denies interactive requests"}
                }))
                .await?;
        }
    }

    report.total_ms = started.elapsed().as_millis();
    let (exit_code, stderr) = server.shutdown().await?;
    report.exit_code = exit_code;
    report.stderr = stderr;
    Ok(report)
}

fn print_report(label: &str, report: &TurnReport) {
    println!("== {label} ==");
    println!(
        "  process_exit={:?} turn_status={:?}",
        report.exit_code, report.turn_status
    );
    println!(
        "  thread={} turn={}",
        report.thread_id.as_deref().unwrap_or("<none>"),
        report.turn_id.as_deref().unwrap_or("<none>")
    );
    println!(
        "  first_event={:?}ms total={}ms",
        report.first_event_ms, report.total_ms
    );
    println!("  events={:?}", report.event_counts);
    println!("  mcp_call_ids={:?}", report.mcp_call_ids);
    println!("  mcp_completions={:?}", report.mcp_completions);
    println!("  usage={:?}", report.usage);
    println!("  result={:?}", report.final_text);
    println!("  error={:?}", report.turn_error);
    if !report.stderr.is_empty() {
        println!(
            "  stderr={}",
            report.stderr.lines().next().unwrap_or_default()
        );
    }
}
