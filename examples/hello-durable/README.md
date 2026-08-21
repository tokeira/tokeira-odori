# hello-durable

One agent, one durable run, one embedded engine. The Codex CLI supplies the
subscription-authenticated model; tokeira owns the history and Odori runs the
harness turn as an activity.

```rust
use anyhow::Result;
use odori::{providers::CodexProvider, *};
use std::sync::Arc;
use tokeira_engine::Engine;

#[tokio::main]
async fn main() -> Result<()> {
    let engine = Engine::embedded().await?;
    let mut agents = AgentRegistry::new();
    agents.register(Agent::new("hello", "Answer clearly.").with_provider("codex"));
    let providers = Providers::new(Arc::new(CodexProvider::new()));
    let target = ConnectTarget::service_override(engine.service_override());
    let builder = OdoriRuntime::builder("hello-durable").connect(target);
    let runtime = builder.agents(agents).providers(providers).start().await?;
    let runner = runtime.runner();
    let answer: String = runner.run("hello", "Say hello.", "hello-1").await?;
    println!("{answer}");
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
