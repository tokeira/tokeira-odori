//! Odori — a minimal Rust agent framework with durable execution built in.
//!
//! This is the facade crate: the one name a quickstart depends on. It owns no
//! machinery of its own — it re-exports the workspace's public surface so that
//! `cargo add odori` plus ~20 lines is a complete, durable multi-agent
//! program.
//!
//! The primitive set is deliberately small and borrowed from the OpenAI
//! Agents SDK: `Agent`, `Runner`, `Tool`, `Handoff`, `Guardrail`, and typed
//! outputs (see [`agents`]). What Odori adds is the execution substrate:
//! every run executes durably on an embedded tokeira engine ([`engine`]),
//! providers drive vendor harnesses as supervised subprocesses
//! ([`providers`]), and framework-owned tools invoked mid-turn execute as
//! durable activities through the MCP bridge ([`mcp_bridge`]).
//!
//! The facade re-exports the public primitive, provider, and runtime crates;
//! advanced APIs remain available through the `agents`, `providers`,
//! `engine`, and `mcp_bridge` modules.

pub use odori_agents as agents;
pub use odori_engine as engine;
pub use odori_mcp_bridge as mcp_bridge;
pub use odori_providers as providers;

pub use odori_agents::{
    Agent, AgentOutput, AgentRegistry, Conversation, Effort, Guardrail, GuardrailVerdict, Json,
    Provider, Providers, RunBudget, RunConfig, RunEnd, RunOutput, Runner, RunnerError, Tool,
    ToolFailure, ToolPolicy, TurnRecord,
};
pub use odori_engine::{
    ClusterStartupReport, ConnectTarget, DsqlMigrationPolicy, EmbeddedConfigError,
    EmbeddedDsqlLimits, EmbeddedEngineConfig, EmbeddedEngineShutdownError,
    EmbeddedEngineStartError, EmbeddedShutdownFailure, EmbeddedStartupPhase, EmbeddedStorageConfig,
    EmbeddedStorageMode, EmbeddedValidationError, Engine, EngineStartupReport,
    ExistingEmbeddedDsqlConfig, ManagedClusterIntent, ManagedEmbeddedDsqlConfig, OdoriRuntime,
    OwnershipStartupReport, SchemaStartupOutcome, SchemaStartupReport, SnapshotPolicyConfig,
    TokeiraConfig,
};
