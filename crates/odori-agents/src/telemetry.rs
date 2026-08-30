//! GenAI-convention span emission for runs, turns, and tool executions.
//!
//! Odori speaks only the [`tracing`] facade: it never constructs an
//! OpenTelemetry pipeline, an exporter, or a subscriber. The **host**
//! process owns all of that — install any `tracing` subscriber (a
//! `tracing-opentelemetry` layer over OTLP, Pydantic's `logfire` crate, a
//! plain fmt logger) and every span here flows through it. With no
//! subscriber installed the macros are disabled and cost almost nothing.
//!
//! ## Shape and naming
//!
//! One run is one `invoke_agent` span (opened client-side by the
//! [`crate::runner::Runner`]), each harness turn is one `chat` span, and
//! each durable framework-tool execution is one `execute_tool` span —
//! the OpenTelemetry GenAI semantic-convention operations. `tracing`
//! span names must be static, so the convention's dynamic display names
//! (`"chat gpt-5"`) ride the `otel.name` field, which
//! `tracing-opentelemetry` applies as the exported span's name. Backends
//! reading raw `tracing` output see the static name plus the field.
//!
//! ## Redaction posture
//!
//! Redacted by default: prompts, turn text, tool arguments, and tool
//! results are **never** attached to spans. What is attached: names,
//! identifiers (run, session, call), token/cost accounting, and error
//! classes — the same information the run already records as history.
//!
//! ## Parenting across the client/worker seam
//!
//! A turn executes as an engine-dispatched activity, not as a child task
//! of the runner's future, so span context cannot flow task-locally and
//! Odori deliberately persists no trace context into workflow history.
//! Instead the runner registers its live run span in a **process-local**
//! registry keyed by workflow id, and activity spans adopt it as their
//! explicit parent. In Odori's flagship embedded mode runner and worker
//! share a process, so the full run → turn → tool tree assembles. When
//! they do not share a process (a remote engine with a detached worker),
//! lookups miss and turn/tool spans export as trace roots that still
//! carry `odori.run.id` for attribute-level correlation. The registry
//! holds telemetry state only — nothing here touches history, replay, or
//! determinism.

use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};

use tracing::{Instrument as _, Span, field};

use crate::{
    provider::TurnError,
    run::{RunEnd, RunOutput, TurnActivityOutput},
};

fn run_registry() -> &'static Mutex<HashMap<String, Span>> {
    static RUNS: OnceLock<Mutex<HashMap<String, Span>>> = OnceLock::new();
    RUNS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn turn_registry() -> &'static Mutex<HashMap<String, Span>> {
    static TURNS: OnceLock<Mutex<HashMap<String, Span>>> = OnceLock::new();
    TURNS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lookup(registry: &'static Mutex<HashMap<String, Span>>, key: &str) -> Option<Span> {
    registry
        .lock()
        .expect("telemetry span registry lock")
        .get(key)
        .cloned()
}

/// A live span registered for parent lookup, deregistered on drop.
///
/// Registration is last-writer-wins: a retried turn attempt replaces the
/// prior attempt's entry, and an overlapping zombie attempt can at worst
/// deregister its successor early — the affected spans then fall back to
/// the next parent tier. Telemetry stays best-effort; execution state is
/// never read from here.
#[derive(Debug)]
pub(crate) struct RegisteredSpan {
    key: String,
    registry: &'static Mutex<HashMap<String, Span>>,
}

impl RegisteredSpan {
    fn insert(registry: &'static Mutex<HashMap<String, Span>>, key: &str, span: &Span) -> Self {
        registry
            .lock()
            .expect("telemetry span registry lock")
            .insert(key.to_owned(), span.clone());
        Self {
            key: key.to_owned(),
            registry,
        }
    }
}

impl Drop for RegisteredSpan {
    fn drop(&mut self) {
        self.registry
            .lock()
            .expect("telemetry span registry lock")
            .remove(&self.key);
    }
}

/// The client-side telemetry a [`crate::runner::Conversation`] carries: the
/// run span plus its registry entry, kept alive for the conversation's
/// lifetime so worker-side turns keep finding their parent.
#[derive(Debug)]
pub(crate) struct ClientRunTelemetry {
    pub(crate) span: Span,
    _registration: RegisteredSpan,
}

