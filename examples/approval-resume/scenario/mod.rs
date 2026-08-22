//! The approval-resume scenario lifecycle.
//!
//! Process one records a proposal and snapshots a live workflow. Process two
//! restores that workflow, records the human decision, and completes the
//! approved durable tools.
#![allow(dead_code)] // The CLI and integration test consume different report surfaces.

mod model;
mod provider;
mod runtime;
mod tools;
mod workspace;

use std::{fs, path::Path, sync::Arc};

use anyhow::{Result, ensure};
use odori::RunEnd;

use model::{ApprovalCompletion, ApprovalDecision, PatchProposal, proposal};
use runtime::{RUN_ID, start_engine, start_runtime, wait_for_transcript};
use tools::ApprovalState;
use workspace::{
    ALLOWED_PATH, APPROVAL_REQUEST, BROKEN_LIB, FIXED_LIB, SNAPSHOT_FILE, WORKSPACE, seed,
    test_succeeds,
};

pub const PLAN_HASH: &str = model::PLAN_HASH;

/// Evidence returned after process one has reached the approval boundary.
#[derive(Debug)]
pub struct PrepareReport {
    pub proposal: PatchProposal,
    pub snapshot_bytes: u64,
}

/// Evidence returned after process two restores and completes the run.
#[derive(Debug)]
pub struct ResumeReport {
    pub completion: ApprovalCompletion,
    pub turns_before_approval: usize,
    pub turns_after_completion: usize,
    pub apply_executions: u64,
    pub finish_bar_executions: u64,
}

/// Record one typed proposal, persist the live workflow to disk, and exit.
pub async fn prepare(state_directory: &Path, print: bool) -> Result<PrepareReport> {
    let workspace = seed(state_directory)?;
    ensure!(
        !test_succeeds(&workspace)?,
        "the bundled fixture must start with a failing test"
    );
    let snapshot = state_directory.join(SNAPSHOT_FILE);
    let state = Arc::new(ApprovalState::waiting(workspace));
    let engine = start_engine(&snapshot).await?;
    let runtime = start_runtime(&engine, state.clone()).await?;
    let conversation = runtime
        .runner()
        .start_conversation(
            "approval-worker",
            "Diagnose the failing increment test and propose one bounded patch. Do not apply it before human approval.",
            RUN_ID,
        )
        .await?;
    let transcript = wait_for_transcript(&conversation, 1).await?;
    ensure!(
        transcript.len() == 1,
        "prepare must stop after the proposal turn"
    );
    let proposal: PatchProposal = serde_json::from_str(&transcript[0].text)?;
    ensure!(proposal == self::proposal());
    ensure!(state.apply_executions() == 0);
    fs::write(
        state_directory.join(APPROVAL_REQUEST),
        serde_json::to_vec_pretty(&proposal)?,
    )?;
    drop(conversation);
    runtime.shutdown().await?;
    engine.shutdown().await?;
    let snapshot_bytes = fs::metadata(&snapshot)?.len();
    ensure!(snapshot_bytes > 0, "engine snapshot is empty");
    ensure!(
        !test_succeeds(state.workspace())?,
        "the workspace changed before approval"
    );
    if print {
        println!("HUMAN APPROVAL REQUIRED: {}", proposal.plan_hash);
        println!(
            "REQUEST: {}",
            state_directory.join(APPROVAL_REQUEST).display()
        );
        println!(
            "SNAPSHOT WRITTEN: {} ({snapshot_bytes} bytes)",
            snapshot.display()
        );
        println!(
            "PROCESS {} EXITING WITH LIVE WORKFLOW {RUN_ID}",
            std::process::id()
        );
    }
    Ok(PrepareReport {
        proposal,
        snapshot_bytes,
    })
}

