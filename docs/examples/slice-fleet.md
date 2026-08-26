# slice-fleet

## What it demonstrates

This is a working miniature of the fleet lifecycle that built Odori. It copies
a bundled Rust library into fresh temporary worker and integration trees. The
fixture begins with a failing `increment` test and a missing `double`
feature.

The orchestrator first produces a typed `SlicePlan`. A human approval signal
opens the plan, then each slice runs through a child-workflow worker. Policies
are executable:

- each worker's `scope_write` tool rejects writes outside its declared file
  list as a model-visible error;
- `cargo check --locked` and a targeted test run as durable finish-bar tools,
  and review starts only after their results are in history;
- Claude-authored work is reviewed by a Codex-scripted agent and Codex-authored
  work by a Claude-scripted agent;
- every apply step has its own human approval signal and ledger check;
- one child has `max_turns = 0` and ends as `BudgetExceeded`;
- one child returns the typed `Raise` variant to the approval seat rather
  than stretching a frozen contract.

The model behavior is scripted, but the embedded engine, signals, workflow
updates, HTTP MCP bridge, tool activities, retries, child workflows, and budget
accounting are real.

## Run it

```console
cargo run --manifest-path tests/embedded/Cargo.toml --example slice-fleet
```

The optional `-- --storage <mode>` flag selects one of the three modes listed
in the [examples index](README.md#storage-mode-flag).

The deterministic path consumes no subscription quota.

## Read the output

`PLAN` is the typed plan before approval. `SCOPE FENCE` lines show the
deliberate out-of-scope `Cargo.toml` calls being rejected. `FINISH BAR` and
`HOSTILE REVIEW` are history-backed evidence, not decorative labels.
`BUDGET` and `RAISE` are typed clean outcomes routed back to the approval
seat. Separate `HUMAN APPROVAL` lines precede each apply.

The final `GREEN` line reports the two applied slices, eleven parent-accounted
turns (six direct and five delegated), and 550 tokens (440 input plus 110
output). The full captured transcript and module map live in the
[example README](../../examples/slice-fleet/README.md).
