//! Typed values that cross the proposal, approval, and completion boundaries.

use serde::{Deserialize, Serialize};

use super::workspace::{ALLOWED_PATH, BROKEN_LIB, FIXED_LIB};

pub const PLAN_HASH: &str = "plan-v1-fix-increment";

pub(super) const SESSION_ID: &str = "approval-resume-session";
pub(super) const FORKED_SESSION_ID: &str = "approval-resume-approved-session";

/// The durable first-turn result presented to the human approval seat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchProposal {
    pub summary: String,
    pub plan_hash: String,
    pub file_scope: Vec<String>,
    pub before: String,
    pub after: String,
    pub finish_bar: Vec<String>,
}

/// The decision recorded as the restored workflow's second-turn input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct ApprovalDecision {
    pub(super) decision: String,
    pub(super) plan_hash: String,
}

/// The terminal result produced only after the approved patch and finish bar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalCompletion {
    pub plan_hash: String,
    pub applied: String,
    pub finish_bar: Vec<String>,
    pub session_forked: bool,
}

pub(super) fn proposal() -> PatchProposal {
    PatchProposal {
        summary: "Fix increment so the bundled regression test passes.".to_owned(),
        plan_hash: PLAN_HASH.to_owned(),
        file_scope: vec![ALLOWED_PATH.to_owned()],
        before: BROKEN_LIB.to_owned(),
        after: FIXED_LIB.to_owned(),
        finish_bar: vec!["cargo test --locked".to_owned()],
    }
}
