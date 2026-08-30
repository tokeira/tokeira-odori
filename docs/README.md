# Documentation

Odori is a minimal Rust agent framework where every run is durably executed.
Use these guides after the root [quickstart](../README.md#quickstart).

- [Primitives](primitives.md) explains `Agent`, `Runner`, `Tool`,
  `Handoff`, `Guardrail`, and typed outputs.
- [Providers](providers.md) covers subscription and API backends, setup,
  session continuity, and errors.
- [Durable tools](durable-tools.md) explains the `preview` MCP bridge and
  tool replay.
- [Budgets and handoffs](budgets-and-handoffs.md) explains deterministic
  spend enforcement and child workflows.
- [Observability](observability.md) covers the GenAI-convention trace
  tree, the host-owned exporter model, redaction, and Pydantic Logfire.
- [Usage and credits](usage-and-credits.md) covers the typed accounting
  surface, per-provider limit/credit signals, and the capability matrix.
- [Aurora DSQL clusters](dsql-clusters.md) covers managed creation,
  operator-owned adoption, descriptors, IAM requirements, and teardown.
- [Examples](examples/README.md) maps each executable example to the behavior
  it proves.
