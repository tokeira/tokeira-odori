# Observability

Odori instruments the agent layer with [OpenTelemetry GenAI semantic
convention](https://opentelemetry.io/docs/specs/semconv/gen-ai/) spans and
emits them through the [`tracing`](https://docs.rs/tracing) facade — and
does nothing else. No Odori crate depends on an OpenTelemetry SDK, opens a
network exporter, or installs a subscriber. The **host process owns the
pipeline**: install any `tracing` subscriber and the spans flow through
it; install none and the instrumentation is disabled at the callsite.
This is the same architecture the wider Rust agent ecosystem uses (Rig
instruments identically), so any OTLP-speaking backend works. [Pydantic
Logfire](https://pydantic.dev/logfire) is the path documented and
verified here.

## What Odori emits

One run produces one trace:

```text
invoke_agent day-planner            ← the run, opened by the Runner (client side)
├── chat gpt-5                      ← turn 0, opened by the turn activity
│   ├── harness tool use            ← event per backend-reported tool call
│   └── execute_tool save_note     ← durable framework-tool execution
└── chat gpt-5                      ← turn 1
```

Span names are the GenAI-convention operations. `tracing` span names are
static, so the display name (`invoke_agent day-planner`) rides the
`otel.name` field; `tracing-opentelemetry` (and the `logfire` crate,
which builds on it) exports it as the span name, exactly as the embedded
engine's own spans do.

| Span / event | Attributes set at creation | Recorded at completion |
| --- | --- | --- |
| `invoke_agent` (per run) | `gen_ai.operation.name`, `gen_ai.agent.name`, `odori.run.id` | `odori.run.input_tokens`, `odori.run.output_tokens`, `odori.run.cost_usd`, `odori.run.turns`, `odori.run.end` (`completed` \| `conversation_ended` \| `budget_exceeded` \| `guardrail_blocked`), `otel.status_code` |
| `chat` (per turn attempt) | `gen_ai.operation.name`, `gen_ai.system` **and** `gen_ai.provider.name` (both, for maximum backend compatibility), `gen_ai.request.model` (when the agent pins one), `gen_ai.agent.name`, `odori.run.id`, `odori.turn`, `odori.turn.attempt` | `gen_ai.conversation.id` (backend session id, recorded as soon as the provider reports it), `gen_ai.usage.input_tokens`, `gen_ai.usage.output_tokens`, `operation.cost`, `otel.status_code`, `error.type` (the [`TurnError`](providers.md#error-surfaces) class) on failure |
| `execute_tool` (per durable tool execution) | `gen_ai.operation.name`, `gen_ai.tool.name`, `gen_ai.tool.call.id`, `gen_ai.agent.name`, `odori.run.id`, `odori.turn`, `odori.turn.attempt` | `odori.tool.is_error` (model-visible tool failures are successful executions of a failing tool), `otel.status_code`, `error.type` |
| `harness tool use` (event) | `gen_ai.tool.name` — the backend invoked a native or MCP tool mid-turn; harnesses report the fact, not the duration | — |

`operation.cost` is the provider-reported dollar cost (Logfire's cost
convention). Usage attributes are recorded only when the backend reported
them — an unknown figure is absent, never zero, matching the run's
[budget accounting](budgets-and-handoffs.md).

Vendor accounting conventions (`gen_ai.usage.*`, `operation.cost`)
appear **only on turn spans**, where the spend occurs: backends
aggregate them across a trace, so the run span's rollup rides
odori-namespaced attributes instead. This is a measured fix — with the
rollup also on the root, Logfire's trace cost chip displayed exactly
double the real spend (observed 2026-08-30).

## Redaction

Redacted by default, and not un-redactable by flag: prompts, turn text,
tool arguments, and tool results are **never** attached to Odori's spans.
What is attached — names, identifiers, token/cost accounting, error
classes — is information the run already records durably.

The substrate below Odori is chattier. The embedded engine and the
Temporal SDK emit their own diagnostic spans, and at `info` and below
some SDK span fields include **serialized activity payloads — prompts
included**. A subscriber's filter is therefore a data-egress decision,
not a cosmetic one. The redacting default the Logfire example uses:

```text
RUST_LOG='warn,odori_agents=info'
```

exports the complete agent trace tree plus warnings from everything
else. Raise the substrate targets (`tokeira_*`, `temporalio_*`) into an
exporter only when you understand what leaves the process.

## Sending traces to Logfire

The shortest path is Pydantic's first-party [`logfire`
crate](https://docs.rs/logfire) in the host binary:

```rust,ignore
fn main() -> anyhow::Result<()> {
    let logfire = logfire::configure()
        .with_service_name("my-odori-host")
        .finish()?;                       // reads LOGFIRE_TOKEN, installs the
                                          // global tracing subscriber
    // ... build the engine, runtime, and runner; execute runs ...
    logfire.shutdown()?;                  // flush before exit
    Ok(())
}
```

Set `LOGFIRE_TOKEN` to a write token from your Logfire project settings
(env-only — never in code or configuration files). The data region is
parsed from the token itself; `LOGFIRE_BASE_URL` overrides it for
self-hosted deployments. The crate reads its export filter from
`RUST_LOG` and defaults to `trace` — set the redacting filter above
unless you have decided otherwise.

Hosts that already own an OpenTelemetry stack need no Logfire-specific
integration: layer `tracing-opentelemetry` over an OTLP exporter (build
`opentelemetry-otlp` with its `reqwest-rustls` feature, matching this
workspace's rustls-everywhere posture) and aim it with the standard
variables, per [Logfire's alternative-clients
guide](https://pydantic.dev/docs/logfire/guides/alternative-clients/):

```text
OTEL_EXPORTER_OTLP_ENDPOINT=https://logfire-us.pydantic.dev   # or -eu
OTEL_EXPORTER_OTLP_HEADERS='Authorization=<write token>'
OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf
OTEL_SERVICE_NAME=my-odori-host
```

The [logfire example](examples/logfire.md) runs a scripted two-turn
conversation with one durable tool call against the real embedded engine
and exports the trace above; its integration test asserts the exact tree
and attribute set, and that no content string reaches any span field.

## Scope and limits, measured

Claims here were verified on 2026-08-30, three ways: `tracing-subscriber`
capture in-process (the unit and integration tests in-tree assert the
tree, attributes, accounting, and content redaction), the Logfire
platform docs current that day, and a live export of the
[logfire example](examples/logfire.md)'s trace confirmed in the Logfire
Live view — one `invoke_agent day-planner` root with its four nested
rows and a cost chip. That live check is also what caught the
double-counted cost chip fixed by the turn-spans-only accounting rule
above.

- **Cross-process parenting.** Turn and tool spans adopt the run span
  through a process-local registry. In Odori's flagship embedded mode —
  runner, engine, and worker in one process — the full tree assembles.
  Against a remote engine with a detached worker the lookup misses and
  turn/tool spans export as trace roots; `odori.run.id` on every span
  still correlates them by attribute. Odori deliberately persists no
  trace context into workflow history.
- **Handoff child runs** are their own trees today: the child records
  its turns under its own workflow id (which embeds the parent's), but
  no `invoke_agent` span wraps a child run and no parent link crosses
  the child-workflow boundary.
- **No workflow-side spans.** The run loop's workflow code emits
  nothing: replayed workflow tasks re-execute code, and spans emitted
  there would duplicate on every replay. Spans come from the client and
  from activities, which execute exactly once per attempt.
- **Spans only.** Odori emits no OpenTelemetry metrics or logs of its
  own; the accounting on spans (tokens, cost) is queryable in Logfire
  directly.
- **A conversation's run span** lives in the process that called
  [`start_conversation`](primitives.md); a conversation reattached with
  `resume_conversation` in another process follows the run without one.
