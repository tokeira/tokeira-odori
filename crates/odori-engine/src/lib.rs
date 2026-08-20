//! The durable-execution substrate: embedded tokeira plus the worker runtime.
//!
//! This crate will own everything between "the user calls run" and "a
//! workflow task executes":
//!
//! - **Embedded engine lifecycle** — starting tokeira's in-process engine
//!   (`Engine::embedded()` in the engine repo: runtime + in-memory store +
//!   edge on an ephemeral loopback port), snapshot persist/restore, and clean
//!   teardown. No server to operate: the engine lives inside the user's
//!   process.
//! - **Worker bootstrap** — the Temporal Rust SDK 0.7 worker (vanilla
//!   crates.io `temporalio-*` pins) connected over the loopback port to the
//!   embedded engine, which speaks the Temporal contract. The SDK is the
//!   worker programming model; tokeira is the server side; this crate is the
//!   marriage.
//! - **Run-loop registration** — registering `odori-agents`' run-loop
//!   workflow and turn activities (and the MCP bridge's tool activities) on
//!   that worker, with the retry policies and heartbeat plumbing turns need.
//!
//! The loopback port is a v0 pragmatism, not the destination: the roadmap's
//! in-memory duplex transport (upstream custom-connector PR) removes the port
//! entirely, and this crate is where that swap will happen — invisibly to
//! everything above it.
