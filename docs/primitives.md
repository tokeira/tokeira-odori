# Primitives

Odori has five primitives and typed outputs. Configuration lives in ordinary
Rust; execution state lives in workflow history.

## Agent

An `Agent` is a named configuration. It owns instructions and may select a
provider, model, and [reasoning effort](providers.md#reasoning-effort),
expose tools and handoffs, add input and output guardrails, declare an
output schema, and set a budget.

The worker resolves agents by name from an `AgentRegistry`. Agent objects are
not serialized into workflow inputs because they contain live tool handlers and
guardrail implementations. Changing an agent's deterministic behavior while its
runs are live has the same replay risk as changing workflow code; use a new
agent name or drain those runs first.

The smallest registration is the one in
[`hello-durable`](../examples/hello-durable/main.rs):

```rust
let mut agents = AgentRegistry::new();
agents.register(Agent::new("hello", "Answer clearly.").with_provider("codex"));
```

This fragment is compiled as part of the example target.

## Runner

A `Runner` starts and follows `AgentRun` workflows. `run` executes one
non-interactive turn and parses the requested output type. `run_with_config`
adds explicit budgets and turn timeouts. `start_conversation` creates a live
interactive workflow; `resume_conversation` rebuilds a process-local handle
for an existing run ID.

The run ID is the workflow ID and the idempotency key for the whole run.
Reissuing it joins the existing execution instead of creating duplicate work.
An interactive caller may query `transcript`, send the next user message, and
end the conversation. A later process can restore the engine, construct a
runner for the same task queue, call `resume_conversation`, and continue the
recorded workflow.

See [approval-resume](examples/approval-resume.md) for that process boundary.

## Tool

A `Tool` has a model-visible name and description, a JSON Schema for its
arguments, an async handler, and a `ToolPolicy`. The policy controls the tool
activity's start-to-close timeout, optional schedule-to-close and heartbeat
timeouts, and maximum attempts.

With the `preview` bridge enabled, the handler receives a `ToolContext` with
the workflow run ID, turn, turn-attempt number, and harness invocation ID. Use
that identity as the idempotency key for external side effects. A retryable
`ToolFailure` follows the activity retry policy; a terminal failure is
returned to the model without another attempt. Exhausted retries also become a
model-visible tool error rather than silently killing the turn.

With `preview` disabled, the bridge is inert and a registered handler is not
executed. Harness-native tooling remains available through
`Agent::with_allowed_native_tools`. Read [durable tools](durable-tools.md)
before enabling framework-owned tools.

## Handoff

A `Handoff` exposes another registered agent as a framework tool. When the
model calls it, the current `AgentRun` starts the target agent's own
`AgentRun` as a child workflow, passes the model's `input` string as handoff
context, and waits for the child before answering the tool call.

The `preview` MCP bridge is the path that makes this framework tool reachable
inside a harness turn. With the bridge disabled, no handoff handler is invoked.

The default tool name is `transfer_to_<normalized-target>`; callers may
override the name and description. The target must be registered on the same
worker. Child budgets and parent accounting are described in
[budgets and handoffs](budgets-and-handoffs.md).

## Guardrail

A `Guardrail` is a synchronous check over text. Input guardrails run before
the first turn and therefore before model spend. Output guardrails run after
each completed turn. `Pass` continues; `Block { reason }` ends the workflow
with `RunEnd::GuardrailBlocked` and does not retry the turn.

Guardrails execute inside workflow code. They must be deterministic: no I/O,
clock reads, randomness, provider calls, or mutable process state. A check that
needs any of those belongs in an activity or tool, not in `Guardrail::check`.
`RunBudget` is a separate deterministic policy evaluated from usage already
recorded in activity results.

## Typed outputs

`Runner::run::<String>` returns the final text unchanged.
`Runner::run::<Json<T>>` deserializes final text into any
`serde::Deserialize` type `T`. A parse failure is
`RunnerError::Output` and retains the raw text for diagnosis.

`Agent::with_output_schema` also sends a JSON Schema to providers that can
enforce it. Runner-side parsing remains the final type boundary either way.
The fleet example parses its first durable result as
[`Json<SlicePlan>`](../examples/slice-fleet/scenario/mod.rs) before opening
the approval seat.

## Run outcomes

A successful non-interactive run ends as `RunEnd::Completed`; ending an
interactive conversation produces `ConversationEnded`. Budget and guardrail
termination are clean workflow results, mapped by the runner to
`RunnerError::BudgetExceeded` or `RunnerError::GuardrailBlocked`.
Provider, activity, or client failures surface as `RunnerError::Run` or
`RunnerError::Client`; typed parse errors remain distinct.
