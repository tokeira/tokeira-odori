//! The run loop as a workflow; the turn as an activity.
//!
//! [`AgentRun`] is the durable heart of Odori: one workflow execution per
//! run, one [`TurnActivities::execute_turn`] activity per harness turn.
//! Everything the run observes — turn results, usage, session lineage — is
//! recorded history, so a crashed run resumes from its last completed turn
//! with nothing re-executed.
//!
//! Conversation shape: the initial prompt is turn 0. In interactive runs
//! ([`RunConfig::interactive`]), later user messages arrive via the
//! `user_message` signal and each becomes a turn resumed **forked** from
//! the previous turn's session; `end_conversation` ends the run. Forking
//! per turn (rather than resuming in place) is the retry-isolation rule
//! from the claude-driver spike: a retried turn re-forks from the same
//! stable parent, so a failed attempt's divergence never contaminates the
//! lineage the run records.
//!
//! ## Determinism boundary
//!
//! The workflow touches only its inputs, recorded activity results, and the
//! agent's guardrails (deterministic by [`crate::guardrail`]'s contract).
//! Agents are resolved from the worker's [`AgentRegistry`] — configuration,
//! not data — injected via the workflow factory at registration
//! ([`crate::runner::register_odori`]). Changing an agent's guardrails
//! while its runs are in flight is a determinism hazard, exactly like
//! redeploying changed workflow code; ship such changes as new agent names
//! or drain first.
//!
//! ## mcp-bridge slots (O6)
//!
//! Per `.kiro/specs/mcp-bridge/`, this workflow later gains the
//! `tool_invoked` update handler and the invocation registry as workflow
//! state, keyed by the turn/attempt identity the turn activity already
//! stamps on every [`crate::provider::TurnRequest`]. The state struct and
//! the turn-record shape keep those additions purely additive.

// The temporalio macro family (`#[workflow]`, `#[workflow_methods]`,
// `#[activities]`) generates per-method marker types without Debug derives,
// which the workspace's missing_debug_implementations deny would reject.
// The allow is file-scoped; every hand-written type here derives Debug.
#![allow(missing_debug_implementations)]

use std::{collections::VecDeque, sync::Arc, time::Duration};

use serde::{Deserialize, Serialize};
use temporalio_macros::{activities, workflow, workflow_methods};
use temporalio_sdk::{
    ActivityOptions, SyncWorkflowContext, WorkflowContext, WorkflowContextView, WorkflowResult,
    activities::{ActivityContext, ActivityError},
    error::ApplicationFailure,
};
use thiserror::Error;
use tokio::sync::mpsc;

use crate::{
    agent::AgentRegistry,
    guardrail::{GuardrailVerdict, RunBudget},
    invocation::{Admission, InvocationId, InvocationRegistry, ToolCallResult},
    provider::{
        Provider, SessionDirective, TurnError, TurnEvent, TurnEventSink, TurnIdentity, TurnOutcome,
        TurnRequest, TurnUsage,
    },
    tool::{ToolContext, ToolFailure, ToolPolicy},
};

/// Input starting one run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunInput {
    /// Name of the agent to run (resolved from the worker's registry).
    pub agent: String,
    /// The initial prompt (turn 0's input).
    pub prompt: String,
    /// Run-level configuration.
    pub config: RunConfig,
}

/// Run-level configuration, all fields defaulted.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RunConfig {
    /// Turn and cost caps, enforced between turns.
    pub budget: RunBudget,
    /// `true` keeps the run alive after turn 0 for `user_message` signals;
    /// `false` completes after the first turn.
    pub interactive: bool,
    /// Start-to-close timeout for each turn activity.
    pub turn_timeout: Duration,
    /// Heartbeat timeout for turn activities. Providers emit liveness from
    /// harness stream events; silence for this long kills the attempt.
    pub turn_heartbeat_timeout: Duration,
    /// Maximum attempts per turn (1 = never retry).
    pub turn_max_attempts: u32,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            budget: RunBudget::default(),
            interactive: false,
            turn_timeout: Duration::from_secs(600),
            turn_heartbeat_timeout: Duration::from_secs(120),
            turn_max_attempts: 3,
        }
    }
}