impl ClientRunTelemetry {
    /// Open and register the `invoke_agent` span for one run.
    pub(crate) fn start(agent: &str, run_id: &str) -> Self {
        let span = tracing::info_span!(
            "invoke_agent",
            otel.name = format!("invoke_agent {agent}"),
            gen_ai.operation.name = "invoke_agent",
            gen_ai.agent.name = agent,
            odori.run.id = run_id,
            gen_ai.usage.input_tokens = field::Empty,
            gen_ai.usage.output_tokens = field::Empty,
            operation.cost = field::Empty,
            odori.run.turns = field::Empty,
            odori.run.end = field::Empty,
            otel.status_code = field::Empty,
        );
        let registration = RegisteredSpan::insert(run_registry(), run_id, &span);
        Self {
            span,
            _registration: registration,
        }
    }

    /// Run a client future inside the run span.
    pub(crate) async fn scope<T>(&self, future: impl Future<Output = T>) -> T {
        future.instrument(self.span.clone()).await
    }

    /// Record the run's terminal accounting and outcome class.
    pub(crate) fn record_completion(&self, output: &RunOutput) {
        self.span
            .record("gen_ai.usage.input_tokens", output.usage.input_tokens);
        self.span
            .record("gen_ai.usage.output_tokens", output.usage.output_tokens);
        self.span
            .record("operation.cost", output.usage.total_cost_usd);
        self.span.record("odori.run.turns", output.turns);
        let end = match output.end {
            RunEnd::Completed => "completed",
            RunEnd::ConversationEnded => "conversation_ended",
            RunEnd::BudgetExceeded { .. } => "budget_exceeded",
            RunEnd::GuardrailBlocked { .. } => "guardrail_blocked",
        };
        self.span.record("odori.run.end", end);
        self.span.record("otel.status_code", "OK");
    }

    /// Record that the client call itself failed (start, signal, or await).
    pub(crate) fn record_client_failure(&self) {
        self.span.record("otel.status_code", "ERROR");
    }
}

/// Open the `chat` span for one turn-activity attempt, parented to the
/// registered run span when the runner shares this process.
pub(crate) fn turn_span(
    workflow_id: &str,
    agent: &str,
    provider: &str,
    model: Option<&str>,
    turn: u32,
    attempt: u32,
) -> Span {
    let otel_name = format!("chat {}", model.unwrap_or(provider));
    let parent = lookup(run_registry(), workflow_id);
    let span = match parent {
        Some(parent) => tracing::info_span!(
            parent: &parent,
            "chat",
            otel.name = otel_name.as_str(),
            gen_ai.operation.name = "chat",
            gen_ai.system = provider,
            gen_ai.provider.name = provider,
            gen_ai.agent.name = agent,
            gen_ai.request.model = field::Empty,
            gen_ai.conversation.id = field::Empty,
            gen_ai.usage.input_tokens = field::Empty,
            gen_ai.usage.output_tokens = field::Empty,
            operation.cost = field::Empty,
            odori.run.id = workflow_id,
            odori.turn = turn,
            odori.turn.attempt = attempt,
            error.r#type = field::Empty,
            otel.status_code = field::Empty,
        ),
        None => tracing::info_span!(
            parent: None,
            "chat",
            otel.name = otel_name.as_str(),
            gen_ai.operation.name = "chat",
            gen_ai.system = provider,
            gen_ai.provider.name = provider,
            gen_ai.agent.name = agent,
            gen_ai.request.model = field::Empty,
            gen_ai.conversation.id = field::Empty,
            gen_ai.usage.input_tokens = field::Empty,
            gen_ai.usage.output_tokens = field::Empty,
            operation.cost = field::Empty,
            odori.run.id = workflow_id,
            odori.turn = turn,
            odori.turn.attempt = attempt,
            error.r#type = field::Empty,
            otel.status_code = field::Empty,
        ),
    };
    if let Some(model) = model {
        span.record("gen_ai.request.model", model);
    }
    span
}

/// Register a turn span as the workflow's in-flight turn, so mid-turn tool
/// executions parent beneath it.
pub(crate) fn register_turn(workflow_id: &str, span: &Span) -> RegisteredSpan {
    RegisteredSpan::insert(turn_registry(), workflow_id, span)
}

/// Record the backend session id as soon as it is known.
pub(crate) fn record_session(span: &Span, session_id: &str) {
    span.record("gen_ai.conversation.id", session_id);
}

