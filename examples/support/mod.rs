//! Shared machinery for the executable examples.
#![allow(dead_code)] // Each binary selects one half of this shared example module.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    net::TcpListener,
    path::{Component, Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use anyhow::{Context as _, Result, anyhow, ensure};
use async_trait::async_trait;
use odori::{
    Agent, AgentRegistry, Providers, RunBudget, RunEnd, RunOutput, Tool, ToolFailure,
    agents::{
        Handoff,
        provider::{
            McpTransport, Provider, TurnError, TurnEvent, TurnEventSink, TurnOutcome, TurnRequest,
            TurnTooling, TurnUsage,
        },
    },
};
use odori_engine::{ConnectTarget, OdoriRuntime};
use odori_mcp_bridge::BridgeConfig;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use temporalio_client::{UntypedWorkflow, WorkflowDescribeOptions};
use tokeira_engine::{Engine, TokeiraConfig};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

const FIXTURE_MANIFEST: &str = include_str!("../slice-fleet/fixture/Cargo.toml");
const FIXTURE_LOCK: &str = include_str!("../slice-fleet/fixture/Cargo.lock");
const FIXTURE_LIB: &str = include_str!("../slice-fleet/fixture/src/lib.rs");
const FIXTURE_INCREMENT: &str = include_str!("../slice-fleet/fixture/src/increment.rs");
const FIXTURE_DOUBLE: &str = include_str!("../slice-fleet/fixture/src/double.rs");
const FIX_INCREMENT: &str = include_str!("../slice-fleet/fixture/fixes/increment.rs");
const FIX_DOUBLE: &str = include_str!("../slice-fleet/fixture/fixes/double.rs");

pub const PLAN_HASH: &str = "plan-v1-bugfix-feature-budget-contract";

/// A bounded unit of fleet work. The file scope is data, not prose: the
/// write activity enforces it before touching the fixture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Slice {
    pub id: String,
    pub task: String,
    pub provider: String,
    pub files: Vec<String>,
}

/// The orchestrator's typed, human-approved plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlicePlan {
    pub goal: String,
    pub hash: String,
    pub slices: Vec<Slice>,
}

