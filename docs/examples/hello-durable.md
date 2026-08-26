# hello-durable

## What it demonstrates

One `CodexProvider`, one agent, one non-interactive durable run, and one
embedded tokeira engine. The run ID `hello-1` is the whole-run idempotency
key. The worker and engine communicate in process.

The example uses the real Codex subscription harness and therefore requires
the pinned, authenticated CLI described in the [provider guide](../providers.md#codex-subscription).

## Run it

```console
cargo run --manifest-path tests/embedded/Cargo.toml --example hello-durable
```

The optional `-- --storage <mode>` flag selects one of the three modes listed
in the [examples index](README.md#storage-mode-flag).

This consumes Codex quota. The complete compiled source is
[`examples/hello-durable/main.rs`](../../examples/hello-durable/main.rs), and
the root README copies it verbatim as the quickstart.

## Read the output

The printed line is the typed `String` returned by
`Runner::run("hello", "Say hello.", "hello-1")`. Before it prints, the engine
has recorded the completed turn and its provider session ID. The orderly
shutdown first drains the Odori worker and then stops the engine.
