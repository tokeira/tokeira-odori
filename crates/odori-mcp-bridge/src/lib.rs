//! The MCP tool server — durable tools reachable from inside a harness turn.
//!
//! A harness (Claude Code, Codex) mid-turn may call a tool the framework
//! owns. That tool must execute durably — as a tokeira activity with its
//! own retry policy — while the harness blocks awaiting the MCP response,
//! inside a turn that is *itself* an activity. This crate is that MCP tool server:
//!
//! - `bootstrap::McpToolServer` — the in-process streamable-HTTP MCP server on
//!   loopback, per-attempt bearer tokens, and the `McpBootstrapper`
//!   implementation the turn activities consume;
//! - `broker::CallBroker` — `tools/call` → workflow update translation
//!   with keepalive progress (record-before-respond is structural: only a
//!   completed update's reply can be returned);
//! - the workflow-side pieces (tool-call tracker, `tool_invoked` update
//!   handler, `execute_tool` activity) live in `odori-agents`, which this
//!   crate depends on — never the reverse.
//!
//! The `preview` feature gates the complete MCP tool server:
//! with it off this crate compiles to nothing — no listener, no
//! attachment, no MCP tool server code on any path — and framework tools delegate
//! to the harness's own tooling. (Module references above are plain code
//! spans, not links: the modules exist only under `preview`, and the
//! preview-off rustdoc build must stay warning-free.)

#[cfg(feature = "preview")]
pub mod bootstrap;
#[cfg(feature = "preview")]
pub mod broker;
#[cfg(feature = "preview")]
mod server;

#[cfg(feature = "preview")]
pub use bootstrap::{McpToolServer, McpToolServerConfig};
#[cfg(feature = "preview")]
pub use broker::{CallBroker, McpToolServerError, ToolCallDispatcher, WorkflowToolCallDispatcher};
