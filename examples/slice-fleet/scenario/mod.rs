//! The slice-fleet scenario lifecycle and observable report.

mod agents;
mod bridge;
mod model;
mod provider;
mod runtime;
mod state;
mod tools;
mod workspace;

use std::{
    collections::BTreeMap,
    process::Command,
    sync::{Arc, Mutex},
};

use anyhow::{Result, anyhow, ensure};
use odori::{Providers, RunEnd, RunOutput};
use odori_engine::{ConnectTarget, OdoriRuntime};
use odori_mcp_bridge::BridgeConfig;

use agents::registry;
use model::{PLAN_HASH, SlicePlan};
use provider::FleetProvider;
use runtime::{next_event, start_engine};
use state::{Evidence, FleetEvent, FleetState};
use workspace::{TempFixture, run_id};

/// Evidence returned by the deterministic full-path example and test.
#[derive(Debug)]
pub struct FleetReport {
    pub output: RunOutput,
    pub plan: SlicePlan,
    pub applied: Vec<String>,
    pub finish_bars: BTreeMap<String, Vec<String>>,
    pub reviews: BTreeMap<String, String>,
    pub scope_refusals: u32,
    pub budget_exceeded: bool,
    pub raised: bool,
}

/// Run the complete fleet with a scripted harness over the real embedded
/// engine and HTTP bridge. Every signal, child workflow, update, and tool
/// result is real; only model choice is scripted for determinism.
pub async fn run_scripted_fleet(print: bool) -> Result<FleetReport> {
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = Arc::new(FleetState {
        fixture: TempFixture::new()?,
        evidence: Mutex::new(Evidence::default()),
        events: sender,
    });
    let claude = Arc::new(FleetProvider {
        name: "claude-scripted",
        state: state.clone(),
    });
    let codex = Arc::new(FleetProvider {
        name: "codex-scripted",
        state: state.clone(),
    });
    let (embedded, _grpc_guard, _nexus_guard) = start_engine().await?;
    let runtime = OdoriRuntime::builder("example-slice-fleet")
        .connect(ConnectTarget::service_override(embedded.service_override()))
        .agents(registry(state.clone()))
        .providers(Providers::new(codex).with(claude))
        .bridge(BridgeConfig::default())
        .start()
        .await?;

    // The planner is a normal durable AgentRun and the runner enforces the
    // typed decode before anything can reach the approval seat.
    let odori::Json(typed_plan): odori::Json<SlicePlan> = runtime
        .runner()
        .run(
            "orchestrator",
            "GOAL: produce the bounded slice plan for the bundled fixture",
            &run_id("slice-plan"),
        )
        .await?;
    let FleetEvent::Plan(planned_event) = next_event(&mut receiver).await? else {
        return Err(anyhow!("typed plan run emitted the wrong event"));
    };
    ensure!(typed_plan == planned_event);

    let conversation = runtime
        .runner()
        .start_conversation(
            "orchestrator",
            "GOAL: repair and extend the bundled fixture under the fleet policy",
            &run_id("slice-fleet"),
        )
        .await?;
    let FleetEvent::Plan(plan) = next_event(&mut receiver).await? else {
        return Err(anyhow!("plan was not the first fleet event"));
    };
    ensure!(plan == typed_plan);
    if print {
        println!(
            "PLAN {}\n{}",
            plan.hash,
            serde_json::to_string_pretty(&plan)?
        );
        println!("HUMAN APPROVAL: plan {}", plan.hash);
    }
    conversation
        .send(&format!("APPROVE PLAN {}", plan.hash))
        .await?;
    ensure!(matches!(
        next_event(&mut receiver).await?,
        FleetEvent::SlicesReady
    ));
    if print {
        let (finish_bars, reviews) = {
            let evidence = state.evidence.lock().expect("fleet evidence lock");
            (evidence.finish_bars.clone(), evidence.reviews.clone())
        };
        for slice in ["increment-bugfix", "double-feature"] {
            println!("SCOPE FENCE: {slice} Cargo.toml -> tool error");
            println!(
                "FINISH BAR: {slice} -> {:?} -> green",
                finish_bars.get(slice).expect("finish bar recorded")
            );
            println!(
                "HOSTILE REVIEW: {} -> approve",
                reviews.get(slice).expect("review recorded")
            );
        }
        println!("BUDGET: budget-worker -> BudgetExceeded(max_turns=0)");
        println!("RAISE: contract-worker -> operator approval seat");
    }

    // Negative proof: the same apply signal is refused before the seat
    // records its item-level approval.
    conversation.send("APPROVE APPLY increment-bugfix").await?;
    match next_event(&mut receiver).await? {
        FleetEvent::ApplyRefused(slice) if slice == "increment-bugfix" => {
            if print {
                println!("APPROVAL GATE: apply increment-bugfix before approval -> tool error");
            }
        }
        event => return Err(anyhow!("unexpected approval-gate event: {event:?}")),
    }

    for slice in ["increment-bugfix", "double-feature"] {
        state.approve(slice);
        if print {
            println!("HUMAN APPROVAL: apply {slice}");
        }
        conversation.send(&format!("APPROVE APPLY {slice}")).await?;
        match next_event(&mut receiver).await? {
            FleetEvent::Applied(applied) if applied == slice => {}
            event => return Err(anyhow!("unexpected apply event: {event:?}")),
        }
    }
    conversation
        .send("RAISE DECISION keep frozen contract; do not apply contract slice")
        .await?;
    ensure!(matches!(
        next_event(&mut receiver).await?,
        FleetEvent::RaiseObserved
    ));
    let output = conversation.end().await?;

    let final_bar = Command::new("cargo")
        .args(["test", "--locked"])
        .current_dir(state.fixture.copy("integrated"))
        .output()?;
    ensure!(
        final_bar.status.success(),
        "integrated finish bar failed: {}",
        String::from_utf8_lossy(&final_bar.stderr)
    );
    let report = {
        let evidence = state.evidence.lock().expect("fleet evidence lock");
        FleetReport {
            output,
            plan,
            applied: evidence.applied.iter().cloned().collect(),
            finish_bars: evidence.finish_bars.clone(),
            reviews: evidence.reviews.clone(),
            scope_refusals: evidence.scope_refusals,
            budget_exceeded: evidence.budget_exceeded,
            raised: evidence.raised,
        }
    };
    runtime.shutdown().await?;
    embedded.shutdown().await?;
    Ok(report)
}

pub fn verify_fleet(report: &FleetReport) -> Result<()> {
    ensure!(matches!(report.output.end, RunEnd::ConversationEnded));
    // Six direct approval/orchestration turns plus five delegated worker,
    // reviewer, and Raise turns: child spend is parent spend.
    ensure!(report.output.turns == 11);
    ensure!(report.output.usage.input_tokens == 440);
    ensure!(report.output.usage.output_tokens == 110);
    ensure!(report.plan.hash == PLAN_HASH);
    ensure!(report.applied == ["double-feature", "increment-bugfix"]);
    ensure!(report.scope_refusals == 2);
    ensure!(report.budget_exceeded);
    ensure!(report.raised);
    ensure!(report.finish_bars.len() == 2);
    ensure!(report.reviews.get("increment-bugfix").map(String::as_str) == Some("codex-scripted"));
    ensure!(report.reviews.get("double-feature").map(String::as_str) == Some("claude-scripted"));
    Ok(())
}
