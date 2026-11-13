//! The `Agent` primitive and the worker-side registry.
//!
//! An [`Agent`] is a named configuration: instructions, a provider binding,
//! tools, guardrails, and an output shape. Agents hold live objects
//! (handlers, guardrail impls), so they are **not** serialized into
//! workflow inputs — runs reference agents by name, and the worker resolves
//! the name through an [`AgentRegistry`] shared with the turn activities.
//! The serializable slice a backend needs travels as
//! [`crate::provider::AgentDirectives`].

use std::{collections::HashMap, sync::Arc};

use serde_json::Value;
use thiserror::Error;

use crate::{
    guardrail::{Guardrail, RunBudget},
    handoff::Handoff,
    provider::{AgentDirectives, ReasoningEffort, ToolConfig},
    tool::Tool,
};

/// A named agent configuration.
#[derive(Debug, Clone)]
pub struct Agent {
    name: String,
    instructions: String,
    provider: Option<String>,
    model: Option<String>,
    effort: Option<ReasoningEffort>,
    tools: Vec<Tool>,
    handoffs: Vec<Handoff>,
    allowed_tools: Option<Vec<String>>,
    input_guardrails: Vec<Arc<dyn Guardrail>>,
    output_guardrails: Vec<Arc<dyn Guardrail>>,
    output_schema: Option<Value>,
    budget: RunBudget,
}

impl Agent {
    /// A new agent with the two required properties.
    pub fn new(name: impl Into<String>, instructions: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            instructions: instructions.into(),
            provider: None,
            model: None,
            effort: None,
            tools: Vec::new(),
            handoffs: Vec::new(),
            allowed_tools: None,
            input_guardrails: Vec::new(),
            output_guardrails: Vec::new(),
            output_schema: None,
            budget: RunBudget::unlimited(),
        }
    }

    /// Bind the agent to a provider by [`crate::provider::Provider::name`].
    /// Unbound agents use the runtime's default provider.
    pub fn with_provider(mut self, provider: impl Into<String>) -> Self {
        self.provider = Some(provider.into());
        self
    }

    /// Select the backend model, verbatim.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Select a provider-neutral reasoning effort. Each provider maps the
    /// level onto its own lever or rejects it before spawn with a typed
    /// configuration error ([`ReasoningEffort`]'s contract); unset means the
    /// backend's default.
    pub fn with_effort(mut self, effort: ReasoningEffort) -> Self {
        self.effort = Some(effort);
        self
    }

    /// Register a framework-owned tool (durable under the mcp-bridge; see
    /// [`crate::tool`]).
    pub fn with_tool(mut self, tool: Tool) -> Self {
        self.tools.push(tool);
        self
    }

    /// Offer a durable handoff to another registered agent.
    pub fn with_handoff(mut self, handoff: Handoff) -> Self {
        self.handoffs.push(handoff);
        self
    }

    /// Configure allowed native and MCP tool names using backend naming
    /// (Claude Code: `--allowedTools`). Enforcement is provider-specific;
    /// the runner also adds names exposed by the MCP bootstrapper.
    pub fn with_allowed_tools(
        mut self,
        tools: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.allowed_tools = Some(tools.into_iter().map(Into::into).collect());
        self
    }

    /// Add a guardrail over run input (checked before the first turn).
    pub fn with_input_guardrail(mut self, guardrail: impl Guardrail) -> Self {
        self.input_guardrails.push(Arc::new(guardrail));
        self
    }

    /// Add a guardrail over turn output (checked after every turn).
    pub fn with_output_guardrail(mut self, guardrail: impl Guardrail) -> Self {
        self.output_guardrails.push(Arc::new(guardrail));
        self
    }

    /// Require the final output to satisfy a JSON Schema, enforced at the
    /// backend where possible and parsed runner-side either way (see
    /// [`crate::output`]).
    pub fn with_output_schema(mut self, schema: Value) -> Self {
        self.output_schema = Some(schema);
        self
    }

    /// Apply agent-level run caps. Runner-level caps compose by taking the
    /// stricter value for each dimension.
    pub fn with_budget(mut self, budget: RunBudget) -> Self {
        self.budget = budget;
        self
    }

    /// The agent's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The provider binding, if any.
    pub fn provider(&self) -> Option<&str> {
        self.provider.as_deref()
    }

    /// The registered framework-owned tools.
    pub fn tools(&self) -> &[Tool] {
        &self.tools
    }

    /// Registered handoffs exposed as framework tools.
    pub fn handoffs(&self) -> &[Handoff] {
        &self.handoffs
    }

    /// Guardrails applied to run input.
    pub fn input_guardrails(&self) -> &[Arc<dyn Guardrail>] {
        &self.input_guardrails
    }

    /// Guardrails applied to turn output.
    pub fn output_guardrails(&self) -> &[Arc<dyn Guardrail>] {
        &self.output_guardrails
    }

    /// This agent's own run caps.
    pub fn budget(&self) -> &RunBudget {
        &self.budget
    }

    /// The serializable directives a provider receives for this agent.
    pub fn directives(&self) -> AgentDirectives {
        AgentDirectives {
            name: self.name.clone(),
            instructions: self.instructions.clone(),
            model: self.model.clone(),
            effort: self.effort,
            output_schema: self.output_schema.clone(),
        }
    }

    /// The tool configuration derived from this agent's configuration. MCP
    /// bootstrap configuration is layered on by the caller; this carries
    /// the agent-declared parts.
    pub fn tool_config(&self) -> ToolConfig {
        ToolConfig {
            allowed_tools: self.allowed_tools.clone(),
            framework_tool_names: self
                .tools
                .iter()
                .map(|tool| tool.name().to_owned())
                .chain(
                    self.handoffs
                        .iter()
                        .map(|handoff| handoff.tool_name().to_owned()),
                )
                .collect(),
            ..ToolConfig::default()
        }
    }
}