/// Restore the live workflow, record the human decision, and finish exactly once.
pub async fn resume(
    state_directory: &Path,
    approved_plan_hash: &str,
    print: bool,
) -> Result<ResumeReport> {
    let snapshot = state_directory.join(SNAPSHOT_FILE);
    ensure!(
        snapshot.is_file(),
        "missing snapshot {}",
        snapshot.display()
    );
    let proposal: PatchProposal =
        serde_json::from_slice(&fs::read(state_directory.join(APPROVAL_REQUEST))?)?;
    ensure!(
        approved_plan_hash == proposal.plan_hash,
        "approval hash {approved_plan_hash:?} does not match proposal {:?}",
        proposal.plan_hash
    );
    let state = Arc::new(ApprovalState::approved(
        state_directory.join(WORKSPACE),
        approved_plan_hash.to_owned(),
    ));
    let engine = start_engine(&snapshot).await?;
    let runtime = start_runtime(&engine, state.clone()).await?;
    let conversation = runtime.runner().resume_conversation(RUN_ID);
    let transcript_before = wait_for_transcript(&conversation, 1).await?;
    ensure!(transcript_before.len() == 1);
    let restored_proposal: PatchProposal = serde_json::from_str(&transcript_before[0].text)?;
    ensure!(restored_proposal == proposal, "restored proposal changed");
    let decision = serde_json::to_string(&ApprovalDecision {
        decision: "approve".to_owned(),
        plan_hash: approved_plan_hash.to_owned(),
    })?;
    conversation.send(&decision).await?;
    let output = conversation.end().await?;
    ensure!(matches!(output.end, RunEnd::ConversationEnded));
    ensure!(
        output.turns == 2,
        "restored workflow should contain two turns"
    );
    let completion: ApprovalCompletion = serde_json::from_str(&output.text)?;
    ensure!(completion.plan_hash == approved_plan_hash);
    ensure!(
        completion.session_forked,
        "provider session lineage was not forked from the restored turn"
    );
    let completed = runtime.runner().resume_conversation(RUN_ID);
    let transcript_after = completed.transcript().await?;
    ensure!(transcript_after.len() == 2);
    ensure!(transcript_after[1].input == decision);
    let apply_executions = state.apply_executions();
    let finish_bar_executions = state.finish_bar_executions();
    ensure!(
        apply_executions == 1,
        "approved patch executed {apply_executions} times"
    );
    ensure!(
        finish_bar_executions == 1,
        "finish bar executed {finish_bar_executions} times"
    );
    ensure!(fs::read_to_string(state.workspace().join(ALLOWED_PATH))? == FIXED_LIB);
    ensure!(test_succeeds(state.workspace())?);
    runtime.shutdown().await?;
    engine.shutdown().await?;
    if print {
        println!(
            "RESTORED: {} with one recorded proposal turn",
            snapshot.display()
        );
        println!("HUMAN APPROVAL RECORDED: {approved_plan_hash}");
        println!("APPLIED ONCE: {ALLOWED_PATH}");
        println!("GREEN: cargo test --locked");
        println!("PROCESS {} COMPLETED WORKFLOW {RUN_ID}", std::process::id());
    }
    Ok(ResumeReport {
        completion,
        turns_before_approval: transcript_before.len(),
        turns_after_completion: transcript_after.len(),
        apply_executions,
        finish_bar_executions,
    })
}

/// Verify the filesystem evidence left by a completed two-process run.
pub fn verify_completed_state(state_directory: &Path) -> Result<()> {
    let snapshot = state_directory.join(SNAPSHOT_FILE);
    ensure!(snapshot.is_file() && fs::metadata(snapshot)?.len() > 0);
    ensure!(fs::read_to_string(state_directory.join(WORKSPACE).join(ALLOWED_PATH))? == FIXED_LIB);
    ensure!(test_succeeds(&state_directory.join(WORKSPACE))?);
    Ok(())
}

/// Verify that process one left the proposal unapplied and recoverable.
pub fn verify_waiting_state(state_directory: &Path) -> Result<()> {
    let snapshot = state_directory.join(SNAPSHOT_FILE);
    ensure!(snapshot.is_file() && fs::metadata(snapshot)?.len() > 0);
    let proposal: PatchProposal =
        serde_json::from_slice(&fs::read(state_directory.join(APPROVAL_REQUEST))?)?;
    ensure!(proposal == self::proposal());
    let workspace = state_directory.join(WORKSPACE);
    ensure!(fs::read_to_string(workspace.join(ALLOWED_PATH))? == BROKEN_LIB);
    ensure!(!test_succeeds(&workspace)?);
    Ok(())
}
