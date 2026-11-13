//! Property suites for the call broker, against a fake update client with controllable completion ordering,
//! under tokio's paused virtual clock.
#![cfg(feature = "preview")]

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use odori_agents::{
    ToolCallId, ToolCallResult,
    run::{ToolCallReply, ToolCallRequest},
};
use odori_mcp_bridge::{CallBroker, McpToolServerError, ToolCallDispatcher};
use proptest::prelude::*;

/// Completes after a controlled virtual delay, flagging the completion so
/// the ordering between "update completed" and "reply observable" is
/// checkable.
#[derive(Debug)]
struct FakeToolCallDispatcher {
    delay: Duration,
    completed: Arc<AtomicBool>,
    reply_text: String,
}

#[async_trait]
impl ToolCallDispatcher for FakeToolCallDispatcher {
    async fn dispatch(
        &self,
        _workflow_id: &str,
        _invocation: ToolCallRequest,
    ) -> Result<ToolCallReply, McpToolServerError> {
        tokio::time::sleep(self.delay).await;
        self.completed.store(true, Ordering::SeqCst);
        Ok(ToolCallReply::Completed(ToolCallResult::text(
            self.reply_text.clone(),
        )))
    }
}

fn tool_call() -> ToolCallRequest {
    ToolCallRequest {
        tool_call_id: ToolCallId {
            turn: 0,
            attempt: 1,
            call_id: "call-p".to_owned(),
        },
        tool: "probe".to_owned(),
        arguments: serde_json::json!({}),
    }
}

fn paused_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .start_paused(true)
        .build()
        .expect("paused runtime")
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    // record before respond
    //
    // For any completion timing, the broker's reply is observable only
    // after the update client's completion has happened, and it is exactly
    // the completed update's reply — never a synthesized or early value.
    #[test]
    fn p2_record_before_respond(delay_ms in 0u64..600_000, cadence_ms in 1u64..60_000) {
        let runtime = paused_runtime();
        runtime.block_on(async move {
            let completed = Arc::new(AtomicBool::new(false));
            let client = Arc::new(FakeToolCallDispatcher {
                delay: Duration::from_millis(delay_ms),
                completed: completed.clone(),
                reply_text: format!("reply-{delay_ms}"),
            });
            let broker = CallBroker::new(client, Duration::from_millis(cadence_ms));
            let reply = broker
                .call("wf", tool_call(), || {
                    // Progress must never fire after completion-and-return;
                    // nothing to assert here beyond not observing a reply.
                })
                .await
                .expect("fake client never errors");
            assert!(
                completed.load(Ordering::SeqCst),
                "reply observable before the update completed"
            );
            match reply {
                ToolCallReply::Completed(result) => {
                    assert_eq!(result, ToolCallResult::text(format!("reply-{delay_ms}")));
                }
                other => panic!("unexpected reply: {other:?}"),
            }
        });
    }

    // keepalive cadence bound
    //
    // For any (update duration T, cadence k, harness timeout H) with
    // k < H, the gap between consecutive MCP tool server emissions observable by
    // the harness (progress ticks and the final reply) never reaches H.
    #[test]
    fn p5_keepalive_cadence_bound(
        duration_ms in 0u64..600_000,
        cadence_ms in 5u64..30_000,
        headroom_ms in 1u64..30_000,
    ) {
        let timeout_ms = cadence_ms + headroom_ms; // H > k by construction
        let runtime = paused_runtime();
        runtime.block_on(async move {
            let observations: Arc<std::sync::Mutex<Vec<tokio::time::Instant>>> =
                Arc::new(std::sync::Mutex::new(vec![tokio::time::Instant::now()]));
            let client = Arc::new(FakeToolCallDispatcher {
                delay: Duration::from_millis(duration_ms),
                completed: Arc::new(AtomicBool::new(false)),
                reply_text: "done".to_owned(),
            });
            let broker = CallBroker::new(client, Duration::from_millis(cadence_ms));
            let progress_log = observations.clone();
            broker
                .call("wf", tool_call(), move || {
                    progress_log
                        .lock()
                        .expect("observation lock")
                        .push(tokio::time::Instant::now());
                })
                .await
                .expect("fake client never errors");
            let mut stamps = observations.lock().expect("observation lock").clone();
            stamps.push(tokio::time::Instant::now()); // the final reply

            let timeout = Duration::from_millis(timeout_ms);
            for pair in stamps.windows(2) {
                let gap = pair[1].duration_since(pair[0]);
                assert!(
                    gap < timeout.max(Duration::from_millis(cadence_ms + 1)),
                    "silent gap {gap:?} reached the harness timeout {timeout:?} \
                     (cadence {cadence_ms}ms, duration {duration_ms}ms)"
                );
            }
        });
    }
}
