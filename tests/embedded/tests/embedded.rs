//! The Odori run loop against the real embedded engine, end to end:
//! zero-TCP transport, turn activities, retry recovery, budgets,
//! guardrails, interactive conversations.
//!
//! Sticky-execution note: these tests run with the SDK worker's default
//! workflow caching (the engine repo's SDK spike used
//! `max_cached_workflows(0)`; sticky over `service_override` is first
//! exercised HERE). A sticky-related failure is an engine-repo finding to
//! report, not to paper over.

use std::{
    net::TcpListener,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::Result;
use async_trait::async_trait;
use odori_agents::{
    Agent, AgentRegistry, Guardrail, GuardrailVerdict, Json, Providers, RunBudget, RunConfig,
    RunEnd, RunnerError,
    provider::{
        Provider, SessionDirective, TurnError, TurnEvent, TurnEventSink, TurnOutcome, TurnRequest,
        TurnUsage,
    },
};
use odori_engine::{ConnectTarget, OdoriRuntime};
use tokeira_engine::{Engine, TokeiraConfig};

/// Start a zero-listener engine, with sentinel ports proving an accidental
/// TCP fallback would fail deterministically (engine-repo spike trick).
async fn start_engine() -> Result<(Engine, TcpListener, TcpListener)> {
    let grpc_guard = TcpListener::bind("127.0.0.1:0")?;
    let nexus_guard = TcpListener::bind("127.0.0.1:0")?;
    let mut config = TokeiraConfig::default();
    config.infrastructure.network.grpc_addr = grpc_guard.local_addr()?.to_string();
    config.policy.nexus_completion.http_addr = nexus_guard.local_addr()?.to_string();
    let engine = Engine::start_with_config(config).await?;
    Ok((engine, grpc_guard, nexus_guard))
}

async fn start_runtime(
    engine: &Engine,
    task_queue: &str,
    registry: AgentRegistry,
    provider: Arc<dyn Provider>,
) -> Result<OdoriRuntime> {
    OdoriRuntime::builder(task_queue)
        .connect(ConnectTarget::service_override(engine.service_override()))
        .agents(registry)
        .providers(Providers::new(provider))
        .start()
        .await
}

/// A scripted provider: records every request it receives and answers from
/// a queue of behaviours (or echoes by default).
#[derive(Debug, Default)]
struct ScriptedProvider {
    requests: Mutex<Vec<TurnRequest>>,
    /// Popped front-first per call; empty = echo the input.
    script: Mutex<Vec<Behaviour>>,
}

#[derive(Debug)]
enum Behaviour {
    Echo,
    ReplyWith(String),
    ReplyWithUsage {
        text: String,
        usage: TurnUsage,
    },
    FailAfterSession {
        session_id: String,
        error: TurnError,
    },
    FailAfterUsage {
        session_id: String,
        usage: TurnUsage,
        error: TurnError,
    },
}

impl ScriptedProvider {
    fn scripted(script: Vec<Behaviour>) -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
            script: Mutex::new(script),
        })
    }

    fn requests(&self) -> Vec<TurnRequest> {
        self.requests.lock().expect("lock").clone()
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn name(&self) -> &str {
        "scripted"
    }

    async fn execute_turn(
        &self,
        request: TurnRequest,
        events: TurnEventSink,
    ) -> Result<TurnOutcome, TurnError> {
        self.requests.lock().expect("lock").push(request.clone());
        let behaviour = {
            let mut script = self.script.lock().expect("lock");
            if script.is_empty() {
                Behaviour::Echo
            } else {
                script.remove(0)
            }
        };
        let session_id = format!(
            "sess-{}-a{}",
            request.identity.turn, request.identity.attempt
        );
        match behaviour {
            Behaviour::Echo => {
                events.emit(TurnEvent::SessionStarted {
                    session_id: session_id.clone(),
                });
                Ok(TurnOutcome::new(
                    session_id,
                    format!("echo: {}", request.input),
                ))
            }
            Behaviour::ReplyWith(text) => {
                events.emit(TurnEvent::SessionStarted {
                    session_id: session_id.clone(),
                });
                Ok(TurnOutcome::new(session_id, text))
            }
            Behaviour::ReplyWithUsage { text, usage } => {
                events.emit(TurnEvent::SessionStarted {
                    session_id: session_id.clone(),
                });
                events.report_usage(usage.clone());
                let mut outcome = TurnOutcome::new(session_id, text);
                outcome.usage = usage;
                Ok(outcome)
            }
            Behaviour::FailAfterSession { session_id, error } => {
                events.emit(TurnEvent::SessionStarted { session_id });
                // Give the heartbeat pump time to record the session id
                // before the failure ends the attempt.
                tokio::time::sleep(Duration::from_millis(250)).await;
                Err(error)
            }
            Behaviour::FailAfterUsage {
                session_id,
                usage,
                error,
            } => {
                events.emit(TurnEvent::SessionStarted { session_id });
                events.report_usage(usage);
                // Ensure the heartbeat with both session and usage reaches
                // Temporal before this retryable attempt fails.
                tokio::time::sleep(Duration::from_millis(250)).await;
                Err(error)
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn single_turn_completes_over_zero_tcp_transport() -> Result<()> {
    let (engine, _grpc_guard, _nexus_guard) = start_engine().await?;
    let mut registry = AgentRegistry::new();
    registry.register(Agent::new("echoer", "echo things"));
    let provider = ScriptedProvider::scripted(Vec::new());
    let runtime = start_runtime(&engine, "tq-single", registry, provider.clone()).await?;

    let text: String = runtime
        .runner()
        .run("echoer", "hello embedded world", "run-single-1")
        .await?;
    assert_eq!(text, "echo: hello embedded world");

    let requests = provider.requests();
    assert_eq!(requests.len(), 1);
    assert!(matches!(requests[0].session, SessionDirective::Start));
    assert_eq!(requests[0].identity.turn, 0);
    assert_eq!(requests[0].identity.attempt, 1);
    assert_eq!(requests[0].directives.instructions, "echo things");

    runtime.shutdown().await?;
    engine.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn typed_output_parses_json() -> Result<()> {
    let (engine, _g1, _g2) = start_engine().await?;
    let mut registry = AgentRegistry::new();
    registry.register(Agent::new("typed", "answer in json"));
    let provider = ScriptedProvider::scripted(vec![Behaviour::ReplyWith(
        r#"{"answer": 42, "sure": true}"#.to_owned(),
    )]);
    let runtime = start_runtime(&engine, "tq-typed", registry, provider).await?;

    #[derive(Debug, serde::Deserialize)]
    struct Answer {
        answer: u32,
        sure: bool,
    }
    let Json(answer): Json<Answer> = runtime
        .runner()
        .run("typed", "the question", "run-typed-1")
        .await?;
    assert_eq!(answer.answer, 42);
    assert!(answer.sure);

    runtime.shutdown().await?;
    engine.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_attempt_retries_and_resumes_forked_from_heartbeat() -> Result<()> {
    let (engine, _g1, _g2) = start_engine().await?;
    let mut registry = AgentRegistry::new();
    registry.register(Agent::new("flaky", "persevere"));
    let provider = ScriptedProvider::scripted(vec![Behaviour::FailAfterSession {
        session_id: "sess-died-mid-turn".to_owned(),
        error: TurnError::HarnessDied {
            exit_code: Some(143),
            stderr_head: "killed".to_owned(),
        },
    }]);
    let runtime = start_runtime(&engine, "tq-retry", registry, provider.clone()).await?;

    let text: String = runtime
        .runner()
        .run("flaky", "carry on", "run-retry-1")
        .await?;
    assert_eq!(text, "echo: carry on");

    let requests = provider.requests();
    assert_eq!(requests.len(), 2, "attempt 1 fails, attempt 2 succeeds");
    assert_eq!(requests[0].identity.attempt, 1);
    assert!(matches!(requests[0].session, SessionDirective::Start));
    assert_eq!(requests[1].identity.attempt, 2);
    // The retried attempt recovered the dead attempt's session id from
    // heartbeat details and resumed it forked.
    assert!(
        matches!(
            &requests[1].session,
            SessionDirective::ResumeForked { session_id } if session_id == "sess-died-mid-turn"
        ),
        "attempt 2 directive: {:?}",
        requests[1].session
    );

    runtime.shutdown().await?;
    engine.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn retry_usage_is_carried_through_heartbeat_details() -> Result<()> {
    let (engine, _g1, _g2) = start_engine().await?;
    let mut registry = AgentRegistry::new();
    registry.register(Agent::new("metered", "count honestly"));
    let mut failed_usage = TurnUsage::default();
    failed_usage.input_tokens = Some(100);
    failed_usage.output_tokens = Some(25);
    failed_usage.total_cost_usd = Some(0.20);
    let mut successful_usage = TurnUsage::default();
    successful_usage.input_tokens = Some(150);
    successful_usage.output_tokens = Some(40);
    successful_usage.total_cost_usd = Some(0.30);
    let provider = ScriptedProvider::scripted(vec![
        Behaviour::FailAfterUsage {
            session_id: "sess-metered-failed".to_owned(),
            usage: failed_usage,
            error: TurnError::HarnessDied {
                exit_code: Some(143),
                stderr_head: "killed after generation".to_owned(),
            },
        },
        Behaviour::ReplyWithUsage {
            text: "done".to_owned(),
            usage: successful_usage,
        },
    ]);
    let runtime = start_runtime(&engine, "tq-retry-usage", registry, provider).await?;

    let conversation = runtime
        .runner()
        .start_conversation("metered", "work", "run-retry-usage-1")
        .await?;
    let output = conversation.end().await?;
    assert_eq!(output.usage.input_tokens, 250);
    assert_eq!(output.usage.output_tokens, 65);
    assert!((output.usage.total_cost_usd - 0.50).abs() < f64::EPSILON);
    assert_eq!(output.usage.turns_with_unknown_tokens, 0);
    assert_eq!(output.usage.turns_with_unknown_cost, 0);

    runtime.shutdown().await?;
    engine.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn non_retryable_turn_error_fails_the_run() -> Result<()> {
    let (engine, _g1, _g2) = start_engine().await?;
    let mut registry = AgentRegistry::new();
    registry.register(Agent::new("doomed", "fail"));
    let provider = ScriptedProvider::scripted(vec![Behaviour::FailAfterSession {
        session_id: "sess-x".to_owned(),
        error: TurnError::Config {
            message: "harness binary missing".to_owned(),
        },
    }]);
    let runtime = start_runtime(&engine, "tq-fatal", registry, provider.clone()).await?;

    let error = runtime
        .runner()
        .run::<String>("doomed", "go", "run-fatal-1")
        .await
        .expect_err("config errors are non-retryable");
    assert!(matches!(error, RunnerError::Run { .. }), "{error:?}");
    assert_eq!(provider.requests().len(), 1, "no retry after non-retryable");

    runtime.shutdown().await?;
    engine.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn interactive_conversation_forks_turns_and_ends() -> Result<()> {
    let (engine, _g1, _g2) = start_engine().await?;
    let mut registry = AgentRegistry::new();
    registry.register(Agent::new("chatty", "keep talking"));
    let provider = ScriptedProvider::scripted(Vec::new());
    let runtime = start_runtime(&engine, "tq-chat", registry, provider.clone()).await?;

    let conversation = runtime
        .runner()
        .start_conversation("chatty", "first message", "run-chat-1")
        .await?;
    conversation.send("second message").await?;
    let output = conversation.end().await?;

    assert_eq!(output.turns, 2);
    assert_eq!(output.text, "echo: second message");
    assert_eq!(output.end, RunEnd::ConversationEnded);

    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert!(matches!(requests[0].session, SessionDirective::Start));
    // Turn 1 forks from turn 0's session (turn 0, attempt 1 naming scheme).
    assert!(
        matches!(
            &requests[1].session,
            SessionDirective::ResumeForked { session_id } if session_id == "sess-0-a1"
        ),
        "turn 1 directive: {:?}",
        requests[1].session
    );

    runtime.shutdown().await?;
    engine.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn budget_and_guardrails_end_runs_typed() -> Result<()> {
    #[derive(Debug)]
    struct NoPirates;
    impl Guardrail for NoPirates {
        fn name(&self) -> &str {
            "no-pirates"
        }
        fn check(&self, text: &str) -> GuardrailVerdict {
            if text.contains("pirate") {
                GuardrailVerdict::Block {
                    reason: "pirates are out of scope".to_owned(),
                }
            } else {
                GuardrailVerdict::Pass
            }
        }
    }

    let (engine, _g1, _g2) = start_engine().await?;
    let mut registry = AgentRegistry::new();
    registry.register(Agent::new("guarded", "be safe").with_input_guardrail(NoPirates));
    registry.register(
        Agent::new("capped", "stop on time")
            .with_budget(RunBudget::unlimited().with_max_turns(1)),
    );
    let provider = ScriptedProvider::scripted(Vec::new());
    let runtime = start_runtime(&engine, "tq-guard", registry, provider.clone()).await?;

    // Input guardrail blocks before any turn: the provider never runs.
    let error = runtime
        .runner()
        .run::<String>("guarded", "tell me about pirates", "run-guard-1")
        .await
        .expect_err("guardrail must block");
    assert!(
        matches!(&error, RunnerError::GuardrailBlocked { guardrail, .. } if guardrail == "no-pirates"),
        "{error:?}"
    );
    assert!(provider.requests().is_empty());

    // Turn budget: max_turns 0 ends the run before its first turn.
    let config = RunConfig::default().with_budget(RunBudget::unlimited().with_max_turns(0));
    let error = runtime
        .runner()
        .run_with_config::<String>("guarded", "hello", "run-guard-2", config)
        .await
        .expect_err("zero-turn budget must trip");
    assert!(
        matches!(error, RunnerError::BudgetExceeded { .. }),
        "{error:?}"
    );

    // A scripted interactive harness completes its one allowed turn. A
    // queued second turn then terminates deterministically from history.
    let conversation = runtime
        .runner()
        .start_conversation("capped", "first", "run-guard-3")
        .await?;
    conversation.send("second").await?;
    let output = conversation.end().await?;
    assert_eq!(output.turns, 1);
    assert_eq!(
        output.end,
        RunEnd::BudgetExceeded {
            cap: odori_agents::run::BudgetCap::Turns { spent: 1, cap: 1 }
        }
    );

    runtime.shutdown().await?;
    engine.shutdown().await?;
    Ok(())
}
