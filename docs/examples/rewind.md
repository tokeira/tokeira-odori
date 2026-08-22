# rewind

## What it demonstrates

The first recovery path makes a scripted harness call the durable
`checkpoint` tool and then die. Activity retry presents
`stable-checkpoint-call` again. The invocation registry returns the result
already in workflow history, so that tool handler executes once.

The stronger path stops worker A after the same checkpoint while keeping the
embedded engine alive. Before worker B starts, the example reads durable state
to prove the activity has reached retry attempt 2. Worker B uses the SDK's
default workflow cache and 10-second sticky schedule-to-start fallback; it must
receive the attempt and complete within 15 seconds.

Finally, one completed deliberation is serialized as immutable input to two new
durable runs. One timeline chooses `ship`, the other `hold`, and both retain
the same plan hash and checkpoint receipt. This is rewind and fork from one
deliberation; it is not an engine snapshot file.

## Run it

```console
cargo run --manifest-path tests/embedded/Cargo.toml --example rewind
```

The scripted provider consumes no subscription quota.

## Read the output

`RESUME EXACTLY` reports attempts 1 and 2 with the stable call ID and one tool
execution. `DURABLE` proves retry state before replacement; `RESTART RESUME`
proves worker B received that attempt; `STICKY FALLBACK` reports bounded
completion under default cache settings. `PRESENTATIONS` covers both
independent recovery runs, so two total checkpoint executions are expected.
`TIMELINE A` and `TIMELINE B` share the snapshot content and differ only in
the human decision.

The exact assertions and a focused regression command are in the
[example README](../../examples/rewind/README.md).

