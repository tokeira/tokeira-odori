//! Approval-gated durable tools: one scoped write and its declared finish bar.

use std::{
    fs,
    path::{Component, Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use anyhow::Result;
use odori::{Agent, AgentRegistry, Tool, ToolFailure};
use serde_json::{Value, json};

use super::workspace::{ALLOWED_PATH, FIXED_LIB};

#[derive(Debug)]
pub(super) struct ApprovalState {
    workspace: PathBuf,
    approved_hash: Option<String>,
    apply_executions: AtomicU64,
    finish_bar_executions: AtomicU64,
}

impl ApprovalState {
    pub(super) fn waiting(workspace: PathBuf) -> Self {
        Self {
            workspace,
            approved_hash: None,
            apply_executions: AtomicU64::new(0),
            finish_bar_executions: AtomicU64::new(0),
        }
    }

    pub(super) fn approved(workspace: PathBuf, plan_hash: String) -> Self {
        Self {
            workspace,
            approved_hash: Some(plan_hash),
            apply_executions: AtomicU64::new(0),
            finish_bar_executions: AtomicU64::new(0),
        }
    }

    pub(super) fn workspace(&self) -> &Path {
        &self.workspace
    }

    pub(super) fn apply_executions(&self) -> u64 {
        self.apply_executions.load(Ordering::SeqCst)
    }

    pub(super) fn finish_bar_executions(&self) -> u64 {
        self.finish_bar_executions.load(Ordering::SeqCst)
    }
}

pub(super) fn registry(state: Arc<ApprovalState>) -> AgentRegistry {
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
