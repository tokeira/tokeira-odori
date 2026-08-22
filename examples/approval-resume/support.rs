//! Two-process, approval-gated recovery over an embedded-engine disk snapshot.
#![allow(dead_code)] // The CLI and integration test consume different report surfaces.

use std::{
    fs,
    path::{Component, Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::{Context as _, Result, ensure};
use async_trait::async_trait;
use odori::{Agent, AgentRegistry, Conversation, Providers, RunEnd, Tool, ToolFailure, TurnRecord};
use odori_agents::provider::{
    McpTransport, Provider, SessionDirective, TurnError, TurnEvent, TurnEventSink, TurnOutcome,
    TurnRequest, TurnTooling,
};
use odori_engine::{ConnectTarget, OdoriRuntime};
use odori_mcp_bridge::BridgeConfig;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokeira_engine::{Engine, SnapshotPolicyConfig, TokeiraConfig};

pub const PLAN_HASH: &str = "plan-v1-fix-increment";

const RUN_ID: &str = "approval-resume-run";
const TASK_QUEUE: &str = "example-approval-resume";
const SESSION_ID: &str = "approval-resume-session";
const FORKED_SESSION_ID: &str = "approval-resume-approved-session";
const APPROVAL_REQUEST: &str = "approval-request.json";
const SNAPSHOT_FILE: &str = "engine.snapshot";
const WORKSPACE: &str = "workspace";
const ALLOWED_PATH: &str = "src/lib.rs";

const FIXTURE_MANIFEST: &str = include_str!("fixture/Cargo.toml");
const FIXTURE_LOCK: &str = include_str!("fixture/Cargo.lock");
const BROKEN_LIB: &str = include_str!("fixture/src/lib.rs");
const FIXED_LIB: &str = include_str!("fixture/fixed/lib.rs");

/// The durable first-turn result presented to the human approval seat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchProposal {
    pub summary: String,
    pub plan_hash: String,
    pub file_scope: Vec<String>,
    pub before: String,
    pub after: String,
    pub finish_bar: Vec<String>,
}

/// The decision recorded as the restored workflow's second-turn input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ApprovalDecision {
    decision: String,
    plan_hash: String,
}

/// The terminal result produced only after the approved patch and finish bar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalCompletion {
    pub plan_hash: String,
    pub applied: String,
    pub finish_bar: Vec<String>,
    pub session_forked: bool,
}

/// Evidence returned after process one has reached the approval boundary.
#[derive(Debug)]
pub struct PrepareReport {
    pub proposal: PatchProposal,
    pub snapshot_bytes: u64,
}

/// Evidence returned after process two restores and completes the run.
#[derive(Debug)]
pub struct ResumeReport {
    pub completion: ApprovalCompletion,
    pub turns_before_approval: usize,
    pub turns_after_completion: usize,
    pub apply_executions: u64,
    pub finish_bar_executions: u64,
}

#[derive(Debug)]
struct ApprovalState {
    workspace: PathBuf,
    approved_hash: Option<String>,
    apply_executions: AtomicU64,
    finish_bar_executions: AtomicU64,
}

#[derive(Debug, Clone)]
struct ApprovalProvider;

#[async_trait]
impl Provider for ApprovalProvider {
    fn name(&self) -> &str {
        "approval-scripted"
    }

