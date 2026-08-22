//! Deterministic provider behavior for retry, dedupe, and timeline divergence.

use std::sync::Arc;

use async_trait::async_trait;
use odori::agents::provider::{
    Provider, TurnError, TurnEvent, TurnEventSink, TurnOutcome, TurnRequest,
};
use serde_json::json;

use super::{
    bridge::{call_tool, tool_text, tooling},
    model::{DeliberationSnapshot, PLAN_HASH, RewindEvent, TimelineInput},
    state::RewindState,
};

#[derive(Debug, Clone)]
pub(super) struct RewindProvider {
    pub(super) state: Arc<RewindState>,
}

#[async_trait]
impl Provider for RewindProvider {
    fn name(&self) -> &str {
        "rewind-scripted"
    }

    async fn execute_turn(
        &self,
        request: TurnRequest,
        events: TurnEventSink,
    ) -> Result<TurnOutcome, TurnError> {
        let session_id = "rewind-session".to_owned();
        events.emit(TurnEvent::SessionStarted {
            session_id: session_id.clone(),
        });
        if request.directives.name == "timeline" {
            let input: TimelineInput = serde_json::from_str(&request.input).map_err(tooling)?;
            let text = json!({
                "plan_hash": input.snapshot.plan_hash,
                "decision": input.decision,
                "result": format!("{} from {}", input.decision, input.snapshot.checkpoint),
            })
            .to_string();
            return Ok(TurnOutcome::new(session_id, text));
        }

        let call_id = "stable-checkpoint-call";
        self.state
            .presentations
            .lock()
            .expect("rewind presentations lock")
            .push((request.identity.attempt, call_id.to_owned()));
        if request.identity.attempt > 1 {
            self.state
                .events
                .send(RewindEvent::RetryPresented(request.identity.attempt))
                .map_err(tooling)?;
        }
        let result = call_tool(
            &request.tooling,
            "checkpoint",
            json!({"label": "plan-ready"}),
            call_id,
        )
        .await?;
        let receipt = tool_text(&result)?;
        if request.identity.attempt == 1 {
            self.state
                .events
                .send(RewindEvent::CheckpointRecorded)
                .map_err(tooling)?;
            self.state
                .events
                .send(RewindEvent::FailureReturned)
                .map_err(tooling)?;
            return Err(TurnError::HarnessDied {
                exit_code: Some(137),
                stderr_head: "demo kill at plan-ready checkpoint".to_owned(),
            });
        }
        let snapshot = DeliberationSnapshot {
            goal: "choose a release path".to_owned(),
            checkpoint: receipt,
            plan_hash: PLAN_HASH.to_owned(),
        };
        Ok(TurnOutcome::new(
            session_id,
            serde_json::to_string(&snapshot).map_err(tooling)?,
        ))
    }
}
