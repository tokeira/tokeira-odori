//! The call broker: one MCP `tools/call` turned into one workflow update,
//! with keepalive progress while it runs.
//!
//! This is the bridge's core minus HTTP: [`UpdateClient`] abstracts the
//! workflow-update submission (the real client wraps the SDK; tests supply
//! a fake with controllable completion ordering), and [`CallBroker::call`]
//! is the only path a result can travel — which makes record-before-respond
//! (Property 2) structural: the broker returns exactly and only what a
//! *completed* update carried, and the update completes only after its
//! effects are in workflow history (`odori-agents`' handler contract).

use std::{fmt, sync::Arc, time::Duration};

use async_trait::async_trait;
use odori_agents::run::{AgentRun, ToolInvocation, ToolInvocationReply};
use temporalio_client::{Client, WorkflowExecuteUpdateOptions};
use thiserror::Error;

/// Submits `tool_invoked` updates to a run's workflow.
#[async_trait]
pub trait UpdateClient: fmt::Debug + Send + Sync + 'static {
    /// Execute the update to completion and return its reply.
    async fn tool_invoked(
        &self,
        workflow_id: &str,
        invocation: ToolInvocation,
    ) -> Result<ToolInvocationReply, BridgeError>;

    /// Wait until `workflow_id` reaches a terminal workflow outcome.
    ///
    /// Test clients that do not model lifecycle may use this default, which
    /// never declares a run terminal and therefore never permits token
    /// eviction. Production overrides it with the engine's close-event wait.
    async fn wait_for_terminal(&self, _workflow_id: &str) {
        std::future::pending::<()>().await;
    }
}

/// The production [`UpdateClient`]: the SDK client against the engine.
#[derive(Debug, Clone)]
pub struct WorkflowUpdateClient {
    client: Client,
}

impl WorkflowUpdateClient {
    /// Wrap a connected SDK client.
    pub fn new(client: Client) -> Self {
        Self { client }
    }
}

#[async_trait]
impl UpdateClient for WorkflowUpdateClient {
    async fn tool_invoked(
        &self,
        workflow_id: &str,
        invocation: ToolInvocation,
    ) -> Result<ToolInvocationReply, BridgeError> {
        self.client
            .get_workflow_handle::<AgentRun>(workflow_id)
            .execute_update(
                AgentRun::tool_invoked,
                invocation,
                WorkflowExecuteUpdateOptions::default(),
            )
            .await
            .map_err(|error| BridgeError::Engine {
                message: error.to_string(),
            })
    }

    async fn wait_for_terminal(&self, workflow_id: &str) {
        loop {
            let result = self
                .client
                .get_workflow_handle::<AgentRun>(workflow_id)
                .get_result(Default::default())
                .await;
            match result {
                Ok(_) => return,
                Err(error) if error.is_workflow_outcome() => return,
                Err(error) => {
                    // Never convert an infrastructure observation failure
                    // into eviction: a still-live run's stale token must keep
                    // resolving so the workflow can fence it, not become 401.
                    tracing::warn!(
                        workflow_id,
                        %error,
                        "could not observe workflow terminal state; retaining bridge tokens"
                    );
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    }
}

/// Drives one call: submit the update, emit keepalive ticks while it is
/// pending, return its reply.
#[derive(Debug, Clone)]
pub struct CallBroker {
    client: Arc<dyn UpdateClient>,
    keepalive: Duration,
}

impl CallBroker {
    /// A broker emitting keepalive ticks every `keepalive` while a call is
    /// pending. The cadence must sit strictly below the harness's MCP
    /// timeout pinned at spawn.
    pub fn new(client: Arc<dyn UpdateClient>, keepalive: Duration) -> Self {
        Self { client, keepalive }
    }

    pub(crate) async fn wait_for_terminal(&self, workflow_id: &str) {
        self.client.wait_for_terminal(workflow_id).await;
    }

    /// Execute one call. `on_progress` fires every keepalive interval while
    /// the update is pending (the server layer turns each tick into an MCP
    /// progress notification); the return value is the completed update's
    /// reply, and nothing else can be returned — Property 2 by
    /// construction.
    pub async fn call(
        &self,
        workflow_id: &str,
        invocation: ToolInvocation,
        mut on_progress: impl FnMut() + Send,
    ) -> Result<ToolInvocationReply, BridgeError> {
        let update = self.client.tool_invoked(workflow_id, invocation);
        tokio::pin!(update);
        let mut ticker = tokio::time::interval(self.keepalive);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // An interval's first tick completes immediately; consume it so the
        // first keepalive fires one cadence in.
        ticker.tick().await;
        loop {
            tokio::select! {
                reply = &mut update => return reply,
                _ = ticker.tick() => on_progress(),
            }
        }
    }
}

/// Bridge-level failures (the spec's error table, bridge column). Distinct
/// from tool failures, which travel inside a successful reply as
/// `isError: true` results.
#[derive(Debug, Clone, Error)]
#[non_exhaustive]
pub enum BridgeError {
    /// Missing or invalid bearer token; rejected before any processing.
    #[error("unauthorized")]
    Unauthorized,
    /// An MCP surface the bridge does not serve (resources, prompts, …).
    #[error("unsupported method: {method}")]
    Unsupported {
        /// The JSON-RPC method requested.
        method: String,
    },
    /// Malformed invocation (empty call id, non-object arguments, bad
    /// frame).
    #[error("bad invocation: {message}")]
    BadInvocation {
        /// What was malformed.
        message: String,
    },
    /// The update path failed: engine unreachable, serialization fault.
    /// The turn activity observing this fails retryable (Requirement 6.4).
    #[error("engine update failed: {message}")]
    Engine {
        /// Underlying error text.
        message: String,
    },
}
