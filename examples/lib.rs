//! The scenarios behind Odori's runnable examples.
//!
//! Each stays beside the example that runs it, so one directory is one
//! example. Declaring them here gives each a single crate identity:
//! `odori_examples::<name>` is the same module for the example target and
//! for the test that asserts on it.

// The run's narration is the example — hence `println!` over `tracing`.
#![allow(clippy::print_stdout)]

#[path = "approval-resume/scenario/mod.rs"]
pub mod approval_resume;
#[path = "logfire/scenario/mod.rs"]
pub mod logfire;
#[path = "rewind/scenario/mod.rs"]
pub mod rewind;
#[path = "slice-fleet/scenario/mod.rs"]
pub mod slice_fleet;
