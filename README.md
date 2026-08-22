# Odori

A minimal Rust agent framework with durable execution built in.

Odori borrows the OpenAI Agents SDK's primitive set — `Agent`, `Runner`,
`Tool`, `Handoff`, `Guardrail`, typed outputs — and runs it on an embedded
[tokeira](https://github.com/tokeira/tokeira) engine: the run loop is a
workflow, each harness turn is an activity, sessions are history, handoffs are
child workflows. Providers are subscription-first: headless Claude Code and
the Codex app-server, driven as supervised subprocesses.

**Status: pre-release scaffold.** The workspace below is seeded; the
primitives, providers, engine assembly, and MCP bridge land before the public
v0. Full positioning, quickstart, and provider setup docs arrive with them.

## Quickstart

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

See [`examples/hello-durable`](examples/hello-durable),
[`examples/slice-fleet`](examples/slice-fleet),
[`examples/rewind`](examples/rewind), and
[`examples/approval-resume`](examples/approval-resume) for runnable programs
and captured output. The examples are Cargo targets of the workspace-excluded embedded
integration package, preserving the product workspace's dependency boundary.

| Crate | Owns |
| --- | --- |
| [`odori`](crates/odori) | The facade — the one name a quickstart depends on |
| [`odori-agents`](crates/odori-agents) | Primitives: `Agent`, `Runner`, `Tool`, `Handoff`, `Guardrail`, typed outputs, sessions |
| [`odori-providers`](crates/odori-providers) | Supervised vendor harnesses (Claude Code, Codex); raw APIs behind features |
| [`odori-engine`](crates/odori-engine) | Embedded tokeira + Temporal Rust SDK worker bootstrap |
| [`odori-mcp-bridge`](crates/odori-mcp-bridge) | Framework-owned tools as durable activities, mid-turn |

## License

[Apache-2.0](LICENSE).
