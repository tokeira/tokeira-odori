//! The call broker: one MCP `tools/call` turned into one workflow update,
//! with keepalive progress while it runs.
//!
//! This is the MCP tool server's core minus HTTP: [`ToolCallDispatcher`] abstracts the
//! workflow-update submission (the real client wraps the SDK; tests supply
//! a fake with controllable completion ordering), and [`CallBroker::call`]
//! is the only path a result can travel — which makes record-before-respond
//! structural: the broker returns exactly and only what a
//! *completed* update carried, and the update completes only after its
//! effects are in workflow history (`odori-agents`' handler contract).

use std::{fmt, sync::Arc, time::Duration};

use async_trait::async_trait;
use odori_agents::run::{AgentRun, ToolCallReply, ToolCallRequest};
use temporalio_client::{Client, WorkflowExecuteUpdateOptions};
use thiserror::Error;

/// Dispatches tool calls to a run and observes its completion for token cleanup.
#[async_trait]
pub trait ToolCallDispatcher: fmt::Debug + Send + Sync + 'static {
    /// Dispatch one tool call and await its reply.
    async fn dispatch(
        &self,
        workflow_id: &str,
        tool_call: ToolCallRequest,
    ) -> Result<ToolCallReply, McpToolServerError>;

    /// Wait until `workflow_id` reaches a terminal workflow outcome.
    ///
    /// Test clients that do not model lifecycle may use this default, which
    /// never declares a run terminal and therefore never permits token
    /// eviction. Production overrides it with the engine's close-event wait.
    async fn wait_for_terminal(&self, _workflow_id: &str) {
        std::future::pending::<()>().await;
    }
}

/// The production [`ToolCallDispatcher`]: the SDK client against the engine.
#[derive(Debug, Clone)]
pub struct WorkflowToolCallDispatcher {
    client: Client,
}

impl WorkflowToolCallDispatcher {
    /// Wrap a connected SDK client.
    pub fn new(client: Client) -> Self {
        Self { client }
    }
}

#[async_trait]
impl ToolCallDispatcher for WorkflowToolCallDispatcher {
    async fn dispatch(
        &self,
        workflow_id: &str,
        tool_call: ToolCallRequest,
    ) -> Result<ToolCallReply, McpToolServerError> {
        self.client
            .get_workflow_handle::<AgentRun>(workflow_id)
            .execute_update(
                AgentRun::tool_invoked,
                tool_call,
                WorkflowExecuteUpdateOptions::default(),
            )
            .await
            .map_err(|error| McpToolServerError::Engine {
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
                        "could not observe workflow terminal state; retaining MCP tokens"
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
    dispatcher: Arc<dyn ToolCallDispatcher>,
    keepalive: Duration,
}

impl CallBroker {
    /// A broker emitting keepalive ticks every `keepalive` while a call is
    /// pending. The cadence must sit strictly below the harness's MCP
    /// timeout pinned at spawn.
    pub fn new(dispatcher: Arc<dyn ToolCallDispatcher>, keepalive: Duration) -> Self {
        Self {
            dispatcher,
            keepalive,
        }
    }

    pub(crate) async fn wait_for_terminal(&self, workflow_id: &str) {
        self.dispatcher.wait_for_terminal(workflow_id).await;
    }

    /// Execute one call. `on_progress` fires every keepalive interval while
    /// the update is pending (the server layer turns each tick into an MCP
    /// progress notification); the return value is the completed update's
    /// reply, and nothing else can be returned.
    pub async fn call(
        &self,
        workflow_id: &str,
        tool_call: ToolCallRequest,
        mut on_progress: impl FnMut() + Send,
    ) -> Result<ToolCallReply, McpToolServerError> {
        let update = self.dispatcher.dispatch(workflow_id, tool_call);
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

/// MCP tool server infrastructure failures. Distinct
/// from tool failures, which travel inside a successful reply as
/// `isError: true` results.
#[derive(Debug, Clone, Error)]
#[non_exhaustive]
pub enum McpToolServerError {
    /// Missing or invalid bearer token; rejected before any processing.
    #[error("unauthorized")]
    Unauthorized,
    /// An MCP surface the MCP tool server does not serve (resources, prompts, …).
    #[error("unsupported method: {method}")]
    Unsupported {
        /// The JSON-RPC method requested.
        method: String,
    },
    /// Malformed tool call (empty call id, non-object arguments, bad
    /// frame).
    #[error("bad tool_call: {message}")]
    BadInvocation {
        /// What was malformed.
        message: String,
    },
    /// The update path failed: engine unreachable, serialization fault.
    /// The turn activity observing this fails retryable.
    #[error("engine update failed: {message}")]
    Engine {
        /// Underlying error text.
        message: String,
    },
}
