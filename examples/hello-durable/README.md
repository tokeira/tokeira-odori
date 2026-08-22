# hello-durable

One agent, one durable run, one embedded engine. The Codex CLI supplies the
subscription-authenticated model; tokeira owns the history and Odori runs the
harness turn as an activity.

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

Run it with an authenticated Codex CLI (this consumes quota):

```console
cargo run --manifest-path tests/embedded/Cargo.toml --example hello-durable
```

The fixed run id, `hello-1`, is the whole-run idempotency key. Reissuing it
joins the durable execution instead of starting duplicate work.