// The struct is `#[non_exhaustive]`, so downstream crates configure it
// through these instead of struct literals.
impl RunConfig {
    /// Replace the run budget.
    pub fn with_budget(mut self, budget: RunBudget) -> Self {
        self.budget = budget;
        self
    }

    /// Keep the run alive for `user_message` signals after turn 0.
    pub fn with_interactive(mut self, interactive: bool) -> Self {
        self.interactive = interactive;
        self
    }

    /// Set the per-turn start-to-close timeout.
    pub fn with_turn_timeout(mut self, timeout: Duration) -> Self {
        self.turn_timeout = timeout;
        self
    }

    /// Set the per-turn heartbeat timeout.
    pub fn with_turn_heartbeat_timeout(mut self, timeout: Duration) -> Self {
        self.turn_heartbeat_timeout = timeout;
        self
    }

    /// Set the maximum attempts per turn (1 = never retry).
    pub fn with_turn_max_attempts(mut self, attempts: u32) -> Self {
        self.turn_max_attempts = attempts;
        self
    }
}

/// One completed turn, as recorded in the run's transcript.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TurnRecord {
    /// Zero-based turn index.
    pub turn: u32,
    /// The input that drove the turn.
    pub input: String,
    /// The turn's final text.
    pub text: String,
    /// Backend session id the turn ran under.
    pub session_id: String,
    /// The turn's reported usage.
    pub usage: TurnUsage,
}

/// Aggregated run accounting.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RunUsage {
    /// Sum of reported turn costs, USD.
    pub total_cost_usd: f64,
    /// Sum of reported input tokens.
    pub input_tokens: u64,
    /// Sum of reported output tokens.
    pub output_tokens: u64,
}

impl RunUsage {
    fn absorb(&mut self, turn: &TurnUsage) {
        self.total_cost_usd += turn.total_cost_usd.unwrap_or(0.0);
        self.input_tokens += turn.input_tokens.unwrap_or(0);
        self.output_tokens += turn.output_tokens.unwrap_or(0);
    }
}

/// Why the run ended.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum RunEnd {
    /// The (non-interactive) run's turn completed.
    Completed,
    /// An interactive run was ended by `end_conversation`.
    ConversationEnded,
    /// A budget cap tripped before the next turn.
    BudgetExceeded {
        /// Which cap tripped.
        cap: BudgetCap,
    },
    /// A guardrail rejected run input or turn output.
    GuardrailBlocked {
        /// The tripped guardrail's name.
        guardrail: String,
        /// Its stated reason.
        reason: String,
    },
}

/// The budget dimension that ended a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BudgetCap {
    /// [`RunBudget::max_turns`].
    Turns,
    /// [`RunBudget::max_cost_usd`].
    CostUsd,
}

/// The result of a run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RunOutput {
    /// Final text of the last completed turn (empty when no turn ran).
    pub text: String,
    /// Session id of the last completed turn — the resume point for a
    /// follow-on run.
    pub session_id: Option<String>,
    /// Aggregated usage.
    pub usage: RunUsage,
    /// Number of completed turns.
    pub turns: u32,
    /// Why the run ended.
    pub end: RunEnd,
}

/// The run-loop workflow. One instance per run; construct via the factory
/// registered by [`crate::runner::register_odori`].
#[workflow]
#[derive(Debug, Default)]
pub struct AgentRun {
    /// Worker configuration: the agent registry (see the module docs'
    /// determinism note).
    registry: Arc<AgentRegistry>,
    /// Messages queued by the `user_message` signal, drained one per turn.
    pending_messages: VecDeque<String>,
    /// Set by the `end_conversation` signal.
    conversation_ended: bool,
    /// Completed turns, in order.
    transcript: Vec<TurnRecord>,
    /// Agent under execution, set before the first turn; the `tool_invoked`
    /// update resolves tools through it.
    agent_name: Option<String>,
    /// The mcp-bridge invocation registry (spec Requirements 3 and 4).
    invocations: InvocationRegistry,
}

