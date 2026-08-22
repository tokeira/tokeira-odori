//! Deterministic Claude/Codex behavior for planning, work, review, and approval.

use std::sync::Arc;

use async_trait::async_trait;
use odori::agents::provider::{
    Provider, TurnError, TurnEvent, TurnEventSink, TurnOutcome, TurnRequest, TurnUsage,
};
use serde_json::{Value, json};

use super::{
    bridge::{call_tool, ensure_tool_error, tool_text, tooling},
    model::{PLAN_HASH, Review, SlicePlan, WorkerOutcome},
    state::{FleetEvent, FleetState},
    workspace::{FIX_DOUBLE, FIX_INCREMENT},
};

#[derive(Debug, Clone)]
pub(super) struct FleetProvider {
    pub(super) name: &'static str,
    pub(super) state: Arc<FleetState>,
}

impl FleetProvider {
    fn turn(
        &self,
        events: &TurnEventSink,
        request: &TurnRequest,
        text: String,
        input: u64,
        output: u64,
    ) -> TurnOutcome {
        let session_id = format!("{}-session", request.directives.name);
        events.emit(TurnEvent::SessionStarted {
            session_id: session_id.clone(),
        });
        let mut usage = TurnUsage::default();
        usage.input_tokens = Some(input);
        usage.output_tokens = Some(output);
        usage.total_cost_usd = Some((input + output) as f64 / 10_000.0);
        events.report_usage(usage.clone());
        let mut outcome = TurnOutcome::new(session_id, text);
        outcome.usage = usage;
        outcome
    }

    async fn orchestrate(
        &self,
        request: &TurnRequest,
        _events: &TurnEventSink,
    ) -> Result<String, TurnError> {
        if request.identity.turn == 0 {
            let plan = SlicePlan::campaign();
            self.state
                .events
                .send(FleetEvent::Plan(plan.clone()))
                .map_err(tooling)?;
            return serde_json::to_string_pretty(&plan).map_err(tooling);
        }
        if request.input == format!("APPROVE PLAN {PLAN_HASH}") {
            // This is the fan-out point: every transfer is a workflow
            // update, and every accepted transfer starts its target
            // AgentRun as a child workflow.
            let increment_bugfix = call_tool(
                &request.tooling,
                "transfer_to_increment_bugfix_worker",
                json!({"input": "fix increment within src/increment.rs, run its finish bar, then obtain hostile Codex review"}),
                "dispatch-increment-bugfix",
            );
            let double_feature = call_tool(
                &request.tooling,
                "transfer_to_double_feature_worker",
                json!({"input": "implement double within src/double.rs, run its finish bar, then obtain hostile Claude review"}),
                "dispatch-double-feature",
            );
            let budget = call_tool(
                &request.tooling,
                "transfer_to_budget_worker",
                json!({"input": "attempt the capped documentation slice"}),
                "dispatch-budget",
            );
            let raised = call_tool(
                &request.tooling,
                "transfer_to_contract_worker",
                json!({"input": "check whether Cargo.toml may be changed without operator approval"}),
                "dispatch-contract",
            );
            let (increment_bugfix, double_feature, budget, raised) =
                tokio::join!(increment_bugfix, double_feature, budget, raised);
            let (increment_bugfix, double_feature, budget, raised) =
                (increment_bugfix?, double_feature?, budget?, raised?);
            let increment_bugfix_text = tool_text(&increment_bugfix)?;
            let double_feature_text = tool_text(&double_feature)?;
            serde_json::from_str::<WorkerOutcome>(&increment_bugfix_text).map_err(tooling)?;
            serde_json::from_str::<WorkerOutcome>(&double_feature_text).map_err(tooling)?;
            ensure_tool_error(&budget, "budget handoff")?;
            let raised_text = tool_text(&raised)?;
            let WorkerOutcome::Raise { .. } =
                serde_json::from_str::<WorkerOutcome>(&raised_text).map_err(tooling)?
            else {
                return Err(tooling("contract worker did not raise"));
            };
            {
                let mut evidence = self.state.evidence.lock().expect("fleet evidence lock");
                evidence.budget_exceeded = true;
                evidence.raised = true;
            }
            self.state
                .events
                .send(FleetEvent::SlicesReady)
                .map_err(tooling)?;
            return Ok(json!({
                "approval_queue": ["increment-bugfix", "double-feature"],
                "budget": "BudgetExceeded(max_turns=0)",
                "raise": raised_text,
            })
            .to_string());
        }
        if let Some(slice) = request.input.strip_prefix("APPROVE APPLY ") {
            let result = call_tool(
                &request.tooling,
                "apply_slice",
                json!({
                    "slice": slice,
                    "plan_hash": PLAN_HASH,
                    "approval": request.input,
                }),
                &format!("apply-{slice}"),
            )
            .await?;
            let text = tool_text(&result)?;
            if result.pointer("/result/isError").and_then(Value::as_bool) == Some(true) {
                self.state
                    .events
                    .send(FleetEvent::ApplyRefused(slice.to_owned()))
                    .map_err(tooling)?;
                return Ok(text);
            }
            self.state
                .events
                .send(FleetEvent::Applied(slice.to_owned()))
                .map_err(tooling)?;
            return Ok(text);
        }
        if request.input.starts_with("RAISE DECISION ") {
            self.state
                .events
                .send(FleetEvent::RaiseObserved)
                .map_err(tooling)?;
            return Ok("operator kept the frozen contract; slice remains unapplied".to_owned());
        }
        Err(tooling(format!(
            "orchestrator rejected unrecognized approval signal {:?}",
            request.input
        )))
    }

