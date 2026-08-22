//! Embedded engine startup and bounded fleet-event observation.

use std::net::TcpListener;

use anyhow::{Context as _, Result};
use tokeira_engine::{Engine, TokeiraConfig};

use super::state::FleetEvent;

pub(super) async fn start_engine() -> Result<(Engine, TcpListener, TcpListener)> {
    let grpc_guard = TcpListener::bind("127.0.0.1:0")?;
    let nexus_guard = TcpListener::bind("127.0.0.1:0")?;
    let mut config = TokeiraConfig::default();
    config.infrastructure.network.grpc_addr = grpc_guard.local_addr()?.to_string();
    config.policy.nexus_completion.http_addr = nexus_guard.local_addr()?.to_string();
    let engine = Engine::start_with_config(config).await?;
    Ok((engine, grpc_guard, nexus_guard))
}

pub(super) async fn next_event(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<FleetEvent>,
) -> Result<FleetEvent> {
    tokio::time::timeout(std::time::Duration::from_secs(30), receiver.recv())
        .await
        .context("fleet event timed out")?
        .context("fleet event channel closed")
}