impl AgentRun {
    /// Construct with the worker's agent registry (factory registration).
    pub fn with_registry(registry: Arc<AgentRegistry>) -> Self {
        Self {
            registry,
            ..Self::default()
        }
    }
}

#[workflow_methods]
impl AgentRun {
    /// Execute the run to completion. See the module docs for the loop
    /// shape.
    #[run]
    pub async fn run(
        ctx: &mut WorkflowContext<Self>,
        input: RunInput,
    ) -> WorkflowResult<RunOutput> {
        let registry = ctx.state(|state| state.registry.clone());
        let agent = registry.get(&input.agent).map_err(|error| {
            temporalio_workflow::WorkflowTermination::failed_application(
                ApplicationFailure::builder(anyhow::anyhow!(error))
                    .type_name("odori::run::unknown_agent".to_owned())
                    .non_retryable(true)
                    .build(),
            )
        })?;

        ctx.state_mut(|state| state.agent_name = Some(input.agent.clone()));

        let mut usage = RunUsage::default();
        let end;

        // Input guardrails, before any spend.
        if let Some((name, reason)) = trip(agent.input_guardrails(), &input.prompt) {
            return Ok(RunOutput {
                text: String::new(),
                session_id: None,
                usage,
                turns: 0,
                end: RunEnd::GuardrailBlocked {
                    guardrail: name,
                    reason,
                },
            });
        }

        let mut next_input = Some(input.prompt.clone());
        loop {
            let turn_index = ctx.state(|state| state.transcript.len() as u32);

            // Budget gates, between turns.
            if let Some(max) = input.config.budget.max_turns
                && turn_index >= max
            {
                end = RunEnd::BudgetExceeded {
                    cap: BudgetCap::Turns,
                };
                break;
            }
            if let Some(max) = input.config.budget.max_cost_usd
                && usage.total_cost_usd >= max
            {
                end = RunEnd::BudgetExceeded {
                    cap: BudgetCap::CostUsd,
                };
                break;
            }

            let Some(turn_input) = next_input.take() else {
                unreachable!("loop continues only with an input queued");
            };

            // Each turn forks from the previous turn's session (module docs:
            // retry isolation).
            let session = match ctx.state(|state| {
                state
                    .transcript
                    .last()
                    .map(|record| record.session_id.clone())
            }) {
                None => SessionDirective::Start,
                Some(parent) => SessionDirective::ResumeForked { session_id: parent },
            };

            let activity_input = TurnActivityInput {
                agent: input.agent.clone(),
                turn: turn_index,
                input: turn_input.clone(),
                session,
            };
            let outcome: TurnActivityOutput = ctx
                .execute_activity(
                    TurnActivities::execute_turn,
                    activity_input,
                    turn_activity_options(&input.config),
                )
                .await?;

            usage.absorb(&outcome.usage);
            let record = TurnRecord {
                turn: turn_index,
                input: turn_input,
                text: outcome.text.clone(),
                session_id: outcome.session_id.clone(),
                usage: outcome.usage.clone(),
            };
            ctx.state_mut(|state| state.transcript.push(record));

            // Output guardrails.
            if let Some((name, reason)) = trip(agent.output_guardrails(), &outcome.text) {
                end = RunEnd::GuardrailBlocked {
                    guardrail: name,
                    reason,
                };
                break;
            }

            if !input.config.interactive {
                end = RunEnd::Completed;
                break;
            }

            // Interactive: wait for the next message or the end signal.
            // Queued messages drain before the end signal takes effect, so
            // a send() immediately followed by end() still gets its turn.
            ctx.wait_condition(|state| {
                state.conversation_ended || !state.pending_messages.is_empty()
            })
            .await?;
            next_input = ctx.state_mut(|state| state.pending_messages.pop_front());
            if next_input.is_none() {
                end = RunEnd::ConversationEnded;
                break;
            }
        }

        let (text, session_id, turns) = ctx.state(|state| {
            (
                state
                    .transcript
                    .last()
                    .map(|record| record.text.clone())
                    .unwrap_or_default(),
                state
                    .transcript
                    .last()
                    .map(|record| record.session_id.clone()),
                state.transcript.len() as u32,
            )
        });
        Ok(RunOutput {
            text,
            session_id,
            usage,
            turns,
            end,
        })
    }

