# logfire

One scripted two-turn conversation with one durable tool call, exported
to [Pydantic Logfire](https://pydantic.dev/logfire) as a single trace:

```text
invoke_agent day-planner
├── chat logfire-scripted        (turn 0 — session, tokens, cost recorded)
│   ├── harness tool use         (event: save_note)
│   └── execute_tool save_note   (durable execution via the mcp-bridge)
└── chat logfire-scripted        (turn 1)
```

The provider is scripted, so the run is deterministic and consumes no
subscription quota; the engine, the run-loop workflow, the turn
activities, and the durable tool execution are all real. The host wiring
is the part the example exists to demonstrate — Pydantic's first-party
[`logfire` crate](https://docs.rs/logfire) plus the redacting export
filter, exactly as documented in [Observability](../observability.md):

```rust,ignore
let logfire = logfire::configure()
    .with_service_name("odori-logfire-example")
    .finish()?;                            // reads LOGFIRE_TOKEN
let report = runtime.block_on(scenario::run_scripted_conversation(storage));
logfire.shutdown()?;                       // flush before exit
```

Run it with a write token from your Logfire project's settings:

```console
LOGFIRE_TOKEN=<write token> \
cargo run --manifest-path tests/embedded/Cargo.toml --example logfire
```

The token is required — without it the example refuses to run rather
than "succeed" while exporting nothing. The data region is parsed from
the token. When `RUST_LOG` is unset the example seeds the redacting
default `warn,odori_agents=info`, so the exported trace is the agent
tree above and nothing below it; see
[Observability § Redaction](../observability.md#redaction) before
widening that filter.

The executable also accepts `--storage in-memory` (default),
`--storage managed-dsql`, or `--storage adopt-existing-endpoint`, with
the `ODORI_DSQL_*` environment listed in the
[storage-mode table](README.md#storage-mode-flag).

The integration test `tests/embedded/tests/telemetry.rs` runs the same
scenario with a capturing subscriber standing where the exporter stands,
and asserts the tree's parenting, the GenAI attributes, the usage and
cost figures on every span, and that no prompt or tool content appears
in any span field.