    async fn execute_turn(
        &self,
        request: TurnRequest,
        events: TurnEventSink,
    ) -> Result<TurnOutcome, TurnError> {
        let (session_id, session_forked) = match (&request.session, request.identity.turn) {
            (SessionDirective::Start, 0) => (SESSION_ID, false),
            (SessionDirective::ResumeForked { session_id }, 1) if session_id == SESSION_ID => {
                (FORKED_SESSION_ID, true)
            }
            (directive, turn) => {
                return Err(tooling(format!(
                    "turn {turn} received unexpected session directive {directive:?}"
                )));
            }
        };
        events.emit(TurnEvent::SessionStarted {
            session_id: session_id.to_owned(),
        });
        events.emit(TurnEvent::Liveness);

        if request.identity.turn == 0 {
            return Ok(TurnOutcome::new(
                session_id,
                serde_json::to_string(&proposal()).map_err(tooling)?,
            ));
        }

        let decision: ApprovalDecision = serde_json::from_str(&request.input).map_err(tooling)?;
        if decision.decision != "approve" || decision.plan_hash != PLAN_HASH {
            return Err(tooling(
                "the restored turn did not carry the approved plan hash",
            ));
        }
        let apply = call_tool(
            &request.tooling,
            "apply_approved_patch",
            json!({
                "plan_hash": decision.plan_hash,
                "path": ALLOWED_PATH,
                "content": FIXED_LIB,
            }),
            "approval-resume-apply",
        )
        .await?;
        tool_success_text(&apply)?;
        let finish_bar = call_tool(
            &request.tooling,
            "finish_bar",
            json!({"plan_hash": PLAN_HASH}),
            "approval-resume-finish-bar",
        )
        .await?;
        tool_success_text(&finish_bar)?;

        Ok(TurnOutcome::new(
            session_id,
            serde_json::to_string(&ApprovalCompletion {
                plan_hash: PLAN_HASH.to_owned(),
                applied: ALLOWED_PATH.to_owned(),
                finish_bar: vec!["cargo test --locked".to_owned()],
                session_forked,
            })
            .map_err(tooling)?,
        ))
    }
}

fn proposal() -> PatchProposal {
    PatchProposal {
        summary: "Fix increment so the bundled regression test passes.".to_owned(),
        plan_hash: PLAN_HASH.to_owned(),
        file_scope: vec![ALLOWED_PATH.to_owned()],
        before: BROKEN_LIB.to_owned(),
        after: FIXED_LIB.to_owned(),
        finish_bar: vec!["cargo test --locked".to_owned()],
    }
}

fn registry(state: Arc<ApprovalState>) -> AgentRegistry {
    let mut registry = AgentRegistry::new();
    registry.register(
        Agent::new(
            "approval-worker",
            "Propose the bounded patch, wait for the human decision, then apply only the approved bytes and prove the finish bar.",
        )
        .with_provider("approval-scripted")
        .with_tool(apply_tool(state.clone()))
        .with_tool(finish_bar_tool(state)),
    );
    registry
}

fn apply_tool(state: Arc<ApprovalState>) -> Tool {
    Tool::new(
        "apply_approved_patch",
        "Apply exactly the human-approved patch inside its declared file scope.",
        json!({
            "type": "object",
            "properties": {
                "plan_hash": {"type": "string"},
                "path": {"type": "string"},
                "content": {"type": "string"}
            },
            "required": ["plan_hash", "path", "content"],
            "additionalProperties": false
        }),
        move |_context, arguments| {
            let state = state.clone();
            async move {
                let plan_hash = string_argument(&arguments, "plan_hash")?;
                if state.approved_hash.as_deref() != Some(plan_hash) {
                    return Err(ToolFailure::terminal(format!(
                        "approval gate: plan {plan_hash:?} was not approved by this process's human input"
                    )));
                }
                let path = string_argument(&arguments, "path")?;
                let relative = Path::new(path);
                let safe = !relative.is_absolute()
                    && relative.components().all(|component| {
                        matches!(component, Component::Normal(_) | Component::CurDir)
                    });
                if !safe || path != ALLOWED_PATH {
                    return Err(ToolFailure::terminal(format!(
                        "scope fence: {path:?} is outside declared path {ALLOWED_PATH:?}"
                    )));
                }
                let content = string_argument(&arguments, "content")?;
                if content != FIXED_LIB {
                    return Err(ToolFailure::terminal(
                        "approval gate: proposed bytes differ from the reviewed patch",
                    ));
                }
                let destination = state.workspace.join(relative);
                fs::write(&destination, content).map_err(|error| {
                    ToolFailure::terminal(format!("write {}: {error}", destination.display()))
                })?;
                let execution = state.apply_executions.fetch_add(1, Ordering::SeqCst) + 1;
                Ok(json!({
                    "plan_hash": plan_hash,
                    "written": path,
                    "execution": execution,
                }))
            }
        },
    )
}

