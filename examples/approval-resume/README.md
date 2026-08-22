# approval-resume

This example stops a live agent workflow at a real human approval boundary,
writes the embedded engine's complete state to disk, exits the process, and
finishes from that file in a new process.

This is deliberately the embedded/dev-tier persistence story. Production
deployments retain their normal durable storage contract; the example does not
present a local snapshot file as a replacement for that storage.

The fixture is a tiny Rust library with one failing test. The first turn may
only propose a typed patch: its file scope, exact before/after bytes, finish
bar, and plan hash are recorded in workflow history. Once that record is
queryable, the program writes a human-readable `approval-request.json`, shuts
down the worker, and gracefully shuts down the embedded engine. The latter
atomically writes `engine.snapshot`; the workflow remains live and waiting for
input.

Run process one with a new state directory:

```console
cargo run --manifest-path tests/embedded/Cargo.toml --example approval-resume -- \
  prepare /tmp/odori-approval-demo
```

It exits after output like:

```text
HUMAN APPROVAL REQUIRED: plan-v1-fix-increment
REQUEST: /tmp/odori-approval-demo/approval-request.json
SNAPSHOT WRITTEN: /tmp/odori-approval-demo/engine.snapshot (... bytes)
PROCESS ... EXITING WITH LIVE WORKFLOW approval-resume-run
```

Review the JSON request, including its exact `after` bytes, then make the
human decision explicit in process two:

```console
cargo run --manifest-path tests/embedded/Cargo.toml --example approval-resume -- \
  resume /tmp/odori-approval-demo --approve plan-v1-fix-increment
```

The second process starts a new embedded engine from `engine.snapshot`, starts
a replacement Odori worker on the same task queue, and reattaches to
`approval-resume-run`. It queries the restored transcript before signalling
the approval. The approval is therefore a new history event after the
snapshot, not configuration smuggled into a fresh run.

The apply tool enforces three independent conditions: the process received the
human-approved hash, the requested path is exactly `src/lib.rs`, and the bytes
match the reviewed proposal. It then runs `cargo test --locked` as a durable
tool. Stable tool-call ids plus the invocation registry make both effects
exactly once if the restored turn is retried.

Typical completion output:

```text
RESTORED: /tmp/odori-approval-demo/engine.snapshot with one recorded proposal turn
HUMAN APPROVAL RECORDED: plan-v1-fix-increment
APPLIED ONCE: src/lib.rs
GREEN: cargo test --locked
PROCESS ... COMPLETED WORKFLOW approval-resume-run
```

The unguarded integration test invokes the prepare and resume stages through
two separate OS processes. It asserts that the first process leaves a
non-empty snapshot and an unchanged failing fixture, and that the second
restores the exact proposal, forks the provider lineage from its recorded
session, records two turns, applies once, runs the finish bar once, and reaches
a terminal workflow result.

```console
cargo test --manifest-path tests/embedded/Cargo.toml --test examples \
  approval_resume_crosses_a_process_boundary --locked -- --exact
```

The scripted provider consumes no subscription quota. Its shape deliberately
matches the live provider contract; a real-provider variant can be added
behind the existing quota gate without weakening this deterministic path.