    /// Queue a user message; interactive runs consume one per turn.
    #[signal]
    pub fn user_message(&mut self, _ctx: &mut SyncWorkflowContext<Self>, message: String) {
        self.pending_messages.push_back(message);
    }

    /// End an interactive conversation after the current turn.
    #[signal]
    pub fn end_conversation(&mut self, _ctx: &mut SyncWorkflowContext<Self>) {
        self.conversation_ended = true;
    }

    /// The completed turns so far.
    #[query]
    pub fn transcript(&self, _ctx: &WorkflowContextView) -> Vec<TurnRecord> {
        self.transcript.clone()
    }

    /// The mcp-bridge data path: one mid-turn tool call, made durable.
    ///
    /// Validate against the spec's contract-policy table, admit through the
    /// invocation registry, schedule `execute_tool` with the tool's policy
    /// on a fresh admission, and complete the update only once the result
    /// is recorded — record-before-respond (Property 2) is structural: the
    /// update cannot complete before its effects are in history.
    #[update]
    pub async fn tool_invoked(
        ctx: &mut WorkflowContext<Self>,
        invocation: ToolInvocation,
    ) -> ToolInvocationReply {
        // Contract-policy validation.
        if invocation.identity.call_id.is_empty() {
            return ToolInvocationReply::Rejected(InvocationRejection::InvalidCallId);
        }
        let current_turn = ctx.state(|state| state.transcript.len() as u32);
        if invocation.identity.turn > current_turn {
            return ToolInvocationReply::Rejected(InvocationRejection::UnknownTurn);
        }
        let agent = {
            let registry = ctx.state(|state| state.registry.clone());
            let Some(name) = ctx.state(|state| state.agent_name.clone()) else {
                return ToolInvocationReply::Rejected(InvocationRejection::UnknownTurn);
            };
            match registry.get(&name) {
                Ok(agent) => agent,
                Err(_) => return ToolInvocationReply::Rejected(InvocationRejection::UnknownTool),
            }
        };
        let Some(policy) = agent
            .tools()
            .iter()
            .find(|tool| tool.name() == invocation.tool)
            .map(|tool| tool.policy().clone())
        else {
            return ToolInvocationReply::Rejected(InvocationRejection::UnknownTool);
        };

        let admission = ctx.state_mut(|state| {
            state
                .invocations
                .admit(&invocation.identity, &invocation.tool)
        });
        match admission {
            Admission::Recorded(result) => ToolInvocationReply::Completed(result),
            Admission::Fenced => ToolInvocationReply::Rejected(InvocationRejection::Fenced),
            Admission::AwaitExisting => {
                let turn = invocation.identity.turn;
                let call_id = invocation.identity.call_id.clone();
                if ctx
                    .wait_condition(|state| state.invocations.recorded(turn, &call_id).is_some())
                    .await
                    .is_err()
                {
                    return ToolInvocationReply::Rejected(InvocationRejection::RunCancelled);
                }
                let result = ctx.state(|state| state.invocations.recorded(turn, &call_id).cloned());
                match result {
                    Some(result) => ToolInvocationReply::Completed(result),
                    None => ToolInvocationReply::Rejected(InvocationRejection::RunCancelled),
                }
            }
            Admission::Execute(ticket) => {
                let outcome: Result<ToolCallResult, _> = ctx
                    .execute_activity(
                        ToolActivities::execute_tool,
                        ExecuteToolInput {
                            agent: agent.name().to_owned(),
                            tool: invocation.tool.clone(),
                            arguments: invocation.arguments.clone(),
                            identity: invocation.identity.clone(),
                        },
                        tool_activity_options(&policy),
                    )
                    .await;
                // Tool retry exhaustion is a MODEL-VISIBLE tool failure
                // (spec Requirement 6.1), recorded like any result so
                // replays serve it identically; it never fails the turn
                // (Requirement 6.2).
                let result = match outcome {
                    Ok(result) => result,
                    Err(error) => ToolCallResult::error(format!("tool execution failed: {error}")),
                };
                ctx.state_mut(|state| state.invocations.complete(ticket, result.clone()));
                ToolInvocationReply::Completed(result)
            }
        }
    }
}