fn finish_bar_tool(state: Arc<ApprovalState>) -> Tool {
    Tool::new(
        "finish_bar",
        "Run the approved patch's declared test command after the scoped write.",
        json!({
            "type": "object",
            "properties": {"plan_hash": {"type": "string"}},
            "required": ["plan_hash"],
            "additionalProperties": false
        }),
        move |_context, arguments| {
            let state = state.clone();
            async move {
                let plan_hash = string_argument(&arguments, "plan_hash")?;
                if state.approved_hash.as_deref() != Some(plan_hash) {
                    return Err(ToolFailure::terminal(
                        "approval gate: finish bar has no matching human approval",
                    ));
                }
                let source =
                    fs::read_to_string(state.workspace.join(ALLOWED_PATH)).map_err(|error| {
                        ToolFailure::terminal(format!("read patched source: {error}"))
                    })?;
                if source != FIXED_LIB {
                    return Err(ToolFailure::terminal(
                        "finish bar refused because the reviewed patch is not present",
                    ));
                }
                let workspace = state.workspace.clone();
                let output = tokio::task::spawn_blocking(move || {
                    Command::new("cargo")
                        .args(["test", "--locked"])
                        .current_dir(workspace)
                        .output()
                })
                .await
                .map_err(|error| ToolFailure::terminal(format!("join finish bar: {error}")))?
                .map_err(|error| ToolFailure::terminal(format!("run finish bar: {error}")))?;
                if !output.status.success() {
                    return Err(ToolFailure::terminal(format!(
                        "finish bar failed: {}",
                        String::from_utf8_lossy(&output.stderr)
                    )));
                }
                let execution = state.finish_bar_executions.fetch_add(1, Ordering::SeqCst) + 1;
                Ok(json!({
                    "green": true,
                    "command": "cargo test --locked",
                    "execution": execution,
                }))
            }
        },
    )
}

fn string_argument<'a>(arguments: &'a Value, name: &str) -> Result<&'a str, ToolFailure> {
    arguments
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| ToolFailure::terminal(format!("{name} must be a string")))
}

fn tooling(error: impl std::fmt::Display) -> TurnError {
    TurnError::Tooling {
        message: error.to_string(),
    }
}

fn endpoint(tooling_config: &TurnTooling) -> Result<(String, String), TurnError> {
    let server = tooling_config
        .mcp_servers
        .first()
        .ok_or_else(|| tooling("the durable bridge was not attached"))?;
    let McpTransport::Http { url, headers } = &server.transport else {
        return Err(tooling("the example requires the HTTP bridge"));
    };
    let authorization = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
        .map(|(_, value)| value.clone())
        .ok_or_else(|| tooling("bridge attachment omitted authorization"))?;
    Ok((url.clone(), authorization))
}

async fn call_tool(
    tooling_config: &TurnTooling,
    name: &str,
    arguments: Value,
    call_id: &str,
) -> Result<Value, TurnError> {
    let (url, authorization) = endpoint(tooling_config)?;
    let body = reqwest::Client::new()
        .post(url)
        .header("Authorization", authorization)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": call_id,
            "method": "tools/call",
            "params": {
                "name": name,
                "arguments": arguments,
                "_meta": {"odori/callId": call_id},
            },
        }))
        .send()
        .await
        .map_err(tooling)?
        .text()
        .await
        .map_err(tooling)?;
    let frame = body
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .next_back()
        .ok_or_else(|| tooling("bridge returned no final SSE frame"))?;
    serde_json::from_str(frame).map_err(tooling)
}

fn tool_success_text(frame: &Value) -> Result<String, TurnError> {
    if frame
        .pointer("/result/isError")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(tooling(format!("durable tool refused the call: {frame}")));
    }
    if let Some(error) = frame.pointer("/error/message").and_then(Value::as_str) {
        return Err(tooling(error));
    }
    frame
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| tooling(format!("tool response has no text result: {frame}")))
}

fn engine_config(snapshot: &Path) -> TokeiraConfig {
    let mut config = TokeiraConfig::default();
    config.policy.snapshot = Some(SnapshotPolicyConfig {
        location: snapshot.to_path_buf(),
        interval_ms: 3_600_000,
    });
    config
}

async fn runtime(engine: &Engine, state: Arc<ApprovalState>) -> Result<OdoriRuntime> {
    OdoriRuntime::builder(TASK_QUEUE)
        .connect(ConnectTarget::service_override(engine.service_override()))
        .agents(registry(state))
        .providers(Providers::new(Arc::new(ApprovalProvider)))
        .bridge(BridgeConfig::default())
        .start()
        .await
}

