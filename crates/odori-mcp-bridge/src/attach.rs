//! Bridge lifecycle and harness attachment: mint per-attempt bearer
//! tokens, keep the token → turn-context directory, and hand providers the
//! MCP config that points their harness at the in-process server.
//!
//! Tokens are **per turn-attempt**, not per run. This is load-bearing for
//! fencing (spec Requirement 4, Property 4): the attempt stamped on an
//! invocation must be the attempt that *spawned the harness making the
//! call*, so a zombie harness keeps presenting its stale token and its
//! calls carry the superseded attempt the registry fences. A per-run token
//! would stamp zombie calls with the current attempt and dissolve the
//! fence — which resolves spec open question Q5 in favour of per-attempt
//! tokens.

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::Duration,
};

use odori_agents::{
    agent::AgentRegistry,
    provider::{AttachmentSource, McpServerConfig, McpTransport, TurnAttachment, TurnIdentity},
};
use serde_json::{Value, json};

use crate::broker::{CallBroker, UpdateClient};

/// Bridge configuration.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct BridgeConfig {
    /// Keepalive cadence while a call is pending. Must sit strictly below
    /// `mcp_timeout_pin` (Property 5); [`BridgeConfig::validate`] enforces
    /// the invariant.
    pub keepalive: Duration,
    /// The MCP client timeout the provider pins on the harness at spawn
    /// (spec Requirement 5.3).
    pub mcp_timeout_pin: Option<Duration>,
    /// Model-visible MCP server name (spec Q7 draft: `odori`).
    pub server_name: String,
    /// Ceiling on one tool result's serialized content (spec Q4,
    /// operator-decided: cap-and-fail). Enforced at the `execute_tool`
    /// activity, before anything enters history; oversized results become
    /// model-visible `isError` results.
    pub max_result_bytes: usize,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            keepalive: Duration::from_secs(10),
            mcp_timeout_pin: Some(Duration::from_secs(120)),
            server_name: "odori".to_owned(),
            max_result_bytes: odori_agents::run::DEFAULT_MAX_RESULT_BYTES,
        }
    }
}

impl BridgeConfig {
    /// Check the keepalive-below-timeout invariant (spec I6).
    pub fn validate(&self) -> anyhow::Result<()> {
        if let Some(pin) = self.mcp_timeout_pin
            && self.keepalive >= pin
        {
            anyhow::bail!(
                "keepalive cadence ({:?}) must be strictly below the pinned MCP timeout ({:?})",
                self.keepalive,
                pin
            );
        }
        Ok(())
    }
}

/// One attachment's frozen coordinates, resolved by bearer token.
#[derive(Debug, Clone)]
pub(crate) struct RunContext {
    pub(crate) workflow_id: String,
    pub(crate) agent: String,
    pub(crate) turn: u32,
    pub(crate) attempt: u32,
}

pub(crate) struct BridgeInner {
    registry: Arc<AgentRegistry>,
    broker: CallBroker,
    config: BridgeConfig,
    /// Bearer token → the attachment it was minted for. Entries remain until
    /// the workflow is confirmed terminal: while a run is live, a stale token
    /// must keep resolving so its calls reach the registry and get *fenced*
    /// (a 401 would be indistinguishable from misconfiguration).
    directory: Mutex<HashMap<String, RunContext>>,
    /// Workflow ids with one active close-event observer. One observer owns
    /// eviction for all turn-attempt tokens belonging to the run.
    terminal_watchers: Mutex<HashSet<String>>,
}

impl std::fmt::Debug for BridgeInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BridgeInner")
            .field("server_name", &self.config.server_name)
            .finish_non_exhaustive()
    }
}

impl BridgeInner {
    pub(crate) fn server_name(&self) -> &str {
        &self.config.server_name
    }

    pub(crate) fn broker(&self) -> &CallBroker {
        &self.broker
    }

    pub(crate) fn context_for_token(&self, token: &str) -> Option<RunContext> {
        self.directory
            .lock()
            .expect("directory lock")
            .get(token)
            .cloned()
    }

