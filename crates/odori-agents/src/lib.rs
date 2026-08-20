//! Agent primitives — the surface a user of Odori programs against.
//!
//! This crate will own the framework's primitive set, borrowed deliberately
//! from the OpenAI Agents SDK so the shape is already familiar:
//!
//! - **`Agent`** — a named configuration: instructions, tools, handoffs,
//!   guardrails, an output type, and a provider binding.
//! - **`Runner`** — the run loop. One run is one workflow; each harness turn
//!   the loop takes is one activity. The loop itself is therefore replayable
//!   history, and a crashed run resumes from its last completed turn.
//! - **`Tool`** — a typed, framework-owned capability. When the MCP bridge is
//!   active, a tool call arriving mid-turn executes as a durable activity
//!   (see `odori-mcp-bridge`); otherwise tools delegate to the harness's own
//!   tooling.
//! - **`Handoff`** — delegation to another agent, mapped to a child workflow
//!   so the delegate's run is itself durable and individually inspectable.
//! - **`Guardrail`** — input/output validation plus run budgets (turn caps,
//!   token/cost ceilings) enforced by the runner.
//! - **Typed outputs** — a run produces a deserialized, schema-checked value,
//!   not a string.
//! - **Sessions** — conversation history as first-class state. A session id
//!   names the harness-side conversation; resuming a run resumes the session.
//!
//! Nothing here talks to a model vendor directly: providers live in
//! `odori-providers`, and durability comes from `odori-engine`. This crate
//! stays pure surface — types, traits, and the runner's orchestration logic.