impl SlicePlan {
    pub fn campaign() -> Self {
        Self {
            goal: "make the fixture's failing test pass and implement double".to_owned(),
            hash: PLAN_HASH.to_owned(),
            slices: vec![
                Slice {
                    id: "increment-bugfix".to_owned(),
                    task: "fix increment's failing unit test".to_owned(),
                    provider: "claude-scripted".to_owned(),
                    files: vec!["src/increment.rs".to_owned()],
                },
                Slice {
                    id: "double-feature".to_owned(),
                    task: "implement double with a unit test".to_owned(),
                    provider: "codex-scripted".to_owned(),
                    files: vec!["src/double.rs".to_owned()],
                },
                Slice {
                    id: "budget".to_owned(),
                    task: "attempt an explicitly capped documentation slice".to_owned(),
                    provider: "claude-scripted".to_owned(),
                    files: vec!["README.md".to_owned()],
                },
                Slice {
                    id: "contract".to_owned(),
                    task: "raise rather than reshape a frozen contract".to_owned(),
                    provider: "codex-scripted".to_owned(),
                    files: vec!["Cargo.toml".to_owned()],
                },
            ],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
enum WorkerOutcome {
    Ready {
        slice: String,
        review: Review,
        finish_bar: Vec<String>,
    },
    Raise {
        slice: String,
        contract: String,
        evidence: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Review {
    reviewer: String,
    provider: String,
    verdict: String,
}

#[derive(Debug)]
struct TempFixture {
    root: PathBuf,
}

impl TempFixture {
    fn new() -> Result<Self> {
        let ordinal = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "odori-slice-fleet-{}-{ordinal}",
            std::process::id()
        ));
        fs::create_dir(&root).with_context(|| format!("create {}", root.display()))?;
        let fixture = Self { root };
        for copy in [
            "snapshot",
            "increment-bugfix",
            "double-feature",
            "integrated",
        ] {
            fixture.seed(copy)?;
        }
        Ok(fixture)
    }

    fn seed(&self, copy: &str) -> Result<()> {
        let root = self.root.join(copy);
        fs::create_dir_all(root.join("src"))?;
        fs::write(root.join("Cargo.toml"), FIXTURE_MANIFEST)?;
        fs::write(root.join("Cargo.lock"), FIXTURE_LOCK)?;
        fs::write(root.join("src/lib.rs"), FIXTURE_LIB)?;
        fs::write(root.join("src/increment.rs"), FIXTURE_INCREMENT)?;
        fs::write(root.join("src/double.rs"), FIXTURE_DOUBLE)?;
        Ok(())
    }

    fn copy(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
}

impl Drop for TempFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[derive(Debug, Default)]
struct Evidence {
    approvals: BTreeSet<String>,
    applied: BTreeSet<String>,
    finish_bars: BTreeMap<String, Vec<String>>,
    reviews: BTreeMap<String, String>,
    scope_refusals: u32,
    budget_exceeded: bool,
    raised: bool,
}

#[derive(Debug)]
struct FleetState {
    fixture: TempFixture,
    evidence: Mutex<Evidence>,
    events: tokio::sync::mpsc::UnboundedSender<FleetEvent>,
}

impl FleetState {
    fn approve(&self, slice: &str) {
        self.evidence
            .lock()
            .expect("fleet evidence lock")
            .approvals
            .insert(slice.to_owned());
    }
}

#[derive(Debug)]
enum FleetEvent {
    Plan(SlicePlan),
    SlicesReady,
    ApplyRefused(String),
    Applied(String),
    RaiseObserved,
}

#[derive(Debug, Clone)]
struct FleetProvider {
    name: &'static str,
    state: Arc<FleetState>,
}

impl FleetProvider {
    fn turn(
        &self,
        events: &TurnEventSink,
        request: &TurnRequest,
        text: String,
        input: u64,
        output: u64,
    ) -> TurnOutcome {
        let session_id = format!("{}-session", request.directives.name);
        events.emit(TurnEvent::SessionStarted {
            session_id: session_id.clone(),
        });
        let mut usage = TurnUsage::default();
        usage.input_tokens = Some(input);
        usage.output_tokens = Some(output);
        usage.total_cost_usd = Some((input + output) as f64 / 10_000.0);
        events.report_usage(usage.clone());
        let mut outcome = TurnOutcome::new(session_id, text);
        outcome.usage = usage;
        outcome
    }

    async fn orchestrate(
        &self,
        request: &TurnRequest,
        _events: &TurnEventSink,
    ) -> Result<String, TurnError> {
        if request.identity.turn == 0 {
            let plan = SlicePlan::campaign();
            self.state
                .events
                .send(FleetEvent::Plan(plan.clone()))
                .map_err(tooling)?;
            return serde_json::to_string_pretty(&plan).map_err(tooling);
        }
        if request.input == format!("APPROVE PLAN {PLAN_HASH}") {
            // This is the fan-out point: every transfer is a workflow
            // update, and every accepted transfer starts its target
            // AgentRun as a child workflow.
            let increment_bugfix = call_tool(
                &request.tooling,
                "transfer_to_increment_bugfix_worker",
                json!({"input": "fix increment within src/increment.rs, run its finish bar, then obtain hostile Codex review"}),
                "dispatch-increment-bugfix",
            );
            let double_feature = call_tool(
                &request.tooling,
                "transfer_to_double_feature_worker",
                json!({"input": "implement double within src/double.rs, run its finish bar, then obtain hostile Claude review"}),
                "dispatch-double-feature",
            );
            let budget = call_tool(
                &request.tooling,
                "transfer_to_budget_worker",
                json!({"input": "attempt the capped documentation slice"}),
                "dispatch-budget",
            );
            let raised = call_tool(
                &request.tooling,
                "transfer_to_contract_worker",
                json!({"input": "check whether Cargo.toml may be changed without operator approval"}),
                "dispatch-contract",
            );
            let (increment_bugfix, double_feature, budget, raised) =
                tokio::join!(increment_bugfix, double_feature, budget, raised);
            let (increment_bugfix, double_feature, budget, raised) =
                (increment_bugfix?, double_feature?, budget?, raised?);
            let increment_bugfix_text = tool_text(&increment_bugfix)?;
            let double_feature_text = tool_text(&double_feature)?;
            serde_json::from_str::<WorkerOutcome>(&increment_bugfix_text).map_err(tooling)?;
            serde_json::from_str::<WorkerOutcome>(&double_feature_text).map_err(tooling)?;
            ensure_tool_error(&budget, "budget handoff")?;
            let raised_text = tool_text(&raised)?;
            let WorkerOutcome::Raise { .. } =
                serde_json::from_str::<WorkerOutcome>(&raised_text).map_err(tooling)?
            else {
                return Err(tooling("contract worker did not raise"));
            };
            {
                let mut evidence = self.state.evidence.lock().expect("fleet evidence lock");
                evidence.budget_exceeded = true;
                evidence.raised = true;
            }
            self.state
                .events
                .send(FleetEvent::SlicesReady)
                .map_err(tooling)?;
            return Ok(json!({
                "approval_queue": ["increment-bugfix", "double-feature"],
                "budget": "BudgetExceeded(max_turns=0)",
                "raise": raised_text,
            })
            .to_string());
        }
        if let Some(slice) = request.input.strip_prefix("APPROVE APPLY ") {
            let result = call_tool(
                &request.tooling,
                "apply_slice",
                json!({
                    "slice": slice,
                    "plan_hash": PLAN_HASH,
                    "approval": request.input,
                }),
                &format!("apply-{slice}"),
            )
            .await?;
            let text = tool_text(&result)?;
            if result.pointer("/result/isError").and_then(Value::as_bool) == Some(true) {
                self.state
                    .events
                    .send(FleetEvent::ApplyRefused(slice.to_owned()))
                    .map_err(tooling)?;
                return Ok(text);
            }
            self.state
                .events
                .send(FleetEvent::Applied(slice.to_owned()))
                .map_err(tooling)?;
            return Ok(text);
        }
        if request.input.starts_with("RAISE DECISION ") {
            self.state
                .events
                .send(FleetEvent::RaiseObserved)
                .map_err(tooling)?;
            return Ok("operator kept the frozen contract; slice remains unapplied".to_owned());
        }
        Err(tooling(format!(
            "orchestrator rejected unrecognized approval signal {:?}",
            request.input
        )))
    }

    async fn work(
        &self,
        slice: &str,
        allowed_path: &str,
        test_filter: &str,
        replacement: &str,
        reviewer: &str,
        request: &TurnRequest,
    ) -> Result<String, TurnError> {
        let fence = call_tool(
            &request.tooling,
            "scope_write",
            json!({"path": "Cargo.toml", "content": "# scope escape"}),
            &format!("{slice}-scope-probe"),
        )
        .await?;
        ensure_tool_error(&fence, "scope fence")?;
        self.state
            .evidence
            .lock()
            .expect("fleet evidence lock")
            .scope_refusals += 1;
        let write = call_tool(
            &request.tooling,
            "scope_write",
            json!({"path": allowed_path, "content": replacement}),
            &format!("{slice}-write"),
        )
        .await?;
        let write_evidence = tool_text(&write)?;
        let finish = call_tool(
            &request.tooling,
            "finish_bar",
            json!({}),
            &format!("{slice}-finish-bar"),
        )
        .await?;
        let finish_text = tool_text(&finish)?;
        let review = call_tool(
            &request.tooling,
            &format!("transfer_to_{reviewer}"),
            json!({"input": format!(
                "Hostile review of {slice}; diff={write_evidence}; bar={finish_text}; reject scope creep and missing tests"
            )}),
            &format!("{slice}-hostile-review"),
        )
        .await?;
        let review: Review = serde_json::from_str(&tool_text(&review)?).map_err(tooling)?;
        self.state
            .evidence
            .lock()
            .expect("fleet evidence lock")
            .reviews
            .insert(slice.to_owned(), review.provider.clone());
        serde_json::to_string(&WorkerOutcome::Ready {
            slice: slice.to_owned(),
            review,
            finish_bar: vec![
                "cargo check --locked".to_owned(),
                format!("cargo test {test_filter} --locked"),
            ],
        })
        .map_err(tooling)
    }
}

#[async_trait]
impl Provider for FleetProvider {
    fn name(&self) -> &str {
        self.name
    }

    async fn execute_turn(
        &self,
        request: TurnRequest,
        events: TurnEventSink,
    ) -> Result<TurnOutcome, TurnError> {
        let name = request.directives.name.as_str();
        let text = match name {
            "orchestrator" => self.orchestrate(&request, &events).await?,
            "increment-bugfix-worker" => {
                self.work(
                    "increment-bugfix",
                    "src/increment.rs",
                    "increment",
                    FIX_INCREMENT,
                    "reviewer_codex",
                    &request,
                )
                .await?
            }
            "double-feature-worker" => {
                self.work(
                    "double-feature",
                    "src/double.rs",
                    "double",
                    FIX_DOUBLE,
                    "reviewer_claude",
                    &request,
                )
                .await?
            }
            "contract-worker" => serde_json::to_string(&WorkerOutcome::Raise {
                slice: "contract".to_owned(),
                contract: "workspace dependency graph is operator-owned".to_owned(),
                evidence: "the requested Cargo.toml edit falls outside the declared feature files"
                    .to_owned(),
            })
            .map_err(tooling)?,
            "reviewer-codex" | "reviewer-claude" => {
                if !request.input.contains("\"before\"")
                    || !request.input.contains("\"after\"")
                    || !request.input.contains("\"green\":true")
                {
                    return Err(tooling(
                        "review did not receive diff and finish-bar evidence",
                    ));
                }
                serde_json::to_string(&Review {
                    reviewer: name.to_owned(),
                    provider: self.name.to_owned(),
                    verdict: "approve: scoped diff plus green targeted bar".to_owned(),
                })
                .map_err(tooling)?
            }
            other => return Err(tooling(format!("script has no agent {other}"))),
        };
        Ok(self.turn(&events, &request, text, 40, 10))
    }
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

fn tool_text(frame: &Value) -> Result<String, TurnError> {
    if let Some(error) = frame.pointer("/error/message").and_then(Value::as_str) {
        return Err(tooling(error));
    }
    frame
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| tooling(format!("tool response has no text result: {frame}")))
}

fn ensure_tool_error(frame: &Value, label: &str) -> Result<(), TurnError> {
    match frame.pointer("/result/isError").and_then(Value::as_bool) {
        Some(true) => Ok(()),
        _ => Err(tooling(format!("{label} unexpectedly succeeded: {frame}"))),
    }
}

fn scope_write_tool(slice: &'static str, allowed: &'static str, state: Arc<FleetState>) -> Tool {
    Tool::new(
        "scope_write",
        "Write one file, but only inside this slice's declared scope.",
        json!({
            "type": "object",
            "properties": {"path": {"type": "string"}, "content": {"type": "string"}},
            "required": ["path", "content"],
            "additionalProperties": false,
        }),
        move |_context, args| {
            let state = state.clone();
            async move {
                let path = args
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| ToolFailure::terminal("path must be a string"))?;
                let content = args
                    .get("content")
                    .and_then(Value::as_str)
                    .ok_or_else(|| ToolFailure::terminal("content must be a string"))?;
                let relative = Path::new(path);
                let safe = !relative.is_absolute()
                    && relative.components().all(|component| {
                        matches!(component, Component::Normal(_) | Component::CurDir)
                    });
                if !safe || path != allowed {
                    return Err(ToolFailure::terminal(format!(
                        "scope fence: {path:?} is outside declared path {allowed:?}"
                    )));
                }
                let destination = state.fixture.copy(slice).join(relative);
                let before = fs::read_to_string(&destination).map_err(|error| {
                    ToolFailure::terminal(format!("read {}: {error}", destination.display()))
                })?;
                fs::write(&destination, content).map_err(|error| {
                    ToolFailure::terminal(format!("write {}: {error}", destination.display()))
                })?;
                Ok(json!({
                    "written": path,
                    "scope": [allowed],
                    "before": before,
                    "after": content,
                }))
            }
        },
    )
}

fn finish_bar_tool(slice: &'static str, test_filter: &'static str, state: Arc<FleetState>) -> Tool {
    Tool::new(
        "finish_bar",
        "Run this slice's declared cargo check and targeted test as durable activities.",
        json!({"type": "object", "additionalProperties": false}),
        move |_context, _args| {
            let state = state.clone();
            async move {
                let root = state.fixture.copy(slice);
                let commands = vec![
                    vec!["check", "--locked"],
                    vec!["test", test_filter, "--locked"],
                ];
                let mut recorded = Vec::new();
                for arguments in commands {
                    let command_root = root.clone();
                    let command_arguments = arguments.clone();
                    let output = tokio::task::spawn_blocking(move || {
                        Command::new("cargo")
                            .args(&command_arguments)
                            .current_dir(command_root)
                            .output()
                    })
                    .await
                    .map_err(|error| ToolFailure::terminal(error.to_string()))?
                    .map_err(|error| ToolFailure::terminal(error.to_string()))?;
                    if !output.status.success() {
                        return Err(ToolFailure::terminal(format!(
                            "cargo {} failed: {}",
                            arguments.join(" "),
                            String::from_utf8_lossy(&output.stderr)
                        )));
                    }
                    recorded.push(format!("cargo {}", arguments.join(" ")));
                }
                state
                    .evidence
                    .lock()
                    .expect("fleet evidence lock")
                    .finish_bars
                    .insert(slice.to_owned(), recorded.clone());
                Ok(json!({"green": true, "commands": recorded}))
            }
        },
    )
}

fn apply_tool(state: Arc<FleetState>) -> Tool {
    Tool::new(
        "apply_slice",
        "Apply exactly one reviewed slice after its per-item human approval signal.",
        json!({
            "type": "object",
            "properties": {
                "slice": {"enum": ["increment-bugfix", "double-feature"]},
                "plan_hash": {"const": PLAN_HASH},
                "approval": {"type": "string"}
            },
            "required": ["slice", "plan_hash", "approval"],
            "additionalProperties": false,
        }),
        move |_context, args| {
            let state = state.clone();
            async move {
                let slice = args
                    .get("slice")
                    .and_then(Value::as_str)
                    .ok_or_else(|| ToolFailure::terminal("slice is required"))?;
                let approval = args
                    .get("approval")
                    .and_then(Value::as_str)
                    .ok_or_else(|| ToolFailure::terminal("approval is required"))?;
                if approval != format!("APPROVE APPLY {slice}") {
                    return Err(ToolFailure::terminal(format!(
                        "approval gate: invalid receipt for {slice}"
                    )));
                }
                let approved = state
                    .evidence
                    .lock()
                    .expect("fleet evidence lock")
                    .approvals
                    .contains(slice);
                if !approved {
                    return Err(ToolFailure::terminal(format!(
                        "approval gate: {slice} has no recorded per-item approval"
                    )));
                }
                let file = match slice {
                    "increment-bugfix" => "src/increment.rs",
                    "double-feature" => "src/double.rs",
                    other => {
                        return Err(ToolFailure::terminal(format!(
                            "apply gate: unknown slice {other}"
                        )));
                    }
                };
                let source = state.fixture.copy(slice).join(file);
                let destination = state.fixture.copy("integrated").join(file);
                fs::copy(&source, &destination)
                    .map_err(|error| ToolFailure::terminal(format!("apply {slice}: {error}")))?;
                state
                    .evidence
                    .lock()
                    .expect("fleet evidence lock")
                    .applied
                    .insert(slice.to_owned());
                Ok(json!({"applied": slice, "plan_hash": PLAN_HASH}))
            }
        },
    )
}

fn agents(state: Arc<FleetState>) -> AgentRegistry {
    let worker_budget = RunBudget::unlimited()
        .with_max_turns(2)
        .with_max_total_tokens(1_000)
        .with_max_cost_usd(1.0);
    let mut registry = AgentRegistry::new();
    registry.register(
        Agent::new(
            "orchestrator",
            "Plan bounded slices, wait for the exact plan approval, delegate as child workflows, and apply only individually approved items.",
        )
        .with_provider("codex-scripted")
        .with_budget(
            RunBudget::unlimited()
                // Parent accounting includes every delegated worker and
                // reviewer turn, as well as its own approval turns.
                .with_max_turns(20)
                .with_max_total_tokens(10_000)
                .with_max_cost_usd(10.0),
        )
        .with_handoff(Handoff::new("increment-bugfix-worker"))
        .with_handoff(Handoff::new("double-feature-worker"))
        .with_handoff(Handoff::new("budget-worker"))
        .with_handoff(Handoff::new("contract-worker"))
        .with_tool(apply_tool(state.clone())),
    );
    registry.register(
        Agent::new(
            "increment-bugfix-worker",
            "Fix increment only in src/increment.rs, prove its bar, then request Codex review.",
        )
        .with_provider("claude-scripted")
        .with_budget(worker_budget.clone())
        .with_tool(scope_write_tool(
            "increment-bugfix",
            "src/increment.rs",
            state.clone(),
        ))
        .with_tool(finish_bar_tool(
            "increment-bugfix",
            "increment",
            state.clone(),
        ))
        .with_handoff(Handoff::new("reviewer-codex")),
    );
    registry.register(
        Agent::new(
            "double-feature-worker",
            "Implement double only in src/double.rs, prove its bar, then request Claude review.",
        )
        .with_provider("codex-scripted")
        .with_budget(worker_budget)
        .with_tool(scope_write_tool(
            "double-feature",
            "src/double.rs",
            state.clone(),
        ))
        .with_tool(finish_bar_tool("double-feature", "double", state.clone()))
        .with_handoff(Handoff::new("reviewer-claude")),
    );
    registry.register(
        Agent::new(
            "budget-worker",
            "This slice demonstrates a clean budget stop.",
        )
        .with_provider("claude-scripted")
        .with_budget(RunBudget::unlimited().with_max_turns(0)),
    );
    registry.register(
        Agent::new(
            "contract-worker",
            "Return a typed Raise when the frozen contract cannot express the requested edit.",
        )
        .with_provider("codex-scripted")
        .with_budget(RunBudget::unlimited().with_max_turns(1)),
    );
    registry.register(
        Agent::new(
            "reviewer-codex",
            "Review Claude-authored work adversarially.",
        )
        .with_provider("codex-scripted")
        .with_budget(RunBudget::unlimited().with_max_turns(1)),
    );
    registry.register(
        Agent::new(
            "reviewer-claude",
            "Review Codex-authored work adversarially.",
        )
        .with_provider("claude-scripted")
        .with_budget(RunBudget::unlimited().with_max_turns(1)),
    );
    registry
}

async fn engine() -> Result<(Engine, TcpListener, TcpListener)> {
    let grpc_guard = TcpListener::bind("127.0.0.1:0")?;
    let nexus_guard = TcpListener::bind("127.0.0.1:0")?;
    let mut config = TokeiraConfig::default();
    config.infrastructure.network.grpc_addr = grpc_guard.local_addr()?.to_string();
    config.policy.nexus_completion.http_addr = nexus_guard.local_addr()?.to_string();
    let engine = Engine::start_with_config(config).await?;
    Ok((engine, grpc_guard, nexus_guard))
}

async fn next_event(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<FleetEvent>,
) -> Result<FleetEvent> {
    tokio::time::timeout(std::time::Duration::from_secs(30), receiver.recv())
        .await
        .context("fleet event timed out")?
        .context("fleet event channel closed")
}

/// Evidence returned by the deterministic full-path example and test.
#[derive(Debug)]
pub struct FleetReport {
    pub output: RunOutput,
    pub plan: SlicePlan,
    pub applied: Vec<String>,
    pub finish_bars: BTreeMap<String, Vec<String>>,
    pub reviews: BTreeMap<String, String>,
    pub scope_refusals: u32,
    pub budget_exceeded: bool,
    pub raised: bool,
}

/// Run the complete fleet with a scripted harness over the real embedded
/// engine and HTTP bridge. Every signal, child workflow, update, and tool
/// result is real; only model choice is scripted for determinism.
pub async fn run_scripted_fleet(print: bool) -> Result<FleetReport> {
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = Arc::new(FleetState {
        fixture: TempFixture::new()?,
        evidence: Mutex::new(Evidence::default()),
        events: sender,
    });
    let claude = Arc::new(FleetProvider {
        name: "claude-scripted",
        state: state.clone(),
    });
    let codex = Arc::new(FleetProvider {
        name: "codex-scripted",
        state: state.clone(),
    });
    let (embedded, _grpc_guard, _nexus_guard) = engine().await?;
    let runtime = OdoriRuntime::builder("example-slice-fleet")
        .connect(ConnectTarget::service_override(embedded.service_override()))
        .agents(agents(state.clone()))
        .providers(Providers::new(codex).with(claude))
        .bridge(BridgeConfig::default())
        .start()
        .await?;

    // The planner is a normal durable AgentRun and the runner enforces the
    // typed decode before anything can reach the approval seat.
    let odori::Json(typed_plan): odori::Json<SlicePlan> = runtime
        .runner()
        .run(
            "orchestrator",
            "GOAL: produce the bounded slice plan for the bundled fixture",
            &format!("slice-plan-{}", NEXT_TEMP.fetch_add(1, Ordering::Relaxed)),
        )
        .await?;
    let FleetEvent::Plan(planned_event) = next_event(&mut receiver).await? else {
        return Err(anyhow!("typed plan run emitted the wrong event"));
    };
    ensure!(typed_plan == planned_event);

    let conversation = runtime
        .runner()
        .start_conversation(
            "orchestrator",
            "GOAL: repair and extend the bundled fixture under the fleet policy",
            &format!("slice-fleet-{}", NEXT_TEMP.fetch_add(1, Ordering::Relaxed)),
        )
        .await?;
    let FleetEvent::Plan(plan) = next_event(&mut receiver).await? else {
        return Err(anyhow!("plan was not the first fleet event"));
    };
    ensure!(plan == typed_plan);
    if print {
        println!(
            "PLAN {}\n{}",
            plan.hash,
            serde_json::to_string_pretty(&plan)?
        );
        println!("HUMAN APPROVAL: plan {}", plan.hash);
    }
    conversation
        .send(&format!("APPROVE PLAN {}", plan.hash))
        .await?;
    ensure!(matches!(
        next_event(&mut receiver).await?,
        FleetEvent::SlicesReady
    ));
    if print {
        let (finish_bars, reviews) = {
            let evidence = state.evidence.lock().expect("fleet evidence lock");
            (evidence.finish_bars.clone(), evidence.reviews.clone())
        };
        for slice in ["increment-bugfix", "double-feature"] {
            println!("SCOPE FENCE: {slice} Cargo.toml -> tool error");
            println!(
                "FINISH BAR: {slice} -> {:?} -> green",
                finish_bars.get(slice).expect("finish bar recorded")
            );
            println!(
                "HOSTILE REVIEW: {} -> approve",
                reviews.get(slice).expect("review recorded")
            );
        }
        println!("BUDGET: budget-worker -> BudgetExceeded(max_turns=0)");
        println!("RAISE: contract-worker -> operator approval seat");
    }

    // Negative proof: the same apply signal is refused before the seat
    // records its item-level approval.
    conversation.send("APPROVE APPLY increment-bugfix").await?;
    match next_event(&mut receiver).await? {
        FleetEvent::ApplyRefused(slice) if slice == "increment-bugfix" => {
            if print {
                println!("APPROVAL GATE: apply increment-bugfix before approval -> tool error");
            }
        }
        event => return Err(anyhow!("unexpected approval-gate event: {event:?}")),
    }

    for slice in ["increment-bugfix", "double-feature"] {
        state.approve(slice);
        if print {
            println!("HUMAN APPROVAL: apply {slice}");
        }
        conversation.send(&format!("APPROVE APPLY {slice}")).await?;
        match next_event(&mut receiver).await? {
            FleetEvent::Applied(applied) if applied == slice => {}
            event => return Err(anyhow!("unexpected apply event: {event:?}")),
        }
    }
    conversation
        .send("RAISE DECISION keep frozen contract; do not apply contract slice")
        .await?;
    ensure!(matches!(
        next_event(&mut receiver).await?,
        FleetEvent::RaiseObserved
    ));
    let output = conversation.end().await?;

    let final_bar = Command::new("cargo")
        .args(["test", "--locked"])
        .current_dir(state.fixture.copy("integrated"))
        .output()?;
    ensure!(
        final_bar.status.success(),
        "integrated finish bar failed: {}",
        String::from_utf8_lossy(&final_bar.stderr)
    );
    let report = {
        let evidence = state.evidence.lock().expect("fleet evidence lock");
        FleetReport {
            output,
            plan,
            applied: evidence.applied.iter().cloned().collect(),
            finish_bars: evidence.finish_bars.clone(),
            reviews: evidence.reviews.clone(),
            scope_refusals: evidence.scope_refusals,
            budget_exceeded: evidence.budget_exceeded,
            raised: evidence.raised,
        }
    };
    runtime.shutdown().await?;
    embedded.shutdown().await?;
    Ok(report)
}

pub fn verify_fleet(report: &FleetReport) -> Result<()> {
    ensure!(matches!(report.output.end, RunEnd::ConversationEnded));
    // Six direct approval/orchestration turns plus five delegated worker,
    // reviewer, and Raise turns: child spend is parent spend.
    ensure!(report.output.turns == 11);
    ensure!(report.output.usage.input_tokens == 440);
    ensure!(report.output.usage.output_tokens == 110);
    ensure!(report.plan.hash == PLAN_HASH);
    ensure!(report.applied == ["double-feature", "increment-bugfix"]);
    ensure!(report.scope_refusals == 2);
    ensure!(report.budget_exceeded);
    ensure!(report.raised);
    ensure!(report.finish_bars.len() == 2);
    ensure!(report.reviews.get("increment-bugfix").map(String::as_str) == Some("codex-scripted"));
    ensure!(report.reviews.get("double-feature").map(String::as_str) == Some("claude-scripted"));
    Ok(())
}

#[derive(Debug)]
enum RewindEvent {
    CheckpointRecorded,
    FailureReturned,
    RetryPresented(u32),
}

#[derive(Debug)]
struct RewindState {
    tool_executions: AtomicU64,
    presentations: Mutex<Vec<(u32, String)>>,
    events: tokio::sync::mpsc::UnboundedSender<RewindEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeliberationSnapshot {
    goal: String,
    checkpoint: String,
    plan_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TimelineInput {
    snapshot: DeliberationSnapshot,
    decision: String,
}

#[derive(Debug, Clone)]
struct RewindProvider {
    state: Arc<RewindState>,
}

#[async_trait]
impl Provider for RewindProvider {
    fn name(&self) -> &str {
        "rewind-scripted"
    }

    async fn execute_turn(
        &self,
        request: TurnRequest,
        events: TurnEventSink,
    ) -> Result<TurnOutcome, TurnError> {
        let session_id = "rewind-session".to_owned();
        events.emit(TurnEvent::SessionStarted {
            session_id: session_id.clone(),
        });
        if request.directives.name == "timeline" {
            let input: TimelineInput = serde_json::from_str(&request.input).map_err(tooling)?;
            let text = json!({
                "plan_hash": input.snapshot.plan_hash,
                "decision": input.decision,
                "result": format!("{} from {}", input.decision, input.snapshot.checkpoint),
            })
            .to_string();
            return Ok(TurnOutcome::new(session_id, text));
        }

        let call_id = "stable-checkpoint-call";
        self.state
            .presentations
            .lock()
            .expect("rewind presentations lock")
            .push((request.identity.attempt, call_id.to_owned()));
        if request.identity.attempt > 1 {
            self.state
                .events
                .send(RewindEvent::RetryPresented(request.identity.attempt))
                .map_err(tooling)?;
        }
        let result = call_tool(
            &request.tooling,
            "checkpoint",
            json!({"label": "plan-ready"}),
            call_id,
        )
        .await?;
        let receipt = tool_text(&result)?;
        if request.identity.attempt == 1 {
            self.state
                .events
                .send(RewindEvent::CheckpointRecorded)
                .map_err(tooling)?;
            self.state
                .events
                .send(RewindEvent::FailureReturned)
                .map_err(tooling)?;
            return Err(TurnError::HarnessDied {
                exit_code: Some(137),
                stderr_head: "demo kill at plan-ready checkpoint".to_owned(),
            });
        }
        let snapshot = DeliberationSnapshot {
            goal: "choose a release path".to_owned(),
            checkpoint: receipt,
            plan_hash: PLAN_HASH.to_owned(),
        };
        Ok(TurnOutcome::new(
            session_id,
            serde_json::to_string(&snapshot).map_err(tooling)?,
        ))
    }
}

fn rewind_agents(state: Arc<RewindState>) -> AgentRegistry {
    let executions = state.clone();
    let checkpoint = Tool::new(
        "checkpoint",
        "Record the deterministic deliberation checkpoint.",
        json!({
            "type": "object",
            "properties": {"label": {"type": "string"}},
            "required": ["label"],
            "additionalProperties": false,
        }),
        move |context, args| {
            let executions = executions.clone();
            async move {
                let ordinal = executions.tool_executions.fetch_add(1, Ordering::SeqCst) + 1;
                Ok(json!(format!(
                    "checkpoint:{}:{}:execution-{ordinal}",
                    args.get("label")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown"),
                    context.invocation_id
                )))
            }
        },
    );
    let mut registry = AgentRegistry::new();
    registry.register(
        Agent::new(
            "rewind-worker",
            "Record one durable checkpoint, then finish the deliberation. Re-present the same call id after restart.",
        )
        .with_provider("rewind-scripted")
        .with_tool(checkpoint),
    );
    registry.register(
        Agent::new(
            "timeline",
            "Restore the supplied deliberation snapshot and follow exactly the supplied decision.",
        )
        .with_provider("rewind-scripted"),
    );
    registry
}

async fn rewind_runtime(embedded: &Engine, state: Arc<RewindState>) -> Result<OdoriRuntime> {
    OdoriRuntime::builder("example-rewind")
        .connect(ConnectTarget::service_override(embedded.service_override()))
        .agents(rewind_agents(state.clone()))
        .providers(Providers::new(Arc::new(RewindProvider { state })))
        .bridge(BridgeConfig::default())
        .start()
        .await
}

/// Observable proof from the rewind example.
#[derive(Debug)]
pub struct RewindReport {
    pub dedupe_tool_executions: u64,
    pub total_tool_executions: u64,
    pub presentations: Vec<(u32, String)>,
    pub durable_attempt_before_replacement: i32,
    pub replacement_retry_attempt: u32,
    pub replacement_completion: std::time::Duration,
    pub timeline_a: String,
    pub timeline_b: String,
}

/// Kill the first harness attempt after a durable tool result, stop its
/// worker, start a replacement over the same engine, and restore the result
/// from the invocation registry. Then use the successful deliberation as one
/// immutable snapshot for two new workflows with deliberately different
/// decisions.
pub async fn run_rewind(print: bool) -> Result<RewindReport> {
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = Arc::new(RewindState {
        tool_executions: AtomicU64::new(0),
        presentations: Mutex::new(Vec::new()),
        events: sender,
    });
    let (embedded, _grpc_guard, _nexus_guard) = engine().await?;
    let runtime = rewind_runtime(&embedded, state.clone()).await?;
    let config = odori::RunConfig::default()
        .with_turn_timeout(std::time::Duration::from_secs(15))
        .with_turn_heartbeat_timeout(std::time::Duration::from_millis(500))
        .with_turn_max_attempts(5);
    let exact_runner = runtime.runner();
    let exact_config = config.clone();
    let exact_deliberation = tokio::spawn(async move {
        exact_runner
            .run_with_config::<String>(
                "rewind-worker",
                "deliberate until the plan-ready checkpoint",
                "rewind-exact-resume",
                exact_config,
            )
            .await
    });
    ensure!(matches!(
        next_rewind_event(&mut receiver).await?,
        RewindEvent::CheckpointRecorded
    ));
    ensure!(matches!(
        next_rewind_event(&mut receiver).await?,
        RewindEvent::FailureReturned
    ));
    let RewindEvent::RetryPresented(exact_attempt) = next_rewind_event(&mut receiver).await? else {
        return Err(anyhow!("same-worker retry was not presented"));
    };
    let snapshot_json = exact_deliberation
        .await
        .context("join exact deliberation")??;
    let snapshot: DeliberationSnapshot = serde_json::from_str(&snapshot_json)?;
    let dedupe_tool_executions = state.tool_executions.load(Ordering::SeqCst);
    if print {
        println!(
            "RESUME EXACTLY: attempts 1 and {exact_attempt} presented stable-checkpoint-call; tool executions={dedupe_tool_executions}"
        );
    }

    let restart_runner = runtime.runner();
    let restart_probe = tokio::spawn(async move {
        restart_runner
            .run_with_config::<String>(
                "rewind-worker",
                "restart canary at the plan-ready checkpoint",
                "rewind-worker-restart-canary",
                config,
            )
            .await
    });
    ensure!(matches!(
        next_rewind_event(&mut receiver).await?,
        RewindEvent::CheckpointRecorded
    ));
    ensure!(matches!(
        next_rewind_event(&mut receiver).await?,
        RewindEvent::FailureReturned
    ));
    if print {
        println!("KILL: harness exited 137 after the restart canary checkpoint");
        println!("STOP: worker A drains and stops; embedded engine stays alive");
    }
    let durable_client = runtime.client();
    runtime.shutdown().await?;
    let durable_attempt_before_replacement = wait_for_durable_activity_attempt(
        &durable_client,
        "rewind-worker-restart-canary",
        2,
        std::time::Duration::from_secs(5),
    )
    .await?;
    if print {
        println!(
            "DURABLE: engine reports activity attempt {durable_attempt_before_replacement} before worker B starts"
        );
    }
    let replacement = rewind_runtime(&embedded, state.clone()).await?;
    if print {
        println!("RESTART: replacement worker uses default workflow cache settings");
    }
    let (replacement_retry_attempt, replacement_completion) = match tokio::time::timeout(
        std::time::Duration::from_secs(15),
        receiver.recv(),
    )
    .await
    {
        Ok(Some(RewindEvent::RetryPresented(retry_attempt))) => {
            if print {
                println!("RESTART RESUME: replacement polled attempt {retry_attempt}");
            }
            let completion_started = std::time::Instant::now();
            tokio::time::timeout(std::time::Duration::from_secs(15), restart_probe)
                .await
                .context(
                    "replacement retry did not complete across the 10s sticky fallback window",
                )?
                .context("join restart probe")??;
            let replacement_completion = completion_started.elapsed();
            if print {
                println!(
                    "STICKY FALLBACK: workflow completed on worker B in {replacement_completion:?} after retry presentation"
                );
            }
            (retry_attempt, replacement_completion)
        }
        Ok(Some(event)) => return Err(anyhow!("unexpected restart event: {event:?}")),
        Ok(None) => return Err(anyhow!("restart event channel closed")),
        Err(_) => {
            restart_probe.abort();
            let still_pending =
                pending_activity_observation(&durable_client, "rewind-worker-restart-canary")
                    .await?
                    .context("restart-canary activity disappeared without reaching the provider")?;
            ensure!(still_pending.attempt >= durable_attempt_before_replacement);
            return Err(anyhow!(
                "replacement worker did not resume activity attempt {} ({}) within 15s",
                still_pending.attempt,
                still_pending.state
            ));
        }
    };
    let input_a = serde_json::to_string(&TimelineInput {
        snapshot: snapshot.clone(),
        decision: "ship".to_owned(),
    })?;
    let input_b = serde_json::to_string(&TimelineInput {
        snapshot,
        decision: "hold".to_owned(),
    })?;
    let timeline_runner_a = replacement.runner();
    let timeline_runner_b = replacement.runner();
    let (timeline_a, timeline_b) = tokio::try_join!(
        timeline_runner_a.run::<String>("timeline", &input_a, "rewind-timeline-a"),
        timeline_runner_b.run::<String>("timeline", &input_b, "rewind-timeline-b"),
    )?;
    let report = RewindReport {
        dedupe_tool_executions,
        total_tool_executions: state.tool_executions.load(Ordering::SeqCst),
        presentations: state
            .presentations
            .lock()
            .expect("rewind presentations lock")
            .clone(),
        durable_attempt_before_replacement,
        replacement_retry_attempt,
        replacement_completion,
        timeline_a,
        timeline_b,
    };
    if print {
        println!(
            "PRESENTATIONS: {:?}; total checkpoint executions={}",
            report.presentations, report.total_tool_executions
        );
        println!("TIMELINE A: {}", report.timeline_a);
        println!("TIMELINE B: {}", report.timeline_b);
    }
    replacement.shutdown().await?;
    embedded.shutdown().await?;
    Ok(report)
}

#[derive(Debug)]
struct PendingActivityObservation {
    attempt: i32,
    state: String,
}

async fn pending_activity_observation(
    client: &temporalio_client::Client,
    workflow_id: &str,
) -> Result<Option<PendingActivityObservation>> {
    let handle = client.get_workflow_handle::<UntypedWorkflow>(workflow_id);
    let description = handle
        .describe(WorkflowDescribeOptions::default())
        .await
        .context("describe restart-canary workflow")?;
    Ok(description
        .raw()
        .pending_activities
        .iter()
        .max_by_key(|activity| activity.attempt)
        .map(|activity| PendingActivityObservation {
            attempt: activity.attempt,
            state: format!("{:?}", activity.state()),
        }))
}

async fn wait_for_durable_activity_attempt(
    client: &temporalio_client::Client,
    workflow_id: &str,
    minimum_attempt: i32,
    wait_for: std::time::Duration,
) -> Result<i32> {
    let deadline = tokio::time::Instant::now() + wait_for;
    loop {
        let observed_attempt = pending_activity_observation(client, workflow_id)
            .await?
            .map(|activity| activity.attempt)
            .unwrap_or_default();
        if observed_attempt >= minimum_attempt {
            return Ok(observed_attempt);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(anyhow!(
                "engine did not durably advance the restart-canary activity to attempt {minimum_attempt} within {wait_for:?}; last observed attempt was {observed_attempt}"
            ));
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

async fn next_rewind_event(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<RewindEvent>,
) -> Result<RewindEvent> {
    tokio::time::timeout(std::time::Duration::from_secs(15), receiver.recv())
        .await
        .context("rewind event timed out")?
        .context("rewind event channel closed")
}

pub fn verify_rewind(report: &RewindReport) -> Result<()> {
    ensure!(report.dedupe_tool_executions == 1);
    ensure!(report.total_tool_executions == 2);
    ensure!(report.durable_attempt_before_replacement >= 2);
    ensure!(report.replacement_retry_attempt >= 2);
    ensure!(report.replacement_completion <= std::time::Duration::from_secs(15));
    ensure!(report.presentations.len() >= 4);
    ensure!(
        report
            .presentations
            .iter()
            .all(|(_, call_id)| call_id == "stable-checkpoint-call")
    );
    ensure!(report.timeline_a.contains("ship"));
    ensure!(report.timeline_b.contains("hold"));
    ensure!(report.timeline_a != report.timeline_b);
    Ok(())
}
