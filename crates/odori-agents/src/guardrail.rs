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
/// Unknown token or cost usage is recorded explicitly and counts against the
/// turn cap, never as a reported zero. A token/cost cap is evaluated from the
/// values the provider did report. If either half of a turn's token total is
/// unknown, that turn counts only against `max_turns`, not the token cap;
/// likewise an unknown cost counts only against `max_turns`. Users who require
/// a hard ceiling across a backend with unknown usage must set `max_turns`.
/// Provider retries count against these caps: successful turn usage includes
/// failed-attempt spend recovered through activity heartbeat details.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct RunBudget {
    /// Maximum number of turns the run may execute.
    pub max_turns: Option<u32>,
    /// Maximum cumulative reported input + output tokens.
    pub max_total_tokens: Option<u64>,
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

    /// Cap cumulative reported input + output tokens.
    pub fn with_max_total_tokens(mut self, tokens: u64) -> Self {
        self.max_total_tokens = Some(tokens);
        self
    }

    /// Cap cumulative reported cost.
    pub fn with_max_cost_usd(mut self, cost: f64) -> Self {
        self.max_cost_usd = Some(cost);
        self
    }

    pub(crate) fn intersect(&self, other: &Self) -> Self {
        Self {
            max_turns: min_option(self.max_turns, other.max_turns),
            max_total_tokens: min_option(self.max_total_tokens, other.max_total_tokens),
            max_cost_usd: match (self.max_cost_usd, other.max_cost_usd) {
                (Some(left), Some(right)) => Some(left.min(right)),
                (Some(value), None) | (None, Some(value)) => Some(value),
                (None, None) => None,
            },
        }
    }
}

fn min_option<T: Ord>(left: Option<T>, right: Option<T>) -> Option<T> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}
