# slice-fleet

This is a small, executable version of the fleet that built Odori: a typed
plan, a human serial-merge seat, provider-crossed workers and reviewers,
history-backed finish bars, child-workflow handoffs, budgets, and explicit
stop-and-raise.

The bundled fixture starts with a failing `increment` test and a missing
`double` feature. Every run copies that snapshot into fresh temporary worker and
integration trees. The scripted providers make model choices deterministic;
all durable behaviour is real: the embedded engine, child workflows, signals,
workflow updates, HTTP MCP bridge, registry, activities, retries, and budgets.

Run the deterministic path:

```console
cargo run --manifest-path tests/embedded/Cargo.toml --example slice-fleet
```

The policies are code:

- The orchestrator's first durable run is decoded as `Json<SlicePlan>` before
  the approval seat opens; every task has an explicit file list.
- `scope_write` closes over one slice's allowlist. The demo deliberately asks
  both workers to write `Cargo.toml`; each receives a model-visible tool error
  before its in-scope write succeeds.
- The plan and every apply receive separate signals. `apply_slice` also checks
  the per-item approval ledger, so a missing approval is a tool error.
- Worker finish bars (`cargo check --locked` plus a targeted test) execute as
  durable bridge activities. An item is eligible for review only after those
  results exist in workflow history.
- The `increment-bugfix` slice is authored by the Claude provider and reviewed
  by Codex; `double-feature` is authored by Codex and reviewed by Claude. Both
  reviews are child workflows.
- `budget-worker` has `max_turns = 0` and reaches `BudgetExceeded` cleanly.
  Its handoff error is recorded and the fleet continues at the approval seat.
- The orchestrator records eleven turns: six direct turns and five delegated
  worker/reviewer/Raise turns. The test asserts that parent-level accounting.
- `contract-worker` returns the typed `Raise` outcome. No code path silently
  stretches the frozen contract.

## Captured scripted transcript

```text
PLAN plan-v1-bugfix-feature-budget-contract
{
  "goal": "make the fixture's failing test pass and implement double",
  "hash": "plan-v1-bugfix-feature-budget-contract",
  "slices": [
    {
      "id": "increment-bugfix",
      "task": "fix increment's failing unit test",
      "provider": "claude-scripted",
      "files": ["src/increment.rs"]
    },
    {
      "id": "double-feature",
      "task": "implement double with a unit test",
      "provider": "codex-scripted",
      "files": ["src/double.rs"]
    },
    {
      "id": "budget",
      "task": "attempt an explicitly capped documentation slice",
      "provider": "claude-scripted",
      "files": ["README.md"]
    },
    {
      "id": "contract",
      "task": "raise rather than reshape a frozen contract",
      "provider": "codex-scripted",
      "files": ["Cargo.toml"]
    }
  ]
}
HUMAN APPROVAL: plan plan-v1-bugfix-feature-budget-contract
SCOPE FENCE: increment-bugfix Cargo.toml -> tool error
FINISH BAR: increment-bugfix -> ["cargo check --locked", "cargo test increment --locked"] -> green
HOSTILE REVIEW: codex-scripted -> approve
SCOPE FENCE: double-feature Cargo.toml -> tool error
FINISH BAR: double-feature -> ["cargo check --locked", "cargo test double --locked"] -> green
HOSTILE REVIEW: claude-scripted -> approve
BUDGET: budget-worker -> BudgetExceeded(max_turns=0)
RAISE: contract-worker -> operator approval seat
APPROVAL GATE: apply increment-bugfix before approval -> tool error
HUMAN APPROVAL: apply increment-bugfix
HUMAN APPROVAL: apply double-feature
GREEN: applied=["double-feature", "increment-bugfix"], turns=11, tokens=550
```

The integration test runs this entire path unguarded. Real provider smoke runs
live beside it behind the ignored quota marker; they are never part of the
default bar.

## Code structure

The example owns its complete implementation under `slice-fleet/scenario/`:

- `mod.rs` owns the approval-driven fleet lifecycle and report.
- `model.rs` owns the typed plan and worker outcomes.
- `provider.rs` scripts planning, work, hostile review, and approval behavior.
- `agents.rs` assembles the agent, handoff, and budget graph.
- `tools.rs` enforces scope, finish-bar, and per-item apply policies.
- `bridge.rs` is the scripted harness's HTTP MCP client.
- `runtime.rs` owns embedded-engine startup and event observation.
- `state.rs` records policy evidence; `workspace.rs` owns fixture copies.
