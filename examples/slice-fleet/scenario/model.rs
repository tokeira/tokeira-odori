//! The typed slice plan and worker outcomes presented to the approval seat.

use serde::{Deserialize, Serialize};

pub const PLAN_HASH: &str = "plan-v1-bugfix-feature-budget-contract";

/// A bounded unit of fleet work. The file scope is data, not prose: the
/// write activity enforces it before touching the fixture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Slice {
    pub id: String,
    pub task: String,
    pub provider: String,
    pub files: Vec<String>,
}

/// The orchestrator's typed, human-approved plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlicePlan {
    pub goal: String,
    pub hash: String,
    pub slices: Vec<Slice>,
}

impl SlicePlan {
    pub(super) fn campaign() -> Self {
        Self {
            goal: "make the fixture's failing test pass and implement double".to_owned(),
            hash: PLAN_HASH.to_owned(),
            slices: vec![
                Slice {
                    id: "increment-bugfix".to_owned(),
                    task: "fix increment's failing unit test".to_owned(),
                    provider: "claude-scripted".to_owned(),
                    files: vec!["src/increment.rs".to_owned()],
                },
                Slice {
                    id: "double-feature".to_owned(),
                    task: "implement double with a unit test".to_owned(),
                    provider: "codex-scripted".to_owned(),
                    files: vec!["src/double.rs".to_owned()],
                },
                Slice {
                    id: "budget".to_owned(),
                    task: "attempt an explicitly capped documentation slice".to_owned(),
                    provider: "claude-scripted".to_owned(),
                    files: vec!["README.md".to_owned()],
                },
                Slice {
                    id: "contract".to_owned(),
                    task: "raise rather than reshape a frozen contract".to_owned(),
                    provider: "codex-scripted".to_owned(),
                    files: vec!["Cargo.toml".to_owned()],
                },
            ],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub(super) enum WorkerOutcome {
    Ready {
        slice: String,
        review: Review,
        finish_bar: Vec<String>,
    },
    Raise {
        slice: String,
        contract: String,
        evidence: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Review {
    pub(super) reviewer: String,
    pub(super) provider: String,
    pub(super) verdict: String,
}
