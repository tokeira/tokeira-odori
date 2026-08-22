//! Typed inputs, snapshots, and events for the rewind scenario.

use serde::{Deserialize, Serialize};

pub(super) const PLAN_HASH: &str = "plan-v1-bugfix-feature-budget-contract";

#[derive(Debug)]
pub(super) enum RewindEvent {
    CheckpointRecorded,
    FailureReturned,
    RetryPresented(u32),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct DeliberationSnapshot {
    pub(super) goal: String,
    pub(super) checkpoint: String,
    pub(super) plan_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct TimelineInput {
    pub(super) snapshot: DeliberationSnapshot,
    pub(super) decision: String,
}