/// Worker-side lookup from agent name to configuration.
///
/// Shared (via `Arc`) between the application that registers agents and the
/// turn activities that resolve them per run.
#[derive(Debug, Default, Clone)]
pub struct AgentRegistry {
    agents: HashMap<String, Arc<Agent>>,
}

impl AgentRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an agent under its name. Re-registering a name replaces the
    /// prior agent.
    pub fn register(&mut self, agent: Agent) -> &mut Self {
        self.agents.insert(agent.name().to_owned(), Arc::new(agent));
        self
    }

    /// Look up an agent by name.
    pub fn get(&self, name: &str) -> Result<Arc<Agent>, UnknownAgent> {
        self.agents.get(name).cloned().ok_or_else(|| UnknownAgent {
            name: name.to_owned(),
        })
    }

    /// Names of every registered agent.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.agents.keys().map(String::as_str)
    }
}

/// A run referenced an agent name the worker has not registered.
#[derive(Debug, Clone, Error)]
#[error("no agent registered under the name {name:?}")]
pub struct UnknownAgent {
    /// The unresolved name.
    pub name: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guardrail::GuardrailVerdict;

    #[derive(Debug)]
    struct NoPirates;
    impl Guardrail for NoPirates {
        fn name(&self) -> &str {
            "no-pirates"
        }
        fn check(&self, text: &str) -> GuardrailVerdict {
            if text.contains("pirate") {
                GuardrailVerdict::Block {
                    reason: "pirates".into(),
                }
            } else {
                GuardrailVerdict::Pass
            }
        }
    }

    #[test]
    fn registry_resolves_and_rejects() {
        let mut registry = AgentRegistry::new();
        registry.register(
            Agent::new("helper", "assist")
                .with_model("m1")
                .with_effort(ReasoningEffort::High)
                .with_input_guardrail(NoPirates)
                .with_handoff(Handoff::new("specialist"))
                .with_allowed_tools(["Bash"]),
        );
        let agent = registry.get("helper").expect("registered");
        assert_eq!(agent.directives().model.as_deref(), Some("m1"));
        assert_eq!(agent.directives().effort, Some(ReasoningEffort::High));
        assert_eq!(
            agent.tool_config().allowed_tools,
            Some(vec!["Bash".to_owned()])
        );
        assert_eq!(agent.input_guardrails().len(), 1);
        assert_eq!(
            agent.tool_config().framework_tool_names,
            vec!["transfer_to_specialist".to_owned()]
        );
        assert!(registry.get("missing").is_err());
    }
}