fn turn_activity_options(config: &RunConfig) -> ActivityOptions {
    ActivityOptions::with_start_to_close_timeout(config.turn_timeout)
        .heartbeat_timeout(config.turn_heartbeat_timeout)
        .retry_policy(
            temporalio_common::RetryPolicy::builder()
                .maximum_attempts(config.turn_max_attempts)
                .build(),
        )
        .build()
}

fn tool_activity_options(policy: &ToolPolicy) -> ActivityOptions {
    let close_timeouts = match policy.schedule_to_close {
        Some(schedule_to_close) => temporalio_common::ActivityCloseTimeouts::Both {
            schedule_to_close,
            start_to_close: policy.start_to_close,
        },
        None => temporalio_common::ActivityCloseTimeouts::StartToClose(policy.start_to_close),
    };
    ActivityOptions::with_close_timeouts(close_timeouts)
        .retry_policy(
            temporalio_common::RetryPolicy::builder()
                .maximum_attempts(policy.max_attempts.unwrap_or(3))
                .build(),
        )
        .maybe_heartbeat_timeout(policy.heartbeat_timeout)
        .build()
}

fn trip(
    guardrails: &[Arc<dyn crate::guardrail::Guardrail>],
    text: &str,
) -> Option<(String, String)> {
    for guardrail in guardrails {
        if let GuardrailVerdict::Block { reason } = guardrail.check(text) {
            return Some((guardrail.name().to_owned(), reason));
        }
    }
    None
}

/// Serialized input for one turn activity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnActivityInput {
    /// Agent name, resolved worker-side.
    pub agent: String,
    /// Zero-based turn index.
    pub turn: u32,
    /// The turn's input text.
    pub input: String,
    /// Session directive computed by the run loop.
    pub session: SessionDirective,
}

/// Serialized result of one turn activity (the durable mirror of
/// [`TurnOutcome`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnActivityOutput {
    /// Backend session id the turn ran under.
    pub session_id: String,
    /// Final turn text.
    pub text: String,
    /// Reported usage.
    pub usage: TurnUsage,
}

impl From<TurnOutcome> for TurnActivityOutput {
    fn from(outcome: TurnOutcome) -> Self {
        Self {
            session_id: outcome.session_id,
            text: outcome.text,
            usage: outcome.usage,
        }
    }
}

/// Heartbeat details recorded while a turn runs: the recovery anchor a
/// retried attempt reads (and, come O6, the bridge's fencing correlate).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TurnHeartbeat {
    /// Backend session id of the in-flight attempt, once known.
    pub session_id: Option<String>,
}

/// Provider lookup shared with the worker: implementations keyed by
/// [`Provider::name`], plus the default used by unbound agents.
#[derive(Debug, Clone)]
pub struct Providers {
    default: Arc<dyn Provider>,
    by_name: std::collections::HashMap<String, Arc<dyn Provider>>,
}

impl Providers {
    /// A provider set with `default` serving unbound agents.
    pub fn new(default: Arc<dyn Provider>) -> Self {
        let mut by_name = std::collections::HashMap::new();
        by_name.insert(default.name().to_owned(), default.clone());
        Self { default, by_name }
    }

    /// Register an additional provider under its own name.
    pub fn with(mut self, provider: Arc<dyn Provider>) -> Self {
        self.by_name.insert(provider.name().to_owned(), provider);
        self
    }

    fn resolve(&self, name: Option<&str>) -> Result<Arc<dyn Provider>, UnknownProvider> {
        match name {
            None => Ok(self.default.clone()),
            Some(name) => self
                .by_name
                .get(name)
                .cloned()
                .ok_or_else(|| UnknownProvider {
                    name: name.to_owned(),
                }),
        }
    }
}

