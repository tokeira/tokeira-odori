//! Agent primitives — the surface a user of Odori programs against.
//!
//! Landing order note: [`provider`] is the frozen seam (EOD 2026-08-22);
//! the remaining primitives follow in this slice.

pub mod provider;

pub use provider::{Provider, TurnError, TurnEventSink, TurnOutcome, TurnRequest};
