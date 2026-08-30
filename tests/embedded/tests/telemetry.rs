//! The Logfire example's scripted scenario against the real embedded
//! engine, with the emitted span tree captured and asserted: one
//! `invoke_agent` root, one `chat` span per turn beneath it, the durable
//! `execute_tool` beneath its turn, accounting on every close, and no
//! prompt or tool content in any Odori span field.
//!
//! The capture layer stands where a host's exporter (the `logfire` crate,
//! a `tracing-opentelemetry` OTLP layer) would stand; parenting and
//! attributes are what an OTLP backend receives.

#[path = "../../../examples/logfire/scenario/mod.rs"]
mod scenario;

use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result, ensure};
use odori_engine::EmbeddedStorageConfig;
use tracing::field::{Field, Visit};
use tracing_subscriber::{Layer, layer::SubscriberExt, registry::LookupSpan};

#[derive(Debug, Clone, Default)]
struct Captured {
    name: String,
    parent: Option<tracing::span::Id>,
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

/// Captures every span (creation and later `record` calls) and every
/// event that passes the subscriber's filter.
#[derive(Debug, Clone, Default)]
struct CaptureLayer {
    spans: Arc<Mutex<Vec<(tracing::span::Id, Captured)>>>,
    events: Arc<Mutex<Vec<Captured>>>,
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
        let captured = Captured {
            name: attrs.metadata().name().to_owned(),
            parent: attrs.parent().cloned(),
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
        if let Some((_, span)) = spans.iter_mut().find(|(captured_id, _)| captured_id == id) {
            span.fields.extend(collector.0);
        }
    }

    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut collector = FieldCollector::default();
        event.record(&mut collector);
        self.events.lock().expect("captured events lock").push(Captured {
            name: event.metadata().name().to_owned(),
            parent: event.parent().cloned(),
            fields: collector.0,
        });
    }
}

fn field<'a>(span: &'a Captured, name: &str) -> Option<&'a str> {
    span.fields
        .iter()
        .find(|(field_name, _)| field_name == name)
        .map(|(_, value)| value.as_str())
}

#[tokio::test(flavor = "multi_thread")]
async fn logfire_scenario_emits_the_agent_trace_tree() -> Result<()> {
    let capture = CaptureLayer::default();
    // Global, not thread-local: turn and tool spans are emitted on the
    // odori worker thread. The filter is the example's redaction default
    // narrowed to Odori's layer, so the capture holds exactly what a
    // redacting exporter would ship.
    let subscriber = tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new("odori_agents=info"))
        .with(capture.clone());
    tracing::subscriber::set_global_default(subscriber)
        .context("install the capturing subscriber")?;

    let report = scenario::run_scripted_conversation(EmbeddedStorageConfig::InMemory).await?;

    // The conversation itself behaved.
    ensure!(report.saved_notes == vec!["pack the telescope".to_owned()]);
    ensure!(report.output.turns == 2);
    ensure!(report.output.usage.input_tokens == 1380);
    ensure!(report.output.usage.output_tokens == 280);

    let spans = capture.spans.lock().expect("captured spans lock").clone();
    let events = capture.events.lock().expect("captured events lock").clone();

    let runs: Vec<_> = spans
        .iter()
        .filter(|(_, span)| span.name == "invoke_agent")
        .collect();
    ensure!(runs.len() == 1, "expected one run span, saw {}", runs.len());
    let (run_id, run) = runs[0];
    ensure!(run.parent.is_none());
    ensure!(field(run, "gen_ai.agent.name") == Some("day-planner"));
    ensure!(field(run, "odori.run.id") == Some("logfire-1"));
    ensure!(field(run, "gen_ai.usage.input_tokens") == Some("1380"));
    ensure!(field(run, "gen_ai.usage.output_tokens") == Some("280"));
    ensure!(field(run, "odori.run.turns") == Some("2"));
    ensure!(field(run, "odori.run.end") == Some("conversation_ended"));
    ensure!(field(run, "otel.status_code") == Some("OK"));

    let mut turns: Vec<_> = spans
        .iter()
        .filter(|(_, span)| span.name == "chat")
        .collect();
    turns.sort_by_key(|(_, span)| field(span, "odori.turn").map(str::to_owned));
    ensure!(turns.len() == 2, "expected two turn spans, saw {}", turns.len());
    for (index, (_, turn)) in turns.iter().enumerate() {
        ensure!(turn.parent.as_ref() == Some(run_id));
        ensure!(field(turn, "odori.turn") == Some(index.to_string().as_str()));
        ensure!(field(turn, "gen_ai.system") == Some("logfire-scripted"));
        ensure!(field(turn, "gen_ai.provider.name") == Some("logfire-scripted"));
        ensure!(
            field(turn, "gen_ai.conversation.id") == Some(format!("logfire-session-{index}").as_str())
        );
        ensure!(field(turn, "otel.status_code") == Some("OK"));
    }
    ensure!(field(&turns[0].1, "gen_ai.usage.input_tokens") == Some("640"));
    ensure!(field(&turns[1].1, "gen_ai.usage.input_tokens") == Some("740"));

    let tools: Vec<_> = spans
        .iter()
        .filter(|(_, span)| span.name == "execute_tool")
        .collect();
    ensure!(tools.len() == 1, "expected one tool span, saw {}", tools.len());
    let (_, tool) = tools[0];
    ensure!(tool.parent == Some(turns[0].0.clone()));
    ensure!(field(tool, "gen_ai.tool.name") == Some("save_note"));
    ensure!(field(tool, "gen_ai.tool.call.id") == Some("logfire-call-0"));
    ensure!(field(tool, "odori.tool.is_error") == Some("false"));
    ensure!(field(tool, "otel.status_code") == Some("OK"));

    // The provider's in-harness tool observation surfaced as an event on
    // the first turn span.
    let tool_use: Vec<_> = events
        .iter()
        .filter(|event| field(event, "gen_ai.tool.name") == Some("save_note"))
        .collect();
    ensure!(!tool_use.is_empty(), "expected a harness tool-use event");
    ensure!(tool_use[0].parent == Some(turns[0].0.clone()));

    // Redaction: no Odori span or event field carries conversation or tool
    // content.
    let all_values = spans
        .iter()
        .flat_map(|(_, span)| span.fields.iter())
        .chain(events.iter().flat_map(|event| event.fields.iter()));
    for (name, value) in all_values {
        for leaked in ["stargazing", "telescope", "Noted:", "sunset"] {
            ensure!(
                !value.contains(leaked),
                "span field {name} leaked content: {value}"
            );
        }
    }
    Ok(())
}
