//! Model providers — subscription-first, harness-driven.
//!
//! This crate will own the one provider trait and its implementations. The
//! primary tier drives the vendors' own harnesses as supervised subprocesses,
//! because that is where subscription auth, session state, and vendor tooling
//! already live:
//!
//! - **Claude** — headless Claude Code: `claude -p --output-format
//!   stream-json`, one process per turn, sessions resumed by id on retry,
//!   streaming events surfaced as activity heartbeats, exit codes mapped to a
//!   retry taxonomy. (Event shapes and resume behaviour are ground-truthed by
//!   `spikes/claude-driver`.)
//! - **Codex** — `codex app-server` JSON-RPC as the transport, `codex exec
//!   --json` as the documented fallback.
//!
//! Harness versions are pinned like the Temporal pin: a provider states the
//! harness versions it is conformance-tested against, and drift is a
//! detect-and-explain error, never a silent behaviour change.
//!
//! The secondary tier — raw-API providers (Anthropic Messages, OpenAI
//! Responses) — lives behind the `api-anthropic` / `api-openai` features:
//! minimal internal loops for users without a subscription seat.
//!
//! The trait boundary this crate exports is what `odori-agents`' runner calls
//! for each turn; it is frozen early (launch plan: EOD day 22) so provider
//! implementations can proceed in parallel behind a stable seam.
