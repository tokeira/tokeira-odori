//! Embedded-engine snapshot policy, Odori worker assembly, and durable observation.

use std::{path::Path, sync::Arc, time::Duration};

use anyhow::{Context as _, Result};
use odori::{Conversation, Providers, TurnRecord};
use odori_engine::{
    ConnectTarget, EmbeddedEngineConfig, EmbeddedStorageConfig, Engine, OdoriRuntime,
    SnapshotPolicyConfig, TokeiraConfig,
};
use odori_mcp_bridge::BridgeConfig;

use super::{
    provider::ApprovalProvider,
    tools::{ApprovalState, registry},
};

pub(super) const RUN_ID: &str = "approval-resume-run";
const TASK_QUEUE: &str = "example-approval-resume";

pub(super) async fn start_engine(
    snapshot: &Path,
    storage: EmbeddedStorageConfig,
) -> Result<Engine> {
    let mut config = TokeiraConfig::default();
    if matches!(&storage, EmbeddedStorageConfig::InMemory) {
        config.policy.snapshot = Some(SnapshotPolicyConfig {
            location: snapshot.to_path_buf(),
            interval_ms: 3_600_000,
        });
    }
    Engine::start_with_embedded_config(EmbeddedEngineConfig {
        server: config,
        storage,
        ..EmbeddedEngineConfig::default()
    })
    .await
    .map_err(Into::into)
}

pub(super) async fn start_runtime(
    engine: &Engine,
    state: Arc<ApprovalState>,
) -> Result<OdoriRuntime> {
    OdoriRuntime::builder(TASK_QUEUE)
        .connect(ConnectTarget::service_override(engine.service_override()))
        .agents(registry(state))
        .providers(Providers::new(Arc::new(ApprovalProvider)))
        .bridge(BridgeConfig::default())
        .start()
        .await
}

pub(super) async fn wait_for_transcript(
    conversation: &Conversation,
    minimum_turns: usize,
) -> Result<Vec<TurnRecord>> {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let transcript = conversation.transcript().await?;
            if transcript.len() >= minimum_turns {
                return Ok(transcript);
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .with_context(|| format!("workflow did not record {minimum_turns} turn(s)"))?
}
