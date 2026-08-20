//! The invocation registry: the workflow-owned authority on tool execution
//! (mcp-bridge spec, Requirements 3 and 4).
//!
//! A pure, deterministic state machine held as run-loop workflow state — no
//! I/O, no clocks, rebuilt by replay (Property 3). The `tool_invoked`
//! update handler is its only caller in production; being pure, it is also
//! directly property-testable against a reference model.
//!
//! Semantics, from the spec:
//!
//! - Identity is (turn, call id); the **attempt is a fencing dimension**,
//!   tracked as a monotonic per-turn watermark fed by the attempts the
//!   workflow observes. (The workflow cannot see activity attempts
//!   directly — retries happen inside one activity future — so the
//!   watermark is derived from what invocations present, which is itself
//!   recorded history and therefore deterministic.)
//! - A recorded (turn, call id) is served from the registry — same attempt,
//!   later attempt, or superseded attempt alike (Requirements 3.1, 3.3,
//!   4.3).
//! - An in-flight (turn, call id) is joined, never re-executed
//!   (Requirement 3.2).
//! - An **unrecorded** call from a superseded attempt is fenced: it cannot
//!   start new work (Requirement 4.2).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// The identity stamped on one bridged tool call presentation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvocationId {
    /// Zero-based turn index within the run.
    pub turn: u32,
    /// Turn-activity attempt that carried the call (fencing dimension).
    pub attempt: u32,
    /// Harness-assigned tool-use id (Claude Code:
    /// `_meta["claudecode/toolUseId"]`).
    pub call_id: String,
}

/// An MCP-shaped tool result: the content array plus the error flag,
/// exactly what the bridge returns to the harness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallResult {
    /// MCP `content` blocks, held as raw JSON.
    pub content: serde_json::Value,
    /// MCP `isError`: a model-visible tool failure, not a bridge failure.
    pub is_error: bool,
}

impl ToolCallResult {
    /// A successful single-text-block result.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: serde_json::json!([{ "type": "text", "text": text.into() }]),
            is_error: false,
        }
    }

    /// A model-visible failure result.
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            content: serde_json::json!([{ "type": "text", "text": message.into() }]),
            is_error: true,
        }
    }
}

/// The registry's verdict on one presentation.
#[derive(Debug)]
pub enum Admission {
    /// Fresh invocation: the caller must schedule `execute_tool` and then
    /// [`InvocationRegistry::complete`] the ticket. At most one `Execute` is
    /// ever admitted per (turn, call id) — Property 1.
    Execute(ExecutionTicket),
    /// The same identity is already executing: await its completion (the
    /// registry state flips to complete; observe via
    /// [`InvocationRegistry::recorded`]).
    AwaitExisting,
    /// Already recorded: return this result, schedule nothing.
    Recorded(ToolCallResult),
    /// Unrecorded call from a superseded attempt: rejected, no work starts
    /// (Requirement 4.2).
    Fenced,
}

