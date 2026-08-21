//! Live smokes for the API-backed provider tier, one per provider, keyed
//! on the key variable being present and `#[ignore]`d by default (they
//! spend real API credit):
//!
//! ```console
//! cargo test --manifest-path tests/embedded/Cargo.toml --test api_live -- --ignored
//! ```

use odori_agents::provider::{
    AgentDirectives, Provider, SessionDirective, TurnEventSink, TurnIdentity, TurnRequest,
};
use tokio::sync::mpsc;

fn request(input: &str) -> TurnRequest {
    TurnRequest::new(
        TurnIdentity {
            run_id: "run-api-live".to_owned(),
            turn: 0,
            attempt: 1,
        },
        AgentDirectives::new("smoke", "Reply with exactly what the user asks for."),
        input,
        SessionDirective::Start,
    )
}

fn sink() -> TurnEventSink {
    let (sender, receiver) = mpsc::channel(256);
    std::mem::forget(receiver);
    TurnEventSink::new(sender)
}

#[tokio::test]
#[ignore = "spends API credit; needs ANTHROPIC_API_KEY"]
async fn anthropic_api_live_smoke() -> anyhow::Result<()> {
    if std::env::var("ANTHROPIC_API_KEY").is_err() {
        println!("skipped: ANTHROPIC_API_KEY not set");
        return Ok(());
    }
    let provider = odori_providers::AnthropicProvider::new();
    let outcome = provider
        .execute_turn(request("Reply with exactly the word: arabesque"), sink())
        .await?;
    assert!(
        outcome.text.to_lowercase().contains("arabesque"),
        "live answer: {}",
        outcome.text
    );
    assert!(outcome.usage.output_tokens.unwrap_or(0) > 0);
    Ok(())
}

#[tokio::test]
#[ignore = "spends API credit; needs OPENAI_API_KEY"]
async fn openai_api_live_smoke() -> anyhow::Result<()> {
    if std::env::var("OPENAI_API_KEY").is_err() {
        println!("skipped: OPENAI_API_KEY not set");
        return Ok(());
    }
    let provider = odori_providers::OpenAiProvider::new();
    let outcome = provider
        .execute_turn(request("Reply with exactly the word: arabesque"), sink())
        .await?;
    assert!(
        outcome.text.to_lowercase().contains("arabesque"),
        "live answer: {}",
        outcome.text
    );
    assert!(!outcome.session_id.is_empty(), "response id recorded");
    Ok(())
}
