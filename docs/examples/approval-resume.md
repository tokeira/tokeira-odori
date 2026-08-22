# approval-resume

## What it demonstrates

This is the disk-persistence and human-in-the-loop example. Process one starts
an interactive workflow over a failing Rust fixture. The scripted provider may
only propose a typed patch containing its file scope, exact before and after
bytes, finish bar, and plan hash. Once that turn is queryable from workflow
history, the process writes `approval-request.json`, shuts down the worker,
writes the embedded engine to `engine.snapshot`, and exits while the workflow
is still waiting for input.

Process two restores the engine from that snapshot, starts a replacement
worker, reattaches to the same run ID, verifies the restored proposal, and
records the human-approved hash as the next message. The durable apply tool
checks the approval receipt, exact path, and reviewed bytes before changing the
fixture. A durable `cargo test --locked` finish bar must then pass.

This is the embedded development persistence story. It does not replace the
normal durable storage contract of a production deployment.

## Run process one

Keep one temporary directory alive across both commands:

```console
state_directory="$(mktemp -d "${TMPDIR:-/tmp}/odori-approval-resume.XXXXXX")"
cargo run --manifest-path tests/embedded/Cargo.toml --example approval-resume -- \
  prepare "$state_directory"
```

The program prints the approval request and snapshot paths, then exits. Review
`approval-request.json`, including the exact replacement bytes. The fixture
is still failing at this point.

## Restore and complete

```console
cargo run --manifest-path tests/embedded/Cargo.toml --example approval-resume -- \
  resume "$state_directory" --approve plan-v1-fix-increment
rm -r "$state_directory"
```

The scripted provider consumes no subscription quota. The final removal is a
shell cleanup after the completed demonstration; omit it if you want to inspect
the persisted workspace and snapshot.

## Read the output

Process one prints `HUMAN APPROVAL REQUIRED`, `REQUEST`,
`SNAPSHOT WRITTEN`, and a line confirming that it exits with the workflow
live. Process two prints `RESTORED`, `HUMAN APPROVAL RECORDED`,
`APPLIED ONCE`, `GREEN`, and the terminal workflow line.

The integration test launches the two stages as separate operating-system
processes and asserts the unchanged pre-approval fixture, non-empty snapshot,
restored transcript and session lineage, exactly-once apply, exactly-once finish
bar, persisted code change, and terminal workflow. The full command and module
map are in the [example README](../../examples/approval-resume/README.md).

