//! The durable-execution substrate: embedded tokeira plus the worker runtime.
//!
//! This crate will own everything between "the user calls run" and "a
//! workflow task executes":
//!
//! - **Embedded engine lifecycle** — starting tokeira's in-process engine
//!   (`Engine::start_with_config` in the engine repo: runtime + in-memory
//!   store + an in-process RPC endpoint), snapshot persist/restore, and
//!   clean teardown. No server to operate, no TCP listener: the engine lives
//!   inside the user's process and the SDK connects through
//!   `ConnectionOptions::service_override` — an in-memory duplex.
//! - **Worker bootstrap** — the Temporal Rust SDK 0.7 worker (vanilla
//!   crates.io `temporalio-*` pins) connected over that service override to
//!   the embedded engine, which speaks the Temporal contract. The SDK is the
//!   worker programming model; tokeira is the server side; this crate is the
//!   marriage. This crate depends only on SDK types (`service_override`
//!   accepts the SDK's own callback-service type), so the engine plugs in
//!   from the application side and `odori-engine` stays engine-agnostic —
//!   the same bootstrap drives an external tokeirad or Temporal server via a
//!   target URL.
//! - **Run-loop registration** — registering `odori-agents`' run-loop
//!   workflow and turn activities (and the MCP bridge's tool activities) on
//!   that worker, with the retry policies and heartbeat plumbing turns need.
