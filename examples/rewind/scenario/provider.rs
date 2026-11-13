//! Deterministic provider behavior for retry, dedupe, and timeline divergence.

use std::sync::Arc;

use async_trait::async_trait;
use odori::agents::provider::{
    Provider, TurnError, TurnEvent, TurnEventSink, TurnOutcome, TurnRequest,
};
use serde_json::json;

use super::{
    mcp_client::{call_tool, tool_config_error, tool_text},
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
            let input: TimelineInput =
                serde_json::from_str(&request.input).map_err(tool_config_error)?;
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
            .push((request.turn_id.attempt, call_id.to_owned()));
        if request.turn_id.attempt > 1 {
            self.state
                .events
                .send(RewindEvent::RetryPresented(request.turn_id.attempt))
                .map_err(tool_config_error)?;
        }
        let result = call_tool(
            &request.tool_config,
            "checkpoint",
            json!({"label": "plan-ready"}),
            call_id,
        )
        .await?;
        let receipt = tool_text(&result)?;
        if request.turn_id.attempt == 1 {
            self.state
                .events
                .send(RewindEvent::CheckpointRecorded)
                .map_err(tool_config_error)?;
            self.state
                .events
                .send(RewindEvent::FailureReturned)
                .map_err(tool_config_error)?;
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
            serde_json::to_string(&snapshot).map_err(tool_config_error)?,
        ))
    }
}
