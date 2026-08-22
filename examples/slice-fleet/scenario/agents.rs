//! Agent registry, handoffs, and budget policy for the fleet.

use std::sync::Arc;

use odori::{Agent, AgentRegistry, RunBudget, agents::Handoff};

use super::{
    state::FleetState,
    tools::{apply_tool, finish_bar_tool, scope_write_tool},
};

pub(super) fn registry(state: Arc<FleetState>) -> AgentRegistry {
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
