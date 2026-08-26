# rewind

This demo makes the durability claim visible instead of asking you to trust
it. The scripted harness calls a durable `checkpoint` tool and then dies at a
fixed point. Its activity retry presents `stable-checkpoint-call` again; the
workflow registry returns the result already in history, so the handler's
execution count remains one.

## Run the example

Run the complete deterministic demonstration with:

```console
cargo run --manifest-path tests/embedded/Cargo.toml --example rewind
```

Select storage with `--storage in-memory` (the default), `--storage
managed-dsql`, or `--storage adopt-existing-endpoint`. For example:

```console
ODORI_DSQL_REGION=us-east-1 \
ODORI_DSQL_DESCRIPTOR_PATH=/operator-owned/path/rewind-cluster.json \
cargo run --manifest-path tests/embedded/Cargo.toml --example rewind -- \
  --storage managed-dsql
```

In a DSQL mode, the canary stops the first engine after observing durable
attempt 2, starts a new engine over the same database, and only then starts
worker B. The initial and restarted cluster/schema reports and measured startup
times are printed. Managed engine shutdown releases ownership but does not
delete the cluster; the descriptor is retained for explicit administrative
[teardown](../../docs/dsql-clusters.md#delete-a-cluster). The adopted mode uses
the exact `ODORI_DSQL_*` identity supplied by the operator and performs no
cluster lifecycle mutation.

This starts the embedded engine, exercises exact retry and worker-replacement
recovery, then creates the two divergent timelines. Its provider is scripted,
so the command consumes no subscription quota.

It then runs the stronger replacement canary. Worker A stops after an
identical checkpoint. In memory, the embedded engine stays alive; in either
DSQL mode it is shut down and replaced. Before worker B is
started, the demo uses `DescribeWorkflowExecution` to prove that the failed
activity advanced durably to retry attempt 2; this avoids confusing a shutdown
race with a matching or sticky-routing defect. Worker B must receive that
attempt, re-present the same call id without executing the checkpoint again,
and complete the workflow within the 15-second bound that covers the SDK's
default 10-second sticky schedule-to-start fallback.

The canary keeps the SDK defaults (`max_cached_workflows = 1000` and a
10-second sticky fallback); it does not hide recovery behavior by setting
`max_cached_workflows(0)`. The in-memory path is graceful worker replacement
over one engine. The DSQL path is graceful worker and engine replacement; it
is not an operating-system process-kill test.

The replacement-worker path runs unguarded. Failure to resume the durable
attempt is an example error; there is no diagnostic fallback branch that lets
the demo continue without proving recovery.

The second half treats the completed deliberation as an immutable, serialized
snapshot. Two new durable runs receive byte-equivalent snapshot configuration
but different human decisions. One timeline ships and the other holds; both
retain the same plan hash and checkpoint receipt.

Typical output:

```text
RESUME EXACTLY: attempts 1 and 2 presented stable-checkpoint-call; tool executions=1
KILL: harness exited 137 after the restart canary checkpoint
STOP: worker A drains and stops; embedded engine stays alive
DURABLE: engine reports activity attempt 2 before worker B starts
RESTART: replacement worker uses default workflow cache settings
RESTART RESUME: replacement polled attempt 2
STICKY FALLBACK: workflow completed on worker B in 159.19775ms after retry presentation
PRESENTATIONS: [(1, "stable-checkpoint-call"), (2, "stable-checkpoint-call"), (1, "stable-checkpoint-call"), (2, "stable-checkpoint-call")]; total checkpoint executions=2
TIMELINE A: {"decision":"ship","plan_hash":"plan-v1-bugfix-feature-budget-contract","result":"ship from checkpoint:plan-ready:stable-checkpoint-call:execution-1"}
TIMELINE B: {"decision":"hold","plan_hash":"plan-v1-bugfix-feature-budget-contract","result":"hold from checkpoint:plan-ready:stable-checkpoint-call:execution-1"}
```

## Regression tests

There is no special recovery branch in the worker. Session recovery comes
from heartbeat details and tool dedupe comes from workflow history. The test
at `tests/embedded/tests/examples.rs` makes each recovery boundary observable:
durable retry state before replacement, activity delivery to worker B, sticky
workflow completion, and exactly-once checkpoint execution. The two timelines
are restored from the completed first deliberation and then diverge.

Both `rewind_resumes_exactly_and_diverges_timelines` and the focused
`rewind_survives_worker_replacement_with_default_cache` regression run
unguarded. Run the focused test with:

```console
cargo test --manifest-path tests/embedded/Cargo.toml --test examples rewind_survives_worker_replacement_with_default_cache --locked -- --exact
```

## Code structure

The example owns its complete implementation under `rewind/scenario/`:

- `mod.rs` owns the retry, worker-replacement, and divergence lifecycle.
- `model.rs` owns the deliberation snapshot and timeline inputs.
- `provider.rs` scripts failure, retry, and timeline behavior.
- `bridge.rs` re-presents stable MCP tool-call identities.
- `runtime.rs` assembles the embedded engine, agents, and checkpoint tool.
- `observation.rs` reads durable activity attempts; `state.rs` records evidence.