/// Record a harness-native tool invocation observed mid-turn. These are
/// events, not spans: the harness reports that a tool ran, not when it
/// started or finished.
pub(crate) fn record_harness_tool_use(span: &Span, name: &str) {
    tracing::info!(
        parent: span,
        gen_ai.tool.name = name,
        "harness tool use"
    );
}

/// Record a completed turn's accounting on its span.
pub(crate) fn record_turn_success(span: &Span, outcome: &TurnActivityOutput) {
    record_session(span, &outcome.session_id);
    if let Some(input_tokens) = outcome.usage.input_tokens {
        span.record("gen_ai.usage.input_tokens", input_tokens);
    }
    if let Some(output_tokens) = outcome.usage.output_tokens {
        span.record("gen_ai.usage.output_tokens", output_tokens);
    }
    if let Some(cost) = outcome.usage.total_cost_usd {
        span.record("operation.cost", cost);
    }
    span.record("otel.status_code", "OK");
}

/// Record a failed turn attempt's error class on its span.
pub(crate) fn record_turn_failure(span: &Span, error: &TurnError) {
    span.record("error.type", error.error_type());
    span.record("otel.status_code", "ERROR");
}

/// Open the `execute_tool` span for one durable tool execution, parented
/// to the in-flight turn (or the run, or detached) by registry lookup.
pub(crate) fn tool_span(
    workflow_id: &str,
    agent: &str,
    tool: &str,
    call_id: &str,
    turn: u32,
    attempt: u32,
) -> Span {
    let parent =
        lookup(turn_registry(), workflow_id).or_else(|| lookup(run_registry(), workflow_id));
    match parent {
        Some(parent) => tracing::info_span!(
            parent: &parent,
            "execute_tool",
            otel.name = format!("execute_tool {tool}"),
            gen_ai.operation.name = "execute_tool",
            gen_ai.tool.name = tool,
            gen_ai.tool.call.id = call_id,
            gen_ai.agent.name = agent,
            odori.run.id = workflow_id,
            odori.turn = turn,
            odori.turn.attempt = attempt,
            odori.tool.is_error = field::Empty,
            error.r#type = field::Empty,
            otel.status_code = field::Empty,
        ),
        None => tracing::info_span!(
            parent: None,
            "execute_tool",
            otel.name = format!("execute_tool {tool}"),
            gen_ai.operation.name = "execute_tool",
            gen_ai.tool.name = tool,
            gen_ai.tool.call.id = call_id,
            gen_ai.agent.name = agent,
            odori.run.id = workflow_id,
            odori.turn = turn,
            odori.turn.attempt = attempt,
            odori.tool.is_error = field::Empty,
            error.r#type = field::Empty,
            otel.status_code = field::Empty,
        ),
    }
}

/// Record a tool execution that returned a result (including model-visible
/// `is_error` results, which are successful executions of a failing tool).
pub(crate) fn record_tool_result(span: &Span, is_error: bool) {
    span.record("odori.tool.is_error", is_error);
    span.record("otel.status_code", "OK");
}

