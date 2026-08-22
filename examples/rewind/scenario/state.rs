//! In-process observations that make retry and dedupe behavior visible.

use std::sync::{Mutex, atomic::AtomicU64};

use super::model::RewindEvent;

#[derive(Debug)]
pub(super) struct RewindState {
    pub(super) tool_executions: AtomicU64,
    pub(super) presentations: Mutex<Vec<(u32, String)>>,
    pub(super) events: tokio::sync::mpsc::UnboundedSender<RewindEvent>,
}