    fn watch_terminal(self: &Arc<Self>, workflow_id: &str) {
        if !self
            .terminal_watchers
            .lock()
            .expect("terminal watcher lock")
            .insert(workflow_id.to_owned())
        {
            return;
        }

        let inner = Arc::downgrade(self);
        let broker = self.broker.clone();
        let workflow_id = workflow_id.to_owned();
        tokio::spawn(async move {
            broker.wait_for_terminal(&workflow_id).await;
            let Some(inner) = inner.upgrade() else {
                return;
            };
            let removed = {
                let mut directory = inner.directory.lock().expect("directory lock");
                let before = directory.len();
                directory.retain(|_, context| context.workflow_id != workflow_id);
                before - directory.len()
            };
            inner
                .terminal_watchers
                .lock()
                .expect("terminal watcher lock")
                .remove(&workflow_id);
            tracing::debug!(workflow_id, removed, "evicted terminal run bridge tokens");
        });
    }

    /// The `tools/list` payload for the context's agent: unqualified names
    /// on the wire (the `mcp__{server}__{tool}` form is harness-side
    /// namespacing).
    pub(crate) fn tool_listing(&self, context: &RunContext) -> Value {
        let tools: Vec<Value> = self
            .registry
            .get(&context.agent)
            .map(|agent| {
                agent
                    .tools()
                    .iter()
                    .map(|tool| {
                        json!({
                            "name": tool.name(),
                            "description": tool.description(),
                            "inputSchema": tool.input_schema(),
                        })
                    })
                    .chain(agent.handoffs().iter().map(|handoff| {
                        json!({
                            "name": handoff.tool_name(),
                            "description": handoff.description(),
                            "inputSchema": handoff.input_schema(),
                        })
                    }))
                    .collect()
            })
            .unwrap_or_default();
        json!({ "tools": tools })
    }
}

/// The running bridge: an HTTP listener plus the attachment directory. Also
/// the [`AttachmentSource`] handed to the turn activities.
#[derive(Debug, Clone)]
pub struct Bridge {
    inner: Arc<BridgeInner>,
    url: String,
}

impl Bridge {
    /// Bind the loopback listener and start serving. The endpoint lives at
    /// an ephemeral port; [`Bridge::url`] reports it.
    pub async fn start(
        registry: Arc<AgentRegistry>,
        client: Arc<dyn UpdateClient>,
        config: BridgeConfig,
    ) -> anyhow::Result<Self> {
        config.validate()?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("http://{}/mcp", listener.local_addr()?);
        let inner = Arc::new(BridgeInner {
            registry,
            broker: CallBroker::new(client, config.keepalive),
            config,
            directory: Mutex::new(HashMap::new()),
            terminal_watchers: Mutex::new(HashSet::new()),
        });
        tokio::spawn(crate::server::serve(inner.clone(), listener));
        Ok(Self { inner, url })
    }

    /// The bridge endpoint harnesses connect to.
    pub fn url(&self) -> &str {
        &self.url
    }
}

impl AttachmentSource for Bridge {
    fn attachment_for(
        &self,
        workflow_id: &str,
        identity: &TurnIdentity,
        agent_name: &str,
    ) -> Option<TurnAttachment> {
        let agent = self.inner.registry.get(agent_name).ok()?;
        if agent.tools().is_empty() && agent.handoffs().is_empty() {
            return None;
        }
        // A fresh token per attempt (module docs: fencing depends on it).
        let token = format!("odori-{}", uuid::Uuid::new_v4());
        self.inner.directory.lock().expect("directory lock").insert(
            token.clone(),
            RunContext {
                workflow_id: workflow_id.to_owned(),
                agent: agent_name.to_owned(),
                turn: identity.turn,
                attempt: identity.attempt,
            },
        );
        self.inner.watch_terminal(workflow_id);
        let server = &self.inner.config.server_name;
        Some(TurnAttachment::new(
            McpServerConfig {
                name: server.clone(),
                transport: McpTransport::Http {
                    url: self.url.clone(),
                    headers: vec![("Authorization".to_owned(), format!("Bearer {token}"))],
                },
            },
            self.inner.config.mcp_timeout_pin,
            agent
                .tools()
                .iter()
                .map(|tool| format!("mcp__{server}__{}", tool.name()))
                .chain(
                    agent
                        .handoffs()
                        .iter()
                        .map(|handoff| format!("mcp__{server}__{}", handoff.tool_name())),
                )
                .collect(),
        ))
    }
}
