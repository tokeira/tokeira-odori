//! The deterministic harness: propose first, then consume the recorded approval.

use async_trait::async_trait;
use odori_agents::provider::{
    McpTransport, Provider, SessionDirective, TurnError, TurnEvent, TurnEventSink, TurnOutcome,
    TurnRequest, TurnTooling,
};
use serde_json::{Value, json};

use super::{
    model::{
        ApprovalCompletion, ApprovalDecision, FORKED_SESSION_ID, PLAN_HASH, SESSION_ID, proposal,
    },
    workspace::{ALLOWED_PATH, FIXED_LIB},
};

#[derive(Debug, Clone)]
pub(super) struct ApprovalProvider;

#[async_trait]
impl Provider for ApprovalProvider {
    fn name(&self) -> &str {
        "approval-scripted"
    }

    async fn execute_turn(
        &self,
        request: TurnRequest,
        events: TurnEventSink,
    ) -> Result<TurnOutcome, TurnError> {
        let (session_id, session_forked) = match (&request.session, request.identity.turn) {
            (SessionDirective::Start, 0) => (SESSION_ID, false),
            (SessionDirective::ResumeForked { session_id }, 1) if session_id == SESSION_ID => {
                (FORKED_SESSION_ID, true)
            }
            (directive, turn) => {
                return Err(tooling(format!(
                    "turn {turn} received unexpected session directive {directive:?}"
                )));
            }
        };
        events.emit(TurnEvent::SessionStarted {
            session_id: session_id.to_owned(),
        });
        events.emit(TurnEvent::Liveness);

        if request.identity.turn == 0 {
            return Ok(TurnOutcome::new(
                session_id,
                serde_json::to_string(&proposal()).map_err(tooling)?,
            ));
        }

        let decision: ApprovalDecision = serde_json::from_str(&request.input).map_err(tooling)?;
        if decision.decision != "approve" || decision.plan_hash != PLAN_HASH {
            return Err(tooling(
                "the restored turn did not carry the approved plan hash",
            ));
        }
        let apply = call_tool(
            &request.tooling,
            "apply_approved_patch",
            json!({
                "plan_hash": decision.plan_hash,
                "path": ALLOWED_PATH,
                "content": FIXED_LIB,
            }),
            "approval-resume-apply",
        )
        .await?;
        tool_success_text(&apply)?;
        let finish_bar = call_tool(
            &request.tooling,
            "finish_bar",
            json!({"plan_hash": PLAN_HASH}),
            "approval-resume-finish-bar",
        )
        .await?;
        tool_success_text(&finish_bar)?;

        Ok(TurnOutcome::new(
            session_id,
            serde_json::to_string(&ApprovalCompletion {
                plan_hash: PLAN_HASH.to_owned(),
                applied: ALLOWED_PATH.to_owned(),
                finish_bar: vec!["cargo test --locked".to_owned()],
                session_forked,
            })
            .map_err(tooling)?,
        ))
    }
}

fn tooling(error: impl std::fmt::Display) -> TurnError {
    TurnError::Tooling {
        message: error.to_string(),
    }
}

fn endpoint(tooling_config: &TurnTooling) -> Result<(String, String), TurnError> {
    let server = tooling_config
        .mcp_servers
        .first()
        .ok_or_else(|| tooling("the durable bridge was not attached"))?;
    let McpTransport::Http { url, headers } = &server.transport else {
        return Err(tooling("the example requires the HTTP bridge"));
    };
    let authorization = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
        .map(|(_, value)| value.clone())
        .ok_or_else(|| tooling("bridge attachment omitted authorization"))?;
    Ok((url.clone(), authorization))
}

async fn call_tool(
    tooling_config: &TurnTooling,
    name: &str,
    arguments: Value,
    call_id: &str,
) -> Result<Value, TurnError> {
    let (url, authorization) = endpoint(tooling_config)?;
    let body = reqwest::Client::new()
        .post(url)
        .header("Authorization", authorization)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": call_id,
            "method": "tools/call",
            "params": {
                "name": name,
                "arguments": arguments,
                "_meta": {"odori/callId": call_id},
            },
        }))
        .send()
        .await
        .map_err(tooling)?
        .text()
        .await
        .map_err(tooling)?;
    let frame = body
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .next_back()
        .ok_or_else(|| tooling("bridge returned no final SSE frame"))?;
    serde_json::from_str(frame).map_err(tooling)
}

fn tool_success_text(frame: &Value) -> Result<String, TurnError> {
    if frame
        .pointer("/result/isError")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(tooling(format!("durable tool refused the call: {frame}")));
    }
    if let Some(error) = frame.pointer("/error/message").and_then(Value::as_str) {
        return Err(tooling(error));
    }
    frame
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| tooling(format!("tool response has no text result: {frame}")))
}