/// Proof of an `Execute` admission; consumed by
/// [`InvocationRegistry::complete`]. Deliberately neither `Clone` nor
/// constructible outside this module: one admission, one completion.
#[derive(Debug)]
pub struct ExecutionTicket {
    turn: u32,
    call_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum InvocationState {
    InFlight {
        /// Attempt that won the `Execute` admission (diagnostics).
        admitted_attempt: u32,
        /// Tool name, recorded for observability.
        tool: String,
    },
    Complete {
        result: ToolCallResult,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct TurnInvocations {
    /// Highest attempt observed for this turn; lower attempts are
    /// superseded.
    watermark: u32,
    calls: HashMap<String, InvocationState>,
}

/// The registry proper. One per run, inside the run-loop workflow's state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvocationRegistry {
    turns: HashMap<u32, TurnInvocations>,
}

impl InvocationRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Admit one presentation. Pure: the verdict is a function of prior
    /// admissions only.
    pub fn admit(&mut self, id: &InvocationId, tool: &str) -> Admission {
        let turn = self.turns.entry(id.turn).or_default();
        let superseded = id.attempt < turn.watermark;
        if id.attempt > turn.watermark {
            turn.watermark = id.attempt;
        }
        match turn.calls.get(&id.call_id) {
            Some(InvocationState::Complete { result }) => Admission::Recorded(result.clone()),
            Some(InvocationState::InFlight { .. }) => Admission::AwaitExisting,
            None if superseded => Admission::Fenced,
            None => {
                turn.calls.insert(
                    id.call_id.clone(),
                    InvocationState::InFlight {
                        admitted_attempt: id.attempt,
                        tool: tool.to_owned(),
                    },
                );
                Admission::Execute(ExecutionTicket {
                    turn: id.turn,
                    call_id: id.call_id.clone(),
                })
            }
        }
    }

    /// Record an execution's result, flipping its entry to complete. The
    /// ticket came from [`Self::admit`], so the entry exists and is in
    /// flight by construction.
    pub fn complete(&mut self, ticket: ExecutionTicket, result: ToolCallResult) {
        if let Some(turn) = self.turns.get_mut(&ticket.turn) {
            turn.calls
                .insert(ticket.call_id, InvocationState::Complete { result });
        }
    }

    /// The recorded result for (turn, call id), once complete. The waiting
    /// side of [`Admission::AwaitExisting`]: poll this from a workflow
    /// `wait_condition`.
    pub fn recorded(&self, turn: u32, call_id: &str) -> Option<&ToolCallResult> {
        match self.turns.get(&turn)?.calls.get(call_id)? {
            InvocationState::Complete { result } => Some(result),
            InvocationState::InFlight { .. } => None,
        }
    }

    /// Whether (turn, call id) is known at all (in flight or complete).
    pub fn contains(&self, turn: u32, call_id: &str) -> bool {
        self.turns
            .get(&turn)
            .is_some_and(|entries| entries.calls.contains_key(call_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(turn: u32, attempt: u32, call: &str) -> InvocationId {
        InvocationId {
            turn,
            attempt,
            call_id: call.to_owned(),
        }
    }

    #[test]
    fn fresh_call_executes_then_replays_recorded() {
        let mut registry = InvocationRegistry::new();
        let Admission::Execute(ticket) = registry.admit(&id(0, 1, "c1"), "deploy") else {
            panic!("fresh call must execute");
        };
        assert!(matches!(
            registry.admit(&id(0, 1, "c1"), "deploy"),
            Admission::AwaitExisting
        ));
        registry.complete(ticket, ToolCallResult::text("done"));
        let Admission::Recorded(result) = registry.admit(&id(0, 2, "c1"), "deploy") else {
            panic!("recorded call must be served from the registry");
        };
        assert_eq!(result, ToolCallResult::text("done"));
    }

    #[test]
    fn superseded_attempts_are_fenced_unless_recorded() {
        let mut registry = InvocationRegistry::new();
        let Admission::Execute(ticket) = registry.admit(&id(0, 2, "c1"), "t") else {
            panic!("execute");
        };
        registry.complete(ticket, ToolCallResult::text("r"));
        // Attempt 1 is superseded by the watermark at 2.
        assert!(matches!(
            registry.admit(&id(0, 1, "new"), "t"),
            Admission::Fenced
        ));
        assert!(matches!(
            registry.admit(&id(0, 1, "c1"), "t"),
            Admission::Recorded(_)
        ));
    }

    #[test]
    fn turns_are_independent() {
        let mut registry = InvocationRegistry::new();
        let Admission::Execute(_) = registry.admit(&id(0, 5, "c"), "t") else {
            panic!("execute");
        };
        // Turn 1 has its own watermark; attempt 1 is not superseded there.
        assert!(matches!(
            registry.admit(&id(1, 1, "c"), "t"),
            Admission::Execute(_)
        ));
    }
}
