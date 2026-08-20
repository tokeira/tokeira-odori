//! The in-process MCP server: streamable HTTP on loopback, bearer-token
//! gated, serving exactly the spec's contract-policy table.
//!
//! `initialize`/`ping`/`tools/list` answer as plain JSON; `tools/call`
//! answers as an SSE stream so keepalive progress notifications can flow
//! while the durable execution runs (spec Requirement 5.1); everything
//! else is `method_not_found`. Client notifications are accepted with 202.

use std::{convert::Infallible, sync::Arc};

use bytes::Bytes;
use futures::StreamExt;
use http::{Method, Request, Response, StatusCode, header};
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use odori_agents::{
    InvocationId, ToolCallResult,
    run::{InvocationRejection, ToolInvocation, ToolInvocationReply},
};
use serde_json::{Value, json};
use tokio_stream::wrappers::ReceiverStream;

use crate::attach::{BridgeInner, RunContext};

type BoxBody = http_body_util::combinators::BoxBody<Bytes, Infallible>;

pub(crate) async fn serve(inner: Arc<BridgeInner>, listener: tokio::net::TcpListener) {
    loop {
        let Ok((stream, _peer)) = listener.accept().await else {
            return;
        };
        let inner = inner.clone();
        tokio::spawn(async move {
            let io = hyper_util::rt::TokioIo::new(stream);
            let service = hyper::service::service_fn(move |request| {
                let inner = inner.clone();
                async move { Ok::<_, Infallible>(handle(inner, request).await) }
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(io, service)
                .await;
        });
    }
}

async fn handle(inner: Arc<BridgeInner>, request: Request<Incoming>) -> Response<BoxBody> {
    if request.method() != Method::POST {
        return plain(StatusCode::METHOD_NOT_ALLOWED, json!({}));
    }
    // Bearer gate before anything else (spec Requirement 1.3).
    let Some(context) = authorize(&inner, &request) else {
        return plain(StatusCode::UNAUTHORIZED, json!({}));
    };
    let Ok(body) = request.into_body().collect().await else {
        return plain(StatusCode::BAD_REQUEST, json!({}));
    };
    let Ok(message) = serde_json::from_slice::<Value>(&body.to_bytes()) else {
        return plain(StatusCode::BAD_REQUEST, json!({}));
    };

    let method = message.get("method").and_then(Value::as_str).unwrap_or("");
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    if method.starts_with("notifications/") {
        return empty(StatusCode::ACCEPTED);
    }
    match method {
        "initialize" => {
            let protocol = message
                .pointer("/params/protocolVersion")
                .cloned()
                .unwrap_or_else(|| json!("2025-06-18"));
            plain(
                StatusCode::OK,
                rpc_result(
                    id,
                    json!({
                        "protocolVersion": protocol,
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": inner.server_name(), "version": env!("CARGO_PKG_VERSION") },
                    }),
                ),
            )
        }
        "ping" => plain(StatusCode::OK, rpc_result(id, json!({}))),
        "tools/list" => plain(StatusCode::OK, rpc_result(id, inner.tool_listing(&context))),
        "tools/call" => tools_call(inner, context, id, &message).await,
        other => plain(
            StatusCode::OK,
            rpc_error(id, -32601, &format!("method not found: {other}")),
        ),
    }
}

fn authorize(inner: &BridgeInner, request: &Request<Incoming>) -> Option<RunContext> {
    let header = request
        .headers()
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let token = header.strip_prefix("Bearer ")?;
    inner.context_for_token(token)
}

/// `tools/call`: SSE stream carrying keepalive progress then the final
/// JSON-RPC response.
async fn tools_call(
    inner: Arc<BridgeInner>,
    context: RunContext,
    id: Value,
    message: &Value,
) -> Response<BoxBody> {
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let tool = params.get("name").and_then(Value::as_str).unwrap_or("");
    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
    let progress_token = params.pointer("/_meta/progressToken").cloned();
    // The harness call id: Claude Code's `_meta` field first (spike-verified),
    // a generic escape hatch second, the JSON-RPC id as a last resort.
    let call_id = params
        .pointer("/_meta/claudecode~1toolUseId")
        .or_else(|| params.pointer("/_meta/odori~1callId"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| match &id {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(format!("rpc-{n}")),
            _ => None,
        });
    let Some(call_id) = call_id.filter(|call_id| !call_id.is_empty()) else {
        return plain(StatusCode::OK, rpc_error(id, -32602, "missing call id"));
    };

    let invocation = ToolInvocation {
        identity: InvocationId {
            turn: context.turn,
            attempt: context.attempt,
            call_id,
        },
        tool: tool.to_owned(),
        arguments,
    };

    let (sender, receiver) = tokio::sync::mpsc::channel::<Bytes>(16);
    tokio::spawn(async move {
        let progress_sender = sender.clone();
        let reply = inner
            .broker()
            .call(&context.workflow_id, invocation, move || {
                let frame = sse_frame(&json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/progress",
                    "params": {
                        "progressToken": progress_token.clone().unwrap_or(Value::Null),
                        "progress": 0,
                    },
                }));
                let _ = progress_sender.try_send(frame);
            })
            .await;
        let response = match reply {
            Ok(ToolInvocationReply::Completed(result)) => rpc_result(id, call_result_json(&result)),
            Ok(ToolInvocationReply::Rejected(rejection)) => rejection_json(id, rejection),
            Err(error) => rpc_error(id, -32603, &error.to_string()),
        };
        let _ = sender.send(sse_frame(&response)).await;
    });

    let stream = ReceiverStream::new(receiver).map(|bytes| Ok::<_, Infallible>(Frame::data(bytes)));
    let mut response = Response::new(BoxBody::new(StreamBody::new(stream)));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/event-stream"),
    );
    response
}

fn call_result_json(result: &ToolCallResult) -> Value {
    json!({ "content": result.content, "isError": result.is_error })
}

/// The spec's error-table mapping for workflow-side rejections.
fn rejection_json(id: Value, rejection: InvocationRejection) -> Value {
    match rejection {
        InvocationRejection::Fenced => rpc_error(id, -32011, "superseded attempt (fenced)"),
        InvocationRejection::UnknownTool => rpc_error(id, -32602, "unknown tool"),
        InvocationRejection::UnknownTurn => rpc_error(id, -32011, "unknown turn"),
        InvocationRejection::InvalidCallId => rpc_error(id, -32602, "invalid call id"),
        InvocationRejection::RunCancelled => rpc_error(id, -32603, "run cancelled"),
    }
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn sse_frame(payload: &Value) -> Bytes {
    Bytes::from(format!("event: message\ndata: {payload}\n\n"))
}

fn plain(status: StatusCode, payload: Value) -> Response<BoxBody> {
    let mut response = Response::new(BoxBody::new(
        Full::new(Bytes::from(payload.to_string())).map_err(|never| match never {}),
    ));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    response
}

fn empty(status: StatusCode) -> Response<BoxBody> {
    let mut response = Response::new(BoxBody::new(
        Full::new(Bytes::new()).map_err(|never| match never {}),
    ));
    *response.status_mut() = status;
    response
}