/// An agent named a provider the worker does not hold.
#[derive(Debug, Clone, Error)]
#[error("no provider registered under the name {name:?}")]
pub struct UnknownProvider {
    /// The unresolved name.
    pub name: String,
}

/// The activity collection executing harness turns. Holds the worker-side
/// state activities need: the agent registry and the providers.
#[derive(Debug)]
pub struct TurnActivities {
    registry: Arc<AgentRegistry>,
    providers: Providers,
    attachments: Option<Arc<dyn crate::provider::AttachmentSource>>,
}

impl TurnActivities {
    /// Assemble the collection for worker registration.
    pub fn new(registry: Arc<AgentRegistry>, providers: Providers) -> Self {
        Self {
            registry,
            providers,
            attachments: None,
        }
    }

    /// Attach an mcp-bridge attachment source (the `preview` wiring; see
    /// [`crate::provider::AttachmentSource`]).
    pub fn with_attachments(
        mut self,
        attachments: Arc<dyn crate::provider::AttachmentSource>,
    ) -> Self {
        self.attachments = Some(attachments);
        self
    }

    /// The registry handle, shared with the workflow factory so workflow
    /// instances and activities resolve the same agents.
    pub fn registry_handle(&self) -> Arc<AgentRegistry> {
        self.registry.clone()
    }
}

#[activities]
impl TurnActivities {
    /// Execute one harness turn through the agent's provider.
    ///
    /// Retry recovery: on attempts after the first, the session id recorded
    /// in the prior attempt's heartbeat details upgrades a `Start` directive
    /// to `ResumeForked` — the model continues with the failed attempt's
    /// context instead of losing it. `ResumeForked` directives re-fork from
    /// the same parent unchanged (attempt isolation; module docs).
    #[activity]
    pub async fn execute_turn(
        self: Arc<Self>,
        ctx: ActivityContext,
        input: TurnActivityInput,
    ) -> Result<TurnActivityOutput, ActivityError> {
        let agent = self
            .registry
            .get(&input.agent)
            .map_err(|error| terminal_failure("odori::run::unknown_agent", &error))?;
        let provider = self
            .providers
            .resolve(agent.provider())
            .map_err(|error| terminal_failure("odori::run::unknown_provider", &error))?;

        let info = ctx.info();
        let attempt = info.attempt;
        let mut session = input.session;
        if attempt > 1
            && matches!(session, SessionDirective::Start)
            && let Ok(Some(prior)) = ctx.heartbeat_details().deserialize::<TurnHeartbeat>()
            && let Some(session_id) = prior.session_id
        {
            session = SessionDirective::ResumeForked { session_id };
        }

        let identity = TurnIdentity {
            run_id: info.workflow_run_id.clone().unwrap_or_default(),
            turn: input.turn,
            attempt,
        };
        let mut request =
            TurnRequest::new(identity.clone(), agent.directives(), input.input, session);
        request.tooling = agent.tooling();
        if let Some(source) = &self.attachments
            && let Some(attachment) = source.attachment_for(
                info.workflow_id.as_deref().unwrap_or_default(),
                &identity,
                agent.name(),
            )
        {
            request.tooling.mcp_servers.push(attachment.mcp_server);
            request.tooling.mcp_timeout = attachment.mcp_timeout;
            let allowed = request.tooling.allowed_native_tools.get_or_insert_default();
            allowed.extend(attachment.allowed_tools);
        }

        // Pump provider events into activity heartbeats; the session id
        // rides every heartbeat as the retry-recovery anchor.
        let (sender, mut receiver) = mpsc::channel::<TurnEvent>(64);
        let heartbeat_ctx = ctx.clone();
        let pump = tokio::spawn(async move {
            let mut state = TurnHeartbeat::default();
            while let Some(event) = receiver.recv().await {
                if let TurnEvent::SessionStarted { session_id } = &event {
                    state.session_id = Some(session_id.clone());
                }
                let _ = heartbeat_ctx.record_heartbeat(state.clone()).await;
            }
        });

        let outcome = provider
            .execute_turn(request, TurnEventSink::new(sender))
            .await;
        pump.abort();

        match outcome {
            Ok(outcome) => Ok(outcome.into()),
            Err(error) => Err(turn_failure(&error)),
        }
    }
}

