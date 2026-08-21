//! Agent-to-agent delegation as a durable child workflow.
//!
//! A [`Handoff`] is exposed to the harness as a framework tool. The
//! `AgentRun::tool_invoked` update starts the target agent's own `AgentRun`
//! child workflow and awaits its result before answering the tool call. The
//! child enforces its target agent's caps, while its turns, tokens, cost, and
//! unknown-usage counters are also absorbed into the parent's run budget.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// A model-visible transfer from one agent to another.
///
/// The target runs under its own [`crate::guardrail::RunBudget`] and the
/// parent's remaining caps. Its completed turns and reported spend are also
/// charged to the parent, so nested delegation cannot escape parent budgets.
#[derive(Debug, Clone)]
pub struct Handoff {
    target: String,
    tool_name: String,
    description: String,
}

impl Handoff {
    /// Delegate to `target`. The default tool name is
    /// `transfer_to_<normalized-target>`.
    pub fn new(target: impl Into<String>) -> Self {
        let target = target.into();
        let suffix: String = target
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() {
                    character.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect();
        Self {
            tool_name: format!("transfer_to_{suffix}"),
            description: format!("Delegate this request to the {target} agent."),
            target,
        }
    }

    /// Override the model-visible tool name.
    pub fn with_tool_name(mut self, name: impl Into<String>) -> Self {
        self.tool_name = name.into();
        self
    }

    /// Override the model-visible description.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// Target agent name.
    pub fn target(&self) -> &str {
        &self.target
    }

    /// Model-visible framework-tool name.
    pub fn tool_name(&self) -> &str {
        &self.tool_name
    }

    /// Model-visible description.
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Input schema for the handoff context supplied by the model.
    pub fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "input": {
                    "type": "string",
                    "description": "The complete request and context for the target agent."
                }
            },
            "required": ["input"],
            "additionalProperties": false
        })
    }
}

/// Durable context passed from a parent run to its handoff child.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct HandoffContext {
    /// Parent agent making the transfer.
    pub source_agent: String,
    /// Child agent receiving it.
    pub target_agent: String,
    /// Model-supplied request/context, used as the child's initial prompt.
    pub input: String,
    /// Parent workflow id.
    pub parent_workflow_id: String,
    /// Parent workflow run id.
    pub parent_run_id: String,
    /// Parent turn in which the handoff was requested.
    pub parent_turn: u32,
    /// Harness call id; the durable idempotency key for this handoff.
    pub call_id: String,
}
