//! Scope-fenced writes, durable finish bars, and per-item apply approval.

use std::{fs, path::Path, process::Command, sync::Arc};

use odori::{Tool, ToolFailure};
use serde_json::{Value, json};

use super::{model::PLAN_HASH, state::FleetState, workspace::is_scoped_path};

pub(super) fn scope_write_tool(
    slice: &'static str,
    allowed: &'static str,
    state: Arc<FleetState>,
) -> Tool {
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
                let safe = is_scoped_path(relative);
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

pub(super) fn finish_bar_tool(
    slice: &'static str,
    test_filter: &'static str,
    state: Arc<FleetState>,
) -> Tool {
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

pub(super) fn apply_tool(state: Arc<FleetState>) -> Tool {
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
