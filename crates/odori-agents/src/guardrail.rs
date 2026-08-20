//! The `Guardrail` primitive and the run-budget hook.
//!
//! v0 ships the primitive plus budget/turn caps; the curated validator
//! library comes later. Two mechanisms, two homes:
//!
//! - **Guardrails** ([`Guardrail`]) are checks over turn input/output text.
//!   They run *inside the run-loop workflow*, so they MUST be
//!   deterministic: same text in, same verdict out, no I/O, no clocks, no
//!   randomness. A guardrail that needs a model or a network call belongs
//!   in a future activity-backed tier, not here.
//! - **Budgets** ([`RunBudget`]) cap turns and spend. The runner enforces
//!   them between turns from [`crate::provider::TurnUsage`] accounting —
//!   deterministic because usage arrives via recorded activity results.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A deterministic check applied to run input (before the first turn) or
/// turn output (after every turn).
///
/// Runs inside workflow code — determinism is a hard requirement, not a
/// style preference. A tripped guardrail ends the run with
/// [`crate::run::RunEnd::GuardrailBlocked`]; it does not retry the turn.
pub trait Guardrail: fmt::Debug + Send + Sync + 'static {
    /// Stable name, recorded in the run output when the guardrail trips.
    fn name(&self) -> &str;

    /// Evaluate the text.
    fn check(&self, text: &str) -> GuardrailVerdict;
}

/// A guardrail's decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GuardrailVerdict {
    /// The text passes; the run proceeds.
    Pass,
    /// The text is rejected; the run ends, carrying the reason.
    Block {
        /// Operator-facing reason recorded in the run output.
        reason: String,
    },
}

/// Caps enforced by the run loop between turns.
///
/// Unknown usage counts as zero: a backend that reports no cost cannot trip
/// the cost cap. Both caps ending a run produce
/// [`crate::run::RunEnd::BudgetExceeded`] with the cap that tripped.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RunBudget {
    /// Maximum number of turns the run may execute.
    pub max_turns: Option<u32>,
    /// Maximum cumulative reported cost in USD.
    pub max_cost_usd: Option<f64>,
}

impl RunBudget {
    /// An unlimited budget.
    pub fn unlimited() -> Self {
        Self::default()
    }

    /// Cap the number of turns.
    pub fn with_max_turns(mut self, turns: u32) -> Self {
        self.max_turns = Some(turns);
        self
    }

    /// Cap cumulative reported cost.
    pub fn with_max_cost_usd(mut self, cost: f64) -> Self {
        self.max_cost_usd = Some(cost);
        self
    }
}
