//! Fleet evidence and events shared by the scenario's durable components.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Mutex,
};

use super::{model::SlicePlan, workspace::TempFixture};

#[derive(Debug, Default)]
pub(super) struct Evidence {
    pub(super) approvals: BTreeSet<String>,
    pub(super) applied: BTreeSet<String>,
    pub(super) finish_bars: BTreeMap<String, Vec<String>>,
    pub(super) reviews: BTreeMap<String, String>,
    pub(super) scope_refusals: u32,
    pub(super) budget_exceeded: bool,
    pub(super) raised: bool,
}

#[derive(Debug)]
pub(super) struct FleetState {
    pub(super) fixture: TempFixture,
    pub(super) evidence: Mutex<Evidence>,
    pub(super) events: tokio::sync::mpsc::UnboundedSender<FleetEvent>,
}

impl FleetState {
    pub(super) fn approve(&self, slice: &str) {
        self.evidence
            .lock()
            .expect("fleet evidence lock")
            .approvals
            .insert(slice.to_owned());
    }
}

#[derive(Debug)]
pub(super) enum FleetEvent {
    Plan(SlicePlan),
    SlicesReady,
    ApplyRefused(String),
    Applied(String),
    RaiseObserved,
}
