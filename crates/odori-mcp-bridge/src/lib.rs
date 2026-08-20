//! The MCP bridge — durable tools reachable from inside a harness turn.
//!
//! A harness (Claude Code, Codex) mid-turn may call a tool the framework
//! owns. That tool must execute durably — as a tokeira activity with its own
//! retry policy — while the harness blocks awaiting the MCP response, inside
//! a turn that is *itself* an activity. This crate will own that bridge:
//!
//! - the **in-process MCP server** each harness subprocess is pointed at;
//! - **tool-call → activity translation**: an incoming MCP `tools/call`
//!   becomes an activity invocation, and the MCP response is the activity's
//!   result;
//! - **idempotency**: a retried turn replays its tool calls, so invocations
//!   are deduplicated (turn attempt × call id) and replayed calls return the
//!   recorded prior result instead of re-executing;
//! - **timeout interplay**: harness-side MCP client timeouts versus
//!   long-running activities, bridged with progress/keepalive;
//! - the **failure taxonomy**: tool failure vs bridge failure vs harness
//!   death mid-await, and crash-mid-turn recovery (turn retry + harness
//!   session resume).
//!
//! The design is frozen in `docs/design/mcp-bridge.md` before implementation
//! begins; the invariants stated there are binding on this crate. The
//! `preview` feature is the descope boundary: bridge off, framework tools
//! delegate to the harness's own tooling.