fn terminal_failure(
    error_type: &str,
    error: &(dyn std::error::Error + Send + Sync),
) -> ActivityError {
    ActivityError::Application(Box::new(
        ApplicationFailure::builder(anyhow::anyhow!("{error}"))
            .type_name(error_type.to_owned())
            .non_retryable(true)
            .build(),
    ))
}

fn turn_failure(error: &TurnError) -> ActivityError {
    ActivityError::Application(Box::new(
        ApplicationFailure::builder(anyhow::anyhow!("{error}"))
            .type_name(error.error_type().to_owned())
            .non_retryable(!error.is_retryable())
            .build(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::TurnUsage;

    #[test]
    fn usage_absorbs_unknowns_as_zero() {
        let mut usage = RunUsage::default();
        usage.absorb(&TurnUsage::default());
        let reported = TurnUsage {
            total_cost_usd: Some(0.25),
            output_tokens: Some(10),
            ..TurnUsage::default()
        };
        usage.absorb(&reported);
        assert!((usage.total_cost_usd - 0.25).abs() < f64::EPSILON);
        assert_eq!(usage.output_tokens, 10);
        assert_eq!(usage.input_tokens, 0);
    }

    #[test]
    fn providers_resolve_default_named_and_unknown() {
        #[derive(Debug)]
        struct Fake(&'static str);
        #[async_trait::async_trait]
        impl Provider for Fake {
            fn name(&self) -> &str {
                self.0
            }
            async fn execute_turn(
                &self,
                _request: TurnRequest,
                _events: TurnEventSink,
            ) -> Result<TurnOutcome, TurnError> {
                Err(TurnError::Config {
                    message: "unused".into(),
                })
            }
        }
        let providers = Providers::new(Arc::new(Fake("claude"))).with(Arc::new(Fake("codex")));
        assert_eq!(providers.resolve(None).expect("default").name(), "claude");
        assert_eq!(
            providers.resolve(Some("codex")).expect("named").name(),
            "codex"
        );
        assert!(providers.resolve(Some("gemini")).is_err());
    }
}

/// The `tool_invoked` update's payload (mcp-bridge spec, update-payload
/// contract table): identity plus the call itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInvocation {
    /// Turn/attempt/call-id identity the bridge stamped.
    pub identity: InvocationId,
    /// Tool name, as the model called it (unqualified).
    pub tool: String,
    /// The call's arguments, verbatim JSON.
    pub arguments: serde_json::Value,
}

/// The `tool_invoked` update's reply.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolInvocationReply {
    /// The recorded result — fresh, joined, or replayed; the bridge cannot
    /// tell and must not care.
    Completed(ToolCallResult),
    /// The invocation was rejected before any work started.
    Rejected(InvocationRejection),
}

/// Why an invocation was rejected (mcp-bridge error table; the bridge maps
/// each onto its MCP surface).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InvocationRejection {
    /// Superseded attempt, unrecorded call (Requirement 4.2).
    Fenced,
    /// Tool not in the turn's tool set.
    UnknownTool,
    /// Turn index the workflow does not recognize, or no run in progress.
    UnknownTurn,
    /// Empty or malformed call id.
    InvalidCallId,
    /// The run was cancelled while the call awaited its result.
    RunCancelled,
}

/// Serialized input for one `execute_tool` activity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecuteToolInput {
    /// Agent whose tool set resolves the handler.
    pub agent: String,
    /// Tool name.
    pub tool: String,
    /// Arguments, verbatim.
    pub arguments: serde_json::Value,
    /// The invocation identity (rides into the handler's
    /// [`ToolContext`] as its idempotency key — spec Requirement 2.4).
    pub identity: InvocationId,
}

/// The activity collection executing framework tools durably (mcp-bridge
/// spec, Requirement 2).
#[derive(Debug)]
pub struct ToolActivities {
    registry: Arc<AgentRegistry>,
    max_result_bytes: usize,
}

