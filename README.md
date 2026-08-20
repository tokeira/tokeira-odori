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

| Crate | Owns |
| --- | --- |
| [`odori`](crates/odori) | The facade — the one name a quickstart depends on |
| [`odori-agents`](crates/odori-agents) | Primitives: `Agent`, `Runner`, `Tool`, `Handoff`, `Guardrail`, typed outputs, sessions |
| [`odori-providers`](crates/odori-providers) | Supervised vendor harnesses (Claude Code, Codex); raw APIs behind features |
| [`odori-engine`](crates/odori-engine) | Embedded tokeira + Temporal Rust SDK worker bootstrap |
| [`odori-mcp-bridge`](crates/odori-mcp-bridge) | Framework-owned tools as durable activities, mid-turn |

## License

[Apache-2.0](LICENSE).