async fn wait_for_transcript(
    conversation: &Conversation,
    minimum_turns: usize,
) -> Result<Vec<TurnRecord>> {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let transcript = conversation.transcript().await?;
            if transcript.len() >= minimum_turns {
                return Ok(transcript);
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .with_context(|| format!("workflow did not record {minimum_turns} turn(s)"))?
}

fn seed_workspace(state_directory: &Path) -> Result<PathBuf> {
    ensure!(
        !state_directory.exists(),
        "state directory {} already exists; choose a new path",
        state_directory.display()
    );
    let workspace = state_directory.join(WORKSPACE);
    fs::create_dir_all(workspace.join("src"))?;
    fs::write(workspace.join("Cargo.toml"), FIXTURE_MANIFEST)?;
    fs::write(workspace.join("Cargo.lock"), FIXTURE_LOCK)?;
    fs::write(workspace.join(ALLOWED_PATH), BROKEN_LIB)?;
    Ok(workspace)
}

fn fixture_test_succeeds(workspace: &Path) -> Result<bool> {
    let output = Command::new("cargo")
        .args(["test", "--locked"])
        .current_dir(workspace)
        .output()
        .context("run fixture test")?;
    Ok(output.status.success())
}

/// Record one typed proposal, persist the live workflow to disk, and exit.
pub async fn prepare(state_directory: &Path, print: bool) -> Result<PrepareReport> {
    let workspace = seed_workspace(state_directory)?;
    ensure!(
        !fixture_test_succeeds(&workspace)?,
        "the bundled fixture must start with a failing test"
    );
    let snapshot = state_directory.join(SNAPSHOT_FILE);
    let state = Arc::new(ApprovalState {
        workspace,
        approved_hash: None,
        apply_executions: AtomicU64::new(0),
        finish_bar_executions: AtomicU64::new(0),
    });
    let engine = Engine::start_with_config(engine_config(&snapshot)).await?;
    let runtime = runtime(&engine, state.clone()).await?;
    let conversation = runtime
        .runner()
        .start_conversation(
            "approval-worker",
            "Diagnose the failing increment test and propose one bounded patch. Do not apply it before human approval.",
            RUN_ID,
        )
        .await?;
    let transcript = wait_for_transcript(&conversation, 1).await?;
    ensure!(
        transcript.len() == 1,
        "prepare must stop after the proposal turn"
    );
    let proposal: PatchProposal = serde_json::from_str(&transcript[0].text)?;
    ensure!(proposal == self::proposal());
    ensure!(state.apply_executions.load(Ordering::SeqCst) == 0);
    fs::write(
        state_directory.join(APPROVAL_REQUEST),
        serde_json::to_vec_pretty(&proposal)?,
    )?;
    drop(conversation);
    runtime.shutdown().await?;
    engine.shutdown().await?;
    let snapshot_bytes = fs::metadata(&snapshot)?.len();
    ensure!(snapshot_bytes > 0, "engine snapshot is empty");
    ensure!(
        !fixture_test_succeeds(&state.workspace)?,
        "the workspace changed before approval"
    );
    if print {
        println!("HUMAN APPROVAL REQUIRED: {}", proposal.plan_hash);
        println!(
            "REQUEST: {}",
            state_directory.join(APPROVAL_REQUEST).display()
        );
        println!(
            "SNAPSHOT WRITTEN: {} ({snapshot_bytes} bytes)",
            snapshot.display()
        );
        println!(
            "PROCESS {} EXITING WITH LIVE WORKFLOW {RUN_ID}",
            std::process::id()
        );
    }
    Ok(PrepareReport {
        proposal,
        snapshot_bytes,
    })
}

/// Restore the live workflow, record the human decision, and finish exactly once.
pub async fn resume(
    state_directory: &Path,
    approved_plan_hash: &str,
    print: bool,
) -> Result<ResumeReport> {
    let snapshot = state_directory.join(SNAPSHOT_FILE);
    ensure!(
        snapshot.is_file(),
        "missing snapshot {}",
        snapshot.display()
    );
    let proposal: PatchProposal =
        serde_json::from_slice(&fs::read(state_directory.join(APPROVAL_REQUEST))?)?;
    ensure!(
        approved_plan_hash == proposal.plan_hash,
        "approval hash {approved_plan_hash:?} does not match proposal {:?}",
        proposal.plan_hash
    );
    let state = Arc::new(ApprovalState {
        workspace: state_directory.join(WORKSPACE),
        approved_hash: Some(approved_plan_hash.to_owned()),
        apply_executions: AtomicU64::new(0),
        finish_bar_executions: AtomicU64::new(0),
    });
    let engine = Engine::start_with_config(engine_config(&snapshot)).await?;
    let runtime = runtime(&engine, state.clone()).await?;
    let conversation = runtime.runner().resume_conversation(RUN_ID);
    let transcript_before = wait_for_transcript(&conversation, 1).await?;
    ensure!(transcript_before.len() == 1);
    let restored_proposal: PatchProposal = serde_json::from_str(&transcript_before[0].text)?;
    ensure!(restored_proposal == proposal, "restored proposal changed");
    let decision = serde_json::to_string(&ApprovalDecision {
        decision: "approve".to_owned(),
        plan_hash: approved_plan_hash.to_owned(),
    })?;
    conversation.send(&decision).await?;
    let output = conversation.end().await?;
    ensure!(matches!(output.end, RunEnd::ConversationEnded));
    ensure!(
        output.turns == 2,
        "restored workflow should contain two turns"
    );
    let completion: ApprovalCompletion = serde_json::from_str(&output.text)?;
    ensure!(completion.plan_hash == approved_plan_hash);
    ensure!(
        completion.session_forked,
        "provider session lineage was not forked from the restored turn"
    );
    let completed = runtime.runner().resume_conversation(RUN_ID);
    let transcript_after = completed.transcript().await?;
    ensure!(transcript_after.len() == 2);
    ensure!(transcript_after[1].input == decision);
    let apply_executions = state.apply_executions.load(Ordering::SeqCst);
    let finish_bar_executions = state.finish_bar_executions.load(Ordering::SeqCst);
    ensure!(
        apply_executions == 1,
        "approved patch executed {apply_executions} times"
    );
    ensure!(
        finish_bar_executions == 1,
        "finish bar executed {finish_bar_executions} times"
    );
    ensure!(fs::read_to_string(state.workspace.join(ALLOWED_PATH))? == FIXED_LIB);
    ensure!(fixture_test_succeeds(&state.workspace)?);
    runtime.shutdown().await?;
    engine.shutdown().await?;
    if print {
        println!(
            "RESTORED: {} with one recorded proposal turn",
            snapshot.display()
        );
        println!("HUMAN APPROVAL RECORDED: {approved_plan_hash}");
        println!("APPLIED ONCE: {ALLOWED_PATH}");
        println!("GREEN: cargo test --locked");
        println!("PROCESS {} COMPLETED WORKFLOW {RUN_ID}", std::process::id());
    }
    Ok(ResumeReport {
        completion,
        turns_before_approval: transcript_before.len(),
        turns_after_completion: transcript_after.len(),
        apply_executions,
        finish_bar_executions,
    })
}

/// Verify the filesystem evidence left by a completed two-process run.
pub fn verify_completed_state(state_directory: &Path) -> Result<()> {
    let snapshot = state_directory.join(SNAPSHOT_FILE);
    ensure!(snapshot.is_file() && fs::metadata(snapshot)?.len() > 0);
    ensure!(fs::read_to_string(state_directory.join(WORKSPACE).join(ALLOWED_PATH))? == FIXED_LIB);
    ensure!(fixture_test_succeeds(&state_directory.join(WORKSPACE))?);
    Ok(())
}

/// Verify that process one left the proposal unapplied and recoverable.
pub fn verify_waiting_state(state_directory: &Path) -> Result<()> {
    let snapshot = state_directory.join(SNAPSHOT_FILE);
    ensure!(snapshot.is_file() && fs::metadata(snapshot)?.len() > 0);
    let proposal: PatchProposal =
        serde_json::from_slice(&fs::read(state_directory.join(APPROVAL_REQUEST))?)?;
    ensure!(proposal == self::proposal());
    let workspace = state_directory.join(WORKSPACE);
    ensure!(fs::read_to_string(workspace.join(ALLOWED_PATH))? == BROKEN_LIB);
    ensure!(!fixture_test_succeeds(&workspace)?);
    Ok(())
}
