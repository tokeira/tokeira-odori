# logfire

The odori → Pydantic Logfire path, end to end: a scripted two-turn
conversation with one durable tool call runs against the real embedded
engine, and the host exports the resulting GenAI-convention trace —
`invoke_agent` → `chat` per turn → `execute_tool`, with token, cost, and
session accounting on every span — using Pydantic's first-party
[`logfire` crate](https://docs.rs/logfire).

Run it with a write token from your Logfire project's settings
(env-only, per the repo's no-secrets rule):

```console
LOGFIRE_TOKEN=<write token> \
cargo run --manifest-path tests/embedded/Cargo.toml --example logfire
```

Then open the project's Live view: the trace is named
`invoke_agent day-planner`. The example refuses to run without the
token, and when `RUST_LOG` is unset it seeds the redacting export filter
`warn,odori_agents=info` — Odori's spans carry identifiers and
accounting but never prompts, turn text, or tool content, while the
substrate's diagnostic spans (which can embed payloads at `info` and
below) stay out of the export. The full attribute reference, the raw
OTLP alternative to the `logfire` crate, and the measured limits live in
[docs/observability.md](../../docs/observability.md).

`--storage in-memory` (default), `--storage managed-dsql`, and
`--storage adopt-existing-endpoint` select the engine's storage mode, as
in the [storage-mode table](../../docs/examples/README.md#storage-mode-flag).
