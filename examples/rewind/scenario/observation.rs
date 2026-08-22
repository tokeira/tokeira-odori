//! Durable-state observations used to prove worker-replacement recovery.

use anyhow::{Context as _, Result, anyhow};
use temporalio_client::{UntypedWorkflow, WorkflowDescribeOptions};

#[derive(Debug)]
pub(super) struct PendingActivityObservation {
    pub(super) attempt: i32,
    pub(super) state: String,
}

pub(super) async fn pending_activity(
    client: &temporalio_client::Client,
    workflow_id: &str,
) -> Result<Option<PendingActivityObservation>> {
    let handle = client.get_workflow_handle::<UntypedWorkflow>(workflow_id);
    let description = handle
        .describe(WorkflowDescribeOptions::default())
        .await
        .context("describe restart-canary workflow")?;
    Ok(description
        .raw()
        .pending_activities
        .iter()
        .max_by_key(|activity| activity.attempt)
        .map(|activity| PendingActivityObservation {
            attempt: activity.attempt,
            state: format!("{:?}", activity.state()),
        }))
}

pub(super) async fn wait_for_attempt(
    client: &temporalio_client::Client,
    workflow_id: &str,
    minimum_attempt: i32,
    wait_for: std::time::Duration,
) -> Result<i32> {
    let deadline = tokio::time::Instant::now() + wait_for;
    loop {
        let observed_attempt = pending_activity(client, workflow_id)
            .await?
            .map(|activity| activity.attempt)
            .unwrap_or_default();
        if observed_attempt >= minimum_attempt {
            return Ok(observed_attempt);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(anyhow!(
                "engine did not durably advance the restart-canary activity to attempt {minimum_attempt} within {wait_for:?}; last observed attempt was {observed_attempt}"
            ));
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}
