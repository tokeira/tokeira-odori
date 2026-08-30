# Budgets and handoffs

Budgets are workflow decisions. They use only turn activity results already
recorded in history, never a live provider query.

## Caps

`RunBudget` can cap:

- completed direct and delegated turns with `max_turns`;
- cumulative reported input plus output tokens with `max_total_tokens`;
- cumulative reported cost in US dollars with `max_cost_usd`.

An agent budget and a `RunConfig` budget compose by taking the stricter cap in
each dimension. The run loop checks before starting a turn and again after the
completed turn and any child spend have been recorded.

When a cap is exhausted or crossed, the workflow returns
`RunEnd::BudgetExceeded { cap }`. The cap records the spent value and the
configured limit. The runner exposes this as
`RunnerError::BudgetExceeded { partial }`, where the partial output includes
the transcript's final text, session ID, aggregate usage, turn count, and clean
terminal reason.

One in-flight aggregate turn may cross a token or cost cap before it can be
observed. The terminal recorded spend is therefore bounded by the cap plus that
one turn's aggregate usage, including retry carryover. A zero turn cap prevents
the first turn from starting.

## Unknown usage

Unknown usage is explicit, never zero. If either token figure for a turn is
unknown, `RunUsage::turns_with_unknown_tokens` increases and that turn does
not add a partial amount to the token total. Unknown cost similarly increases
`turns_with_unknown_cost`.

Every completed turn still counts against `max_turns`. If a backend cannot
report the dimension that must be bounded, use a turn cap as the hard limit.

Budgets enforce on cost and input + output tokens only. The finer
accounting a backend may report — cache splits, reasoning tokens, limit
and credit state — is recorded and rolled up for the operator but never
enters cap arithmetic; see [Usage and credits](usage-and-credits.md).

## Retries spend budget

Retries count because the vendor may already have generated tokens before an
attempt failed. Providers heartbeat cumulative usage snapshots as the turn
runs. A new activity attempt reads the previous attempt's final heartbeat and
folds that spend into its own successful `TurnOutcome::usage`.

The workflow therefore records one deterministic aggregate for the turn,
including failed-attempt spend. A rate limit received before any generation
heartbeat contributes zero incremental usage. Whole-loop retries in the raw API
tier re-spend model tokens; stable tool results replay from history.

## Handoffs

A `Handoff` is a child workflow, not an in-process function call. The parent
exposes it as a tool. When called, the workflow validates the target, calculates
the parent's remaining budget, starts the target agent's `AgentRun`, and waits
for its result. Handoffs are framework tools, so the `preview` MCP bridge must
be enabled for a harness to call them.

The child runs under the intersection of:

- the target agent's own caps; and
- the parent run's remaining caps.

The parent then absorbs the child's completed turns, tokens, cost, and unknown
usage counters. A nested agent cannot escape the parent's budget by delegating.
The child result is the handoff tool result, so the originating model sees a
normal tool success or error.

The [slice-fleet example](examples/slice-fleet.md) exercises both sides: worker
and reviewer handoffs count toward the orchestrator's total, while a child with
`max_turns = 0` reaches `BudgetExceeded` without starting a model turn.
