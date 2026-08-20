//! The `Tool` primitive: a typed, framework-owned capability.
//!
//! In v0 (mcp-bridge `preview` off) a `Tool` is *declarative*: the runner
//! delegates tool intent to the harness's native tooling
//! ([`crate::provider::TurnTooling::allowed_native_tools`]), and the
//! handler registered here is not executed. The declaration still matters —
//! it is the surface the O6 bridge turns durable: with `preview` on, each
//! invocation becomes a tokeira activity governed by this tool's
//! [`ToolPolicy`], executed through the registered handler. Registering the
//! full shape now means flipping the flag changes behaviour, not APIs
//! (mcp-bridge Requirement 8.4).

use std::{fmt, future::Future, pin::Pin, sync::Arc, time::Duration};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// Boxed future returned by tool handlers.
pub type ToolFuture = Pin<Box<dyn Future<Output = Result<Value, ToolFailure>> + Send>>;

type Handler = Arc<dyn Fn(ToolContext, Value) -> ToolFuture + Send + Sync>;

/// Execution context handed to every tool handler: the durable identity of
/// this invocation, usable directly as an idempotency key for the tool's
/// own side effects (mcp-bridge spec, Requirement 2.4).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ToolContext {
    /// Workflow run id of the owning run.
    pub run_id: String,
    /// Zero-based turn index.
    pub turn: u32,
    /// Turn-activity attempt that carried the call.
    pub attempt: u32,
    /// The harness call id (the invocation's identity within the turn).
    pub invocation_id: String,
}

/// A framework-owned tool: name, model-facing description, JSON Schema for
/// arguments, execution policy, and the handler the mcp-bridge will run as
/// a durable activity.
#[derive(Clone)]
pub struct Tool {
    name: String,
    description: String,
    input_schema: Value,
    policy: ToolPolicy,
    handler: Handler,
}

impl Tool {
    /// Define a tool. `input_schema` is a JSON Schema object describing the
    /// arguments the model must supply.
    pub fn new<F, Fut>(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
        handler: F,
    ) -> Self
    where
        F: Fn(ToolContext, Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, ToolFailure>> + Send + 'static,
    {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema,
            policy: ToolPolicy::default(),
            handler: Arc::new(move |context, args| Box::pin(handler(context, args))),
        }
    }

    /// Replace the execution policy.
    pub fn with_policy(mut self, policy: ToolPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// The tool's name as the model calls it.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The model-facing description.
    pub fn description(&self) -> &str {
        &self.description
    }

    /// The JSON Schema for the tool's arguments.
    pub fn input_schema(&self) -> &Value {
        &self.input_schema
    }

    /// The execution policy the bridge applies to this tool's activity.
    pub fn policy(&self) -> &ToolPolicy {
        &self.policy
    }

    /// Invoke the handler directly. The mcp-bridge's `execute_tool`
    /// activity is the intended caller; nothing in the `preview`-off path
    /// executes this.
    pub fn invoke(&self, context: ToolContext, args: Value) -> ToolFuture {
        (self.handler)(context, args)
    }
}

impl fmt::Debug for Tool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tool")
            .field("name", &self.name)
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

/// Per-tool durable-execution policy, mapped by the bridge onto the
/// `execute_tool` activity's options (mcp-bridge Requirement 2.2).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ToolPolicy {
    /// Activity start-to-close timeout for one execution.
    pub start_to_close: Duration,
    /// Overall schedule-to-close ceiling across retries, if bounded.
    pub schedule_to_close: Option<Duration>,
    /// Heartbeat timeout, for tools that report progress.
    pub heartbeat_timeout: Option<Duration>,
    /// Maximum execution attempts (1 = no retries). `None` defers to the
    /// engine default.
    pub max_attempts: Option<u32>,
}

impl Default for ToolPolicy {
    fn default() -> Self {
        Self {
            start_to_close: Duration::from_secs(60),
            schedule_to_close: None,
            heartbeat_timeout: None,
            max_attempts: None,
        }
    }
}

/// A tool handler's failure: what the model reads when an execution fails
/// terminally (surfaced as an MCP `isError` tool result by the bridge).
#[derive(Debug, Clone, Serialize, Deserialize, Error)]
#[error("{message}")]
pub struct ToolFailure {
    /// Model-facing description of the failure.
    pub message: String,
    /// Whether the bridge may retry the execution before surfacing it.
    pub retryable: bool,
}

impl ToolFailure {
    /// A failure the bridge may retry per the tool's policy.
    pub fn retryable(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: true,
        }
    }

    /// A terminal failure: surfaced to the model without further attempts.
    pub fn terminal(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: false,
        }
    }
}
