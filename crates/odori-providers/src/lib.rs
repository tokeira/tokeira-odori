//! Model providers — subscription-first, harness-driven.
//!
//! This crate owns the implementations of Odori's provider trait. The
//! primary tier drives the vendors' own harnesses as supervised subprocesses,
//! because that is where subscription auth, session state, and vendor tooling
//! already live:
//!
//! - **Claude** — headless Claude Code: `claude -p --output-format
//!   stream-json`, one process per turn, sessions resumed by id on retry,
//!   streaming events surfaced as activity heartbeats, exit codes mapped to a
//!   retry taxonomy.
//! - **Codex** — `codex app-server` JSON-RPC, one supervised process per
//!   turn, with persisted threads resumed by id.
//!
//! Harness CLIs are versioned runtime dependencies. Each provider states its
//! minimum version and conformance baseline; newer version drift is detected
//! and warned about, never a silent behaviour change.
//!
//! The secondary tier — raw-API providers (Anthropic Messages, OpenAI
//! Responses) — lives behind the `api-anthropic` / `api-openai` features:
//! minimal internal loops for users without a subscription seat.
//!
//! The seam this crate implements is `odori_agents::provider::Provider` —
//! defined crate-side with the primitives (both ends of the seam need the
//! agents crate's types, so defining it there keeps the graph acyclic) and
//! frozen so every backend and the runner share one stable turn contract.

#[cfg(any(feature = "api-anthropic", feature = "api-openai"))]
pub mod api;
pub mod claude;
pub mod claude_flags;
pub mod codex;

#[cfg(feature = "api-anthropic")]
pub use api::anthropic::{AnthropicConfig, AnthropicProvider};
#[cfg(feature = "api-openai")]
pub use api::openai::{OpenAiConfig, OpenAiProvider};
pub use claude::{ClaudeConfig, ClaudeProvider, PINNED_VERSION};
pub use codex::{CodexProvider, EXPECTED_CODEX_CLI_VERSION};
