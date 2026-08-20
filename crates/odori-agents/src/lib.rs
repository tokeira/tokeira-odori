//! Agent primitives — the surface a user of Odori programs against.
//!
//! The primitive set is borrowed deliberately from the OpenAI Agents SDK so
//! the shape is already familiar; the substrate underneath is what Odori
//! adds. One run is one workflow ([`run::AgentRun`]); each harness turn the
//! loop takes is one activity ([`run::TurnActivities`]); the loop itself is
//! therefore replayable history, and a crashed run resumes from its last
//! completed turn.
//!
//! - [`agent::Agent`] — a named configuration: instructions, provider
//!   binding, tools, guardrails, output shape.
//! - [`runner::Runner`] — the client surface: start runs, follow
//!   conversations, fetch typed outputs.
//! - [`tool::Tool`] — a typed, framework-owned capability; durable
//!   execution arrives with the mcp-bridge (`.kiro/specs/mcp-bridge/`),
//!   declarative delegation to harness tooling until then.
//! - [`guardrail::Guardrail`] — deterministic input/output validation, plus
//!   [`guardrail::RunBudget`] turn/cost caps enforced by the run loop.
//! - [`output::AgentOutput`] — typed outputs: a run produces a value, not a
//!   string.
//! - [`provider::Provider`] — the frozen seam to model backends; the unit
//!   is one harness turn.
//!
//! Nothing here talks to a model vendor directly: providers live in
//! `odori-providers`, and the embedded engine plus worker bootstrap live in
//! `odori-engine`.

pub mod agent;
pub mod guardrail;
pub mod invocation;
pub mod output;
pub mod provider;
pub mod run;
pub mod runner;
pub mod tool;

pub use agent::{Agent, AgentRegistry};
pub use guardrail::{Guardrail, GuardrailVerdict, RunBudget};
pub use invocation::{InvocationId, InvocationRegistry, ToolCallResult};
pub use output::{AgentOutput, Json};
pub use provider::{Provider, TurnError, TurnEventSink, TurnOutcome, TurnRequest};
pub use run::{AgentRun, Providers, RunConfig, RunEnd, RunOutput, TurnActivities};
pub use runner::{Conversation, Runner, RunnerError, register_odori};
pub use tool::{Tool, ToolFailure, ToolPolicy};