    async fn work(
        &self,
        slice: &str,
        allowed_path: &str,
        test_filter: &str,
        replacement: &str,
        reviewer: &str,
        request: &TurnRequest,
    ) -> Result<String, TurnError> {
        let fence = call_tool(
            &request.tooling,
            "scope_write",
            json!({"path": "Cargo.toml", "content": "# scope escape"}),
            &format!("{slice}-scope-probe"),
        )
        .await?;
        ensure_tool_error(&fence, "scope fence")?;
        self.state
            .evidence
            .lock()
            .expect("fleet evidence lock")
            .scope_refusals += 1;
        let write = call_tool(
            &request.tooling,
            "scope_write",
            json!({"path": allowed_path, "content": replacement}),
            &format!("{slice}-write"),
        )
        .await?;
        let write_evidence = tool_text(&write)?;
        let finish = call_tool(
            &request.tooling,
            "finish_bar",
            json!({}),
            &format!("{slice}-finish-bar"),
        )
        .await?;
        let finish_text = tool_text(&finish)?;
        let review = call_tool(
            &request.tooling,
            &format!("transfer_to_{reviewer}"),
            json!({"input": format!(
                "Hostile review of {slice}; diff={write_evidence}; bar={finish_text}; reject scope creep and missing tests"
            )}),
            &format!("{slice}-hostile-review"),
        )
        .await?;
        let review: Review = serde_json::from_str(&tool_text(&review)?).map_err(tooling)?;
        self.state
            .evidence
            .lock()
            .expect("fleet evidence lock")
            .reviews
            .insert(slice.to_owned(), review.provider.clone());
        serde_json::to_string(&WorkerOutcome::Ready {
            slice: slice.to_owned(),
            review,
            finish_bar: vec![
                "cargo check --locked".to_owned(),
                format!("cargo test {test_filter} --locked"),
            ],
        })
        .map_err(tooling)
    }
}

#[async_trait]
impl Provider for FleetProvider {
    fn name(&self) -> &str {
        self.name
    }

    async fn execute_turn(
        &self,
        request: TurnRequest,
        events: TurnEventSink,
    ) -> Result<TurnOutcome, TurnError> {
        let name = request.directives.name.as_str();
        let text = match name {
            "orchestrator" => self.orchestrate(&request, &events).await?,
            "increment-bugfix-worker" => {
                self.work(
                    "increment-bugfix",
                    "src/increment.rs",
                    "increment",
                    FIX_INCREMENT,
                    "reviewer_codex",
                    &request,
                )
                .await?
            }
            "double-feature-worker" => {
                self.work(
                    "double-feature",
                    "src/double.rs",
                    "double",
                    FIX_DOUBLE,
                    "reviewer_claude",
                    &request,
                )
                .await?
            }
            "contract-worker" => serde_json::to_string(&WorkerOutcome::Raise {
                slice: "contract".to_owned(),
                contract: "workspace dependency graph is operator-owned".to_owned(),
                evidence: "the requested Cargo.toml edit falls outside the declared feature files"
                    .to_owned(),
            })
            .map_err(tooling)?,
            "reviewer-codex" | "reviewer-claude" => {
                if !request.input.contains("\"before\"")
                    || !request.input.contains("\"after\"")
                    || !request.input.contains("\"green\":true")
                {
                    return Err(tooling(
                        "review did not receive diff and finish-bar evidence",
                    ));
                }
                serde_json::to_string(&Review {
                    reviewer: name.to_owned(),
                    provider: self.name.to_owned(),
                    verdict: "approve: scoped diff plus green targeted bar".to_owned(),
                })
                .map_err(tooling)?
            }
            other => return Err(tooling(format!("script has no agent {other}"))),
        };
        Ok(self.turn(&events, &request, text, 40, 10))
    }
}