/// Default ceiling on one tool result's serialized content (mcp-bridge
/// spec Q4, operator-decided 2026-08-20): results ride updates into
/// history and snapshots of the in-memory engine, so they are bounded here
/// — at the activity, before anything enters history. Oversized results
/// become model-visible `isError` results the model can adapt to.
pub const DEFAULT_MAX_RESULT_BYTES: usize = 256 * 1024;

impl ToolActivities {
    /// Assemble for worker registration, sharing the agent registry.
    pub fn new(registry: Arc<AgentRegistry>) -> Self {
        Self {
            registry,
            max_result_bytes: DEFAULT_MAX_RESULT_BYTES,
        }
    }

    /// Override the per-result size ceiling (spec Q4; wired from
    /// `BridgeConfig::max_result_bytes` by the engine bootstrap).
    pub fn with_max_result_bytes(mut self, max_result_bytes: usize) -> Self {
        self.max_result_bytes = max_result_bytes;
        self
    }
}

#[activities]
impl ToolActivities {
    /// Execute one framework tool through its registered handler.
    #[activity]
    pub async fn execute_tool(
        self: Arc<Self>,
        ctx: ActivityContext,
        input: ExecuteToolInput,
    ) -> Result<ToolCallResult, ActivityError> {
        let agent = self
            .registry
            .get(&input.agent)
            .map_err(|error| terminal_failure("odori::tool::unknown_agent", &error))?;
        let tool = agent
            .tools()
            .iter()
            .find(|tool| tool.name() == input.tool)
            .ok_or_else(|| {
                terminal_failure(
                    "odori::tool::unknown_tool",
                    &crate::agent::UnknownAgent {
                        name: input.tool.clone(),
                    },
                )
            })?;
        let context = ToolContext {
            run_id: ctx.info().workflow_run_id.clone().unwrap_or_default(),
            turn: input.identity.turn,
            attempt: input.identity.attempt,
            invocation_id: input.identity.call_id.clone(),
        };
        match tool.invoke(context, input.arguments).await {
            Ok(value) => Ok(cap_result(
                tool_output_to_result(value),
                self.max_result_bytes,
            )),
            Err(failure) => Err(tool_failure(&failure)),
        }
    }
}

/// Enforce the Q4 size policy: an oversized result is replaced by a
/// model-visible failure telling the model how to adapt, recorded like any
/// result so replays serve it identically. Terminal by design — retrying
/// reproduces the same size.
fn cap_result(result: ToolCallResult, max_result_bytes: usize) -> ToolCallResult {
    let size = result.content.to_string().len();
    if size <= max_result_bytes {
        return result;
    }
    ToolCallResult::error(format!(
        "tool result too large: {size} bytes exceeds the {max_result_bytes}-byte limit;          write large output to a file and return its path instead"
    ))
}

/// Render a handler's output value as an MCP content array: strings become
/// a text block verbatim; anything else is embedded as compact JSON text.
fn tool_output_to_result(value: serde_json::Value) -> ToolCallResult {
    let text = match value {
        serde_json::Value::String(text) => text,
        other => other.to_string(),
    };
    ToolCallResult::text(text)
}

fn tool_failure(failure: &ToolFailure) -> ActivityError {
    ActivityError::Application(Box::new(
        ApplicationFailure::builder(anyhow::anyhow!("{failure}"))
            .type_name("odori::tool::failed".to_owned())
            .non_retryable(!failure.retryable)
            .build(),
    ))
}

#[cfg(test)]
mod q4_tests {
    use super::*;

    #[test]
    fn oversized_results_become_model_visible_failures() {
        let big = ToolCallResult::text("x".repeat(1024));
        let capped = cap_result(big.clone(), 256);
        assert!(capped.is_error);
        let text = capped.content.to_string();
        assert!(
            text.contains("too large") && text.contains("256-byte limit"),
            "{text}"
        );
        // Under the cap: untouched.
        assert_eq!(cap_result(big.clone(), 1_000_000), big);
    }
}
