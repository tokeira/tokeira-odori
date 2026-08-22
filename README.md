<p align="center">
  <img src="assets/odori.png" alt="Odori Bird" width="320">
</p>

# Odori

[![Stopgap gates](https://github.com/tokeira/tokeira-odori/actions/workflows/ci.yml/badge.svg)](https://github.com/tokeira/tokeira-odori/actions/workflows/ci.yml)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

A minimal Rust agent framework where every run is durably executed.

Odori has five primitives in the OpenAI Agents SDK's image: `Agent`, `Runner`,
`Tool`, `Handoff`, and `Guardrail`, plus typed outputs. They run on an embedded
[tokeira](https://github.com/tokeira/tokeira) engine: the run loop is a
workflow; a turn is an activity; the session is history. Subscription providers
drive Claude Code or Codex through the vendors' own authenticated harnesses;
raw API providers form a secondary tier. Framework tools execute durably
mid-turn through the MCP bridge. Crash anywhere and resume exactly. Snapshot a
world, then rewind and fork. `cargo run` is the whole install.

## Quickstart

This is the complete [`hello-durable`](examples/hello-durable/main.rs) example:

```rust
use std::sync::Arc;

use anyhow::Result;
use odori::{
    Agent, AgentRegistry, ConnectTarget, OdoriRuntime, Providers, providers::CodexProvider,
};
use tokeira_engine::Engine;

#[tokio::main]
async fn main() -> Result<()> {
    // Start the local durable engine that owns this run's history.
    let engine = Engine::embedded().await?;

    // Register an agent against the authenticated Codex subscription provider.
    let mut agents = AgentRegistry::new();
    agents.register(Agent::new("hello", "Answer clearly.").with_provider("codex"));
    let providers = Providers::new(Arc::new(CodexProvider::new()));

    let runtime = OdoriRuntime::builder("hello-durable")
        .connect(ConnectTarget::service_override(engine.service_override()))
        .agents(agents)
        .providers(providers)
        .start()
        .await?;

    // The stable run ID is the idempotency key for the whole execution.
    let answer: String = runtime
        .runner()
        .run("hello", "Say hello.", "hello-1")
        .await?;
    println!("{answer}");

    // Drain the worker before stopping the engine it is connected to.
    runtime.shutdown().await?;
    engine.shutdown().await
}
```

Clone the repository, authenticate the Codex CLI as described below, then run:

```console
cargo run --manifest-path tests/embedded/Cargo.toml --example hello-durable
```

The fixed run ID, `hello-1`, is the idempotency key for the complete durable
execution. Starting it again joins the existing run instead of duplicating it.

## Five primitives

- **`Agent`** names the instructions, provider, model, tools, handoffs,
  guardrails, output shape, and budget for one role.
- **`Runner`** starts a run or conversation and returns a typed result. A run
  ID identifies the durable workflow.
- **`Tool`** gives a model a framework-owned capability. Under the `preview`
  bridge, each call runs as an activity with explicit timeout and retry policy.
- **`Handoff`** exposes another registered agent as a tool. The target runs as
  a child workflow and its spend also counts against the parent.
- **`Guardrail`** is a deterministic input or output check. Budget caps are
  enforced from usage already recorded in workflow history.

Typed outputs turn final text into `String` or `Json<T>` at the runner boundary;
providers also receive the agent's JSON Schema when one is configured. See the
[primitives walkthrough](docs/primitives.md).

## Providers

Subscription-backed harnesses are the primary provider tier. The CLI is a
runtime dependency of the worker process, and Odori warns when the installed
version drifts from the version tested here.

### Claude Code

Install Claude Code by following the
[official quickstart](https://code.claude.com/docs/en/quickstart), then
authenticate once and verify the installed version:

```console
claude login
claude --version
```

The Claude provider depends on **Claude Code 2.1.220 or newer**; 2.1.220 is
the conformance baseline. On a headless machine, `claude setup-token` is the
supported alternative authentication flow. Register `ClaudeProvider::new()`
under provider name `claude`.

### Codex

Install the Codex CLI by following the
[official getting-started guide](https://learn.chatgpt.com/docs/codex/cli#getting-started),
then authenticate once and verify the installed version:

```console
codex login
codex login status
codex --version
```

The Codex provider depends on **Codex CLI 0.148.0-alpha.15 or newer**; that
version is the conformance baseline. Register `CodexProvider::new()` under
provider name `codex`.

Subscription rate limits and quota windows are the tier's weather. Odori
classifies retryable API and rate-limit failures for activity backoff, but it
cannot make a vendor window reopen. Claude usage-cap exhaustion is surfaced as
an actionable terminal configuration error; Codex quota failures exhaust the
configured activity attempts rather than hanging. Missing CLIs and missing
authentication fail immediately with the install or login command to run.

### Raw APIs

The Anthropic Messages and OpenAI Responses providers are a secondary tier,
off by default. Enable `api-anthropic`, `api-openai`, or both on the `odori`
dependency and set `ANTHROPIC_API_KEY` or `OPENAI_API_KEY` in the worker
process's environment. Keys are never read from a repository file.

API-tier whole-loop retry re-spends model tokens while already-recorded tool
results replay from history. Anthropic multi-turn state is
continuity-within-process. OpenAI continues with `previous_response_id`, so its
continuity lasts only as long as the vendor retains that response chain. The
subscription harness tier is the durable multi-turn path.

The [provider guide](docs/providers.md) covers registration, authentication,
session boundaries, and every public error class.

## Durability

The embedded engine and worker communicate in process: no TCP, no ports, no
daemon. Completed turns, session lineage, usage, handoffs, tool admissions, and
tool results are history. Process failure therefore resumes the workflow from
recorded state rather than rebuilding an agent loop from logs.

- [`rewind`](examples/rewind) kills a scripted harness at a fixed tool call,
  proves registry replay across retry and default-cache worker replacement,
  then sends one serialized deliberation into two different timelines.
- [`approval-resume`](examples/approval-resume) stops at a human approval
  boundary, writes an embedded-engine snapshot to disk, exits, restores it in
  another process, and completes the live workflow.
- [`slice-fleet`](examples/slice-fleet) runs typed planning, approval gates,
  child-workflow workers, scoped tools, finish bars, budgets, cross-provider
  review, and stop-and-raise against a real fixture project.

Start with the [examples index](docs/examples/README.md).

## Durable tools (`preview`)

The optional `preview` feature starts one loopback streamable-HTTP MCP bridge
per process and attaches it to each harness turn that has framework tools. A
tool call becomes a workflow update and then a tool activity. The bridge waits
for the result to be recorded before replying, replays completed calls with the
same identity, joins duplicate in-flight calls, and fences stale attempts.

The identity guarantee is deliberate but bounded: the registry deduplicates a
known `(turn, call_id)`. A harness that generates a new call ID is a new
invocation, so side-effecting tool handlers must also use the supplied
`ToolContext` identity as their idempotency key. The [durable-tools
guide](docs/durable-tools.md) describes the full contract and the API tier's
MCP-client path.

## Odori and tokeira

Odori is the dance, tokeira is the engine it dances on.

Odori owns the agent vocabulary and provider integrations. The
[tokeira engine](https://github.com/tokeira/tokeira) owns durable workflow
execution, history, retries, child workflows, signals, updates, snapshots, and
the in-process Temporal-compatible service used by `OdoriRuntime`.

## Roadmap

The v0 surface is subscription-first providers, the five primitives, embedded
durable runs, budgets and handoffs, and runnable recovery examples. Durable
framework tools currently ship behind `preview` while their harness protocol
edges continue to soak. The raw API providers remain the secondary,
feature-gated tier with the continuity boundaries stated above.

The next stabilization work is to graduate the MCP bridge from `preview`,
expand provider conformance coverage as harness pins move, and grow the
curated validator library without changing the deterministic `Guardrail`
contract.

## Licence

[Apache-2.0](LICENSE).