/// Record a tool execution that failed as an activity (handler error).
pub(crate) fn record_tool_failure(span: &Span, error_type: &str) {
    span.record("error.type", error_type);
    span.record("otel.status_code", "ERROR");
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tracing::field::{Field, Visit};
    use tracing_subscriber::{Layer, layer::SubscriberExt, registry::LookupSpan};

    use super::*;
    use crate::provider::TurnUsage;

    /// One captured span: name, explicit parent (if any), whether it was a
    /// contextual root, and every field recorded at creation or later.
    #[derive(Debug, Clone, Default)]
    struct CapturedSpan {
        name: String,
        parent: Option<tracing::span::Id>,
        explicit_root: bool,
        fields: Vec<(String, String)>,
    }

    #[derive(Debug, Default)]
    struct FieldCollector(Vec<(String, String)>);

    impl Visit for FieldCollector {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.0.push((field.name().to_owned(), format!("{value:?}")));
        }

        fn record_str(&mut self, field: &Field, value: &str) {
            self.0.push((field.name().to_owned(), value.to_owned()));
        }

        fn record_u64(&mut self, field: &Field, value: u64) {
            self.0.push((field.name().to_owned(), value.to_string()));
        }

        fn record_f64(&mut self, field: &Field, value: f64) {
            self.0.push((field.name().to_owned(), value.to_string()));
        }

        fn record_bool(&mut self, field: &Field, value: bool) {
            self.0.push((field.name().to_owned(), value.to_string()));
        }
    }

    #[derive(Debug, Clone, Default)]
    struct CaptureLayer {
        spans: Arc<Mutex<Vec<(tracing::span::Id, CapturedSpan)>>>,
    }

    impl CaptureLayer {
        fn captured(&self, id: &tracing::span::Id) -> CapturedSpan {
            self.spans
                .lock()
                .expect("captured spans lock")
                .iter()
                .find(|(captured_id, _)| captured_id == id)
                .map(|(_, span)| span.clone())
                .expect("span was captured")
        }
    }

    impl<S> Layer<S> for CaptureLayer
    where
        S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    {
        fn on_new_span(
            &self,
            attrs: &tracing::span::Attributes<'_>,
            id: &tracing::span::Id,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let mut collector = FieldCollector::default();
            attrs.record(&mut collector);
            let captured = CapturedSpan {
                name: attrs.metadata().name().to_owned(),
                parent: attrs.parent().cloned(),
                explicit_root: attrs.is_root(),
                fields: collector.0,
            };
            self.spans
                .lock()
                .expect("captured spans lock")
                .push((id.clone(), captured));
        }

        fn on_record(
            &self,
            id: &tracing::span::Id,
            values: &tracing::span::Record<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let mut collector = FieldCollector::default();
            values.record(&mut collector);
            let mut spans = self.spans.lock().expect("captured spans lock");
            let entry = spans
                .iter_mut()
                .find(|(captured_id, _)| captured_id == id)
                .map(|(_, span)| span)
                .expect("recorded span was captured");
            entry.fields.extend(collector.0);
        }
    }

    fn field<'a>(span: &'a CapturedSpan, name: &str) -> Option<&'a str> {
        span.fields
            .iter()
            .find(|(field_name, _)| field_name == name)
            .map(|(_, value)| value.as_str())
    }

    fn with_capture(test: impl FnOnce(&CaptureLayer)) {
        let layer = CaptureLayer::default();
        let subscriber = tracing_subscriber::registry().with(layer.clone());
        tracing::subscriber::with_default(subscriber, || test(&layer));
    }

    #[test]
    fn turn_and_tool_spans_nest_under_the_registered_run_span() {
        with_capture(|capture| {
            let telemetry = ClientRunTelemetry::start("planner", "wf-nest");
            let run_id = telemetry.span.id().expect("run span enabled");

            let turn = turn_span("wf-nest", "planner", "codex", Some("gpt-5"), 0, 1);
            let turn_id = turn.id().expect("turn span enabled");
            let _turn_registration = register_turn("wf-nest", &turn);

            let tool = tool_span("wf-nest", "planner", "save_note", "call-1", 0, 1);
            let tool_id = tool.id().expect("tool span enabled");

            assert_eq!(capture.captured(&turn_id).parent, Some(run_id));
            assert_eq!(capture.captured(&tool_id).parent, Some(turn_id.clone()));

            let captured_turn = capture.captured(&turn_id);
            assert_eq!(captured_turn.name, "chat");
            assert_eq!(capture.captured(&tool_id).name, "execute_tool");
            assert_eq!(field(&captured_turn, "gen_ai.operation.name"), Some("chat"));
            assert_eq!(field(&captured_turn, "gen_ai.system"), Some("codex"));
            assert_eq!(field(&captured_turn, "gen_ai.provider.name"), Some("codex"));
            assert_eq!(field(&captured_turn, "gen_ai.request.model"), Some("gpt-5"));
            assert_eq!(field(&captured_turn, "otel.name"), Some("chat gpt-5"));
            assert_eq!(field(&captured_turn, "odori.turn"), Some("0"));

            let captured_tool = capture.captured(&tool_id);
            assert_eq!(field(&captured_tool, "gen_ai.tool.name"), Some("save_note"));
            assert_eq!(field(&captured_tool, "gen_ai.tool.call.id"), Some("call-1"));
        });
    }

    #[test]
    fn unregistered_workflows_produce_detached_roots_with_run_correlation() {
        with_capture(|capture| {
            let turn = turn_span("wf-remote", "planner", "claude", None, 2, 1);
            let turn_id = turn.id().expect("turn span enabled");
            let captured = capture.captured(&turn_id);
            assert!(captured.explicit_root);
            assert_eq!(captured.parent, None);
            assert_eq!(field(&captured, "odori.run.id"), Some("wf-remote"));
            assert_eq!(field(&captured, "otel.name"), Some("chat claude"));
            assert_eq!(field(&captured, "gen_ai.request.model"), None);
        });
    }

    #[test]
    fn deregistration_restores_the_next_parent_tier() {
        with_capture(|capture| {
            let telemetry = ClientRunTelemetry::start("planner", "wf-tiers");
            let run_id = telemetry.span.id().expect("run span enabled");

            let turn = turn_span("wf-tiers", "planner", "codex", None, 0, 1);
            let registration = register_turn("wf-tiers", &turn);
            drop(registration);

            let tool = tool_span("wf-tiers", "planner", "save_note", "call-2", 0, 2);
            let tool_id = tool.id().expect("tool span enabled");
            assert_eq!(capture.captured(&tool_id).parent, Some(run_id));

            drop(telemetry);
            let detached = tool_span("wf-tiers", "planner", "save_note", "call-3", 0, 3);
            let detached_id = detached.id().expect("tool span enabled");
            assert!(capture.captured(&detached_id).explicit_root);
        });
    }

    #[test]
    fn outcome_recording_carries_usage_session_and_status() {
        with_capture(|capture| {
            let turn = turn_span("wf-record", "planner", "codex", None, 0, 1);
            let turn_id = turn.id().expect("turn span enabled");
            let usage = TurnUsage {
                input_tokens: Some(640),
                output_tokens: Some(120),
                total_cost_usd: Some(0.0025),
                ..TurnUsage::default()
            };
            let outcome = TurnActivityOutput {
                session_id: "session-0".to_owned(),
                text: "redacted by never being recorded".to_owned(),
                usage,
            };
            record_turn_success(&turn, &outcome);

            let captured = capture.captured(&turn_id);
            assert_eq!(
                field(&captured, "gen_ai.conversation.id"),
                Some("session-0")
            );
            assert_eq!(field(&captured, "gen_ai.usage.input_tokens"), Some("640"));
            assert_eq!(field(&captured, "gen_ai.usage.output_tokens"), Some("120"));
            assert_eq!(field(&captured, "operation.cost"), Some("0.0025"));
            assert_eq!(field(&captured, "otel.status_code"), Some("OK"));
            assert!(
                captured
                    .fields
                    .iter()
                    .all(|(_, value)| !value.contains("redacted by never")),
                "turn text must never reach span fields"
            );
        });
    }

    #[test]
    fn failure_recording_carries_the_error_class() {
        with_capture(|capture| {
            let turn = turn_span("wf-fail", "planner", "codex", None, 0, 1);
            let turn_id = turn.id().expect("turn span enabled");
            record_turn_failure(
                &turn,
                &TurnError::Timeout {
                    elapsed: std::time::Duration::from_secs(1),
                },
            );
            let captured = capture.captured(&turn_id);
            assert_eq!(field(&captured, "error.type"), Some("odori::turn::timeout"));
            assert_eq!(field(&captured, "otel.status_code"), Some("ERROR"));
        });
    }

    #[test]
    fn run_completion_recording_summarizes_the_run() {
        with_capture(|capture| {
            let telemetry = ClientRunTelemetry::start("planner", "wf-complete");
            let run_id = telemetry.span.id().expect("run span enabled");
            let output: RunOutput = serde_json::from_value(serde_json::json!({
                "text": "final",
                "session_id": "session-1",
                "usage": {
                    "total_cost_usd": 0.0075,
                    "input_tokens": 1380,
                    "output_tokens": 280,
                    "turns_with_unknown_tokens": 0,
                    "turns_with_unknown_cost": 0
                },
                "turns": 2,
                "end": "Completed"
            }))
            .expect("run output shape");
            telemetry.record_completion(&output);

            let captured = capture.captured(&run_id);
            assert_eq!(field(&captured, "gen_ai.usage.input_tokens"), Some("1380"));
            assert_eq!(field(&captured, "gen_ai.usage.output_tokens"), Some("280"));
            assert_eq!(field(&captured, "operation.cost"), Some("0.0075"));
            assert_eq!(field(&captured, "odori.run.turns"), Some("2"));
            assert_eq!(field(&captured, "odori.run.end"), Some("completed"));
            assert_eq!(field(&captured, "otel.status_code"), Some("OK"));
        });
    }
}
