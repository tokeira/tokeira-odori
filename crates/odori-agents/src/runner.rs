//! The `Runner`: start runs, follow conversations, fetch typed results.
//!
//! The runner is pure client surface — it starts [`crate::run::AgentRun`]
//! workflow executions and interprets their results. The durable side (the
//! worker executing them) is registered separately via [`register_odori`]
//! and hosted by `odori-engine`'s bootstrap; a runner works identically
//! against the in-process embedded engine or a remote endpoint, because
//! both are just the Temporal contract.

use std::fmt;

use temporalio_client::{
    Client, WorkflowGetResultOptions, WorkflowSignalOptions, WorkflowStartOptions,
};
use temporalio_sdk::WorkerOptionsBuilder;
use thiserror::Error;

use crate::{
    output::{AgentOutput, OutputParseError},
    run::{AgentRun, RunConfig, RunEnd, RunInput, RunOutput, TurnActivities},
};

/// Register Odori's workflow and activities on a worker under assembly.
///
/// `activities` carries the worker-side state (agent registry, providers);
/// the same registry is injected into every [`AgentRun`] instance through
/// the workflow factory (see `crate::run`'s determinism note).
pub fn register_odori(
    options: WorkerOptionsBuilder,
    activities: TurnActivities,
) -> Result<WorkerOptionsBuilder, temporalio_sdk::WorkflowRegistrationError> {
    let registry = activities.registry_handle();
    let tool_activities = crate::run::ToolActivities::new(registry.clone());
    Ok(options
        .register_workflow_with_factory::<AgentRun, _>(move || {
            AgentRun::with_registry(registry.clone())
        })?
        .register_activities(activities)
        .register_activities(tool_activities))
}

/// Starts and follows agent runs against a connected client.
#[derive(Clone)]
pub struct Runner {
    client: Client,
    task_queue: String,
}

impl fmt::Debug for Runner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Runner")
            .field("task_queue", &self.task_queue)
            .finish_non_exhaustive()
    }
}

impl Runner {
    /// A runner submitting to `task_queue` through `client`.
    pub fn new(client: Client, task_queue: impl Into<String>) -> Self {
        Self {
            client,
            task_queue: task_queue.into(),
        }
    }

    /// Run an agent once: one turn, one typed result.
    ///
    /// `run_id` names the workflow execution (idempotency key for the whole
    /// run: re-issuing the same id joins rather than duplicates).
    pub async fn run<O: AgentOutput>(
        &self,
        agent: &str,
        prompt: &str,
        run_id: &str,
    ) -> Result<O, RunnerError> {
        let config = RunConfig {
            interactive: false,
            ..RunConfig::default()
        };
        let output = self.start_and_await(agent, prompt, run_id, config).await?;
        Self::interpret(output)
    }

    /// Run with explicit configuration (budgets, timeouts, interactivity).
    pub async fn run_with_config<O: AgentOutput>(
        &self,
        agent: &str,
        prompt: &str,
        run_id: &str,
        config: RunConfig,
    ) -> Result<O, RunnerError> {
        let output = self.start_and_await(agent, prompt, run_id, config).await?;
        Self::interpret(output)
    }

    /// Start an interactive run and return a handle for the conversation.
    pub async fn start_conversation(
        &self,
        agent: &str,
        prompt: &str,
        run_id: &str,
    ) -> Result<Conversation, RunnerError> {
        let config = RunConfig {
            interactive: true,
            ..RunConfig::default()
        };
        let handle = self
            .client
            .start_workflow(
                AgentRun::run,
                RunInput {
                    agent: agent.to_owned(),
                    prompt: prompt.to_owned(),
                    config,
                },
                WorkflowStartOptions::new(self.task_queue.clone(), run_id.to_owned()).build(),
            )
            .await
            .map_err(|error| RunnerError::Client {
                message: error.to_string(),
            })?;
        Ok(Conversation { handle })
    }

    async fn start_and_await(
        &self,
        agent: &str,
        prompt: &str,
        run_id: &str,
        config: RunConfig,
    ) -> Result<RunOutput, RunnerError> {
        let handle = self
            .client
            .start_workflow(
                AgentRun::run,
                RunInput {
                    agent: agent.to_owned(),
                    prompt: prompt.to_owned(),
                    config,
                },
                WorkflowStartOptions::new(self.task_queue.clone(), run_id.to_owned()).build(),
            )
            .await
            .map_err(|error| RunnerError::Client {
                message: error.to_string(),
            })?;
        handle
            .get_result(WorkflowGetResultOptions::default())
            .await
            .map_err(|error| RunnerError::Run {
                message: error.to_string(),
            })
    }

    fn interpret<O: AgentOutput>(output: RunOutput) -> Result<O, RunnerError> {
        match output.end {
            RunEnd::Completed | RunEnd::ConversationEnded => {
                O::parse(&output.text).map_err(RunnerError::Output)
            }
            RunEnd::BudgetExceeded { cap } => Err(RunnerError::BudgetExceeded {
                cap: format!("{cap:?}"),
                partial: Box::new(output),
            }),
            RunEnd::GuardrailBlocked { guardrail, reason } => {
                Err(RunnerError::GuardrailBlocked { guardrail, reason })
            }
        }
    }
}

/// A live interactive run.
pub struct Conversation {
    handle: temporalio_client::WorkflowHandle<
        Client,
        <AgentRun as temporalio_workflow::runtime::entry::WorkflowImplementation>::Run,
    >,
}

impl fmt::Debug for Conversation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Conversation").finish_non_exhaustive()
    }
}

impl Conversation {
    /// Queue the next user message as a turn.
    pub async fn send(&self, message: &str) -> Result<(), RunnerError> {
        self.handle
            .signal(
                AgentRun::user_message,
                message.to_owned(),
                WorkflowSignalOptions::default(),
            )
            .await
            .map_err(|error| RunnerError::Client {
                message: error.to_string(),
            })
    }

    /// End the conversation and await the run's final output.
    pub async fn end(self) -> Result<RunOutput, RunnerError> {
        self.handle
            .signal(
                AgentRun::end_conversation,
                (),
                WorkflowSignalOptions::default(),
            )
            .await
            .map_err(|error| RunnerError::Client {
                message: error.to_string(),
            })?;
        self.handle
            .get_result(WorkflowGetResultOptions::default())
            .await
            .map_err(|error| RunnerError::Run {
                message: error.to_string(),
            })
    }
}

/// Failures surfaced by the runner.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum RunnerError {
    /// The request never reached a running workflow (connection, start, or
    /// signal failure).
    #[error("client error: {message}")]
    Client {
        /// Underlying client error text.
        message: String,
    },
    /// The run itself failed (turn retries exhausted, workflow failure).
    #[error("run failed: {message}")]
    Run {
        /// Underlying failure text.
        message: String,
    },
    /// The run completed but its text did not parse as the requested type.
    #[error(transparent)]
    Output(OutputParseError),
    /// A budget cap ended the run before completion.
    #[error("run budget exceeded ({cap})")]
    BudgetExceeded {
        /// The cap that tripped.
        cap: String,
        /// The partial run output, for inspection.
        partial: Box<RunOutput>,
    },
    /// A guardrail rejected the input or an output.
    #[error("guardrail {guardrail:?} blocked the run: {reason}")]
    GuardrailBlocked {
        /// The tripped guardrail.
        guardrail: String,
        /// Its stated reason.
        reason: String,
    },
}
