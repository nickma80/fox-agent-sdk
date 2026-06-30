/// multi_provider: demonstrates switching models at runtime via AgentBuilder.
///
/// Covers:
/// - Building an Agent with one model via AgentBuilder
/// - Runtime model switching via `agent.set_model()`
/// - Using MockProvider for deterministic testing
use fox_agent_sdk::{
    AgentBuilder, AgentEvent, MockProvider, StreamEvent, TurnOutcome,
};
use std::sync::Arc;

#[tokio::main]
async fn main() {
    println!("=== Multi-Provider Demo ===\n");

    // ── Build agent with initial model ──
    let provider_openai = Arc::new(MockProvider::new("openai"));

    provider_openai.push_script(vec![
        StreamEvent::TextDelta {
            text: "gpt-4o responding".to_string(),
        },
        StreamEvent::MessageStop { stop_reason: None },
    ]);

    let mut agent = AgentBuilder::new()
        .with_provider(provider_openai.clone())
        .model_id("gpt-4o")
        .build()
        .await
        .expect("build agent");

    // ── Run with initial model ──
    let (tx, _rx) = tokio::sync::mpsc::channel::<AgentEvent>(16);
    let outcome = agent
        .run_once_streaming("which model?", &tx)
        .await
        .unwrap();
    match outcome {
        TurnOutcome::Completed { text } => {
            println!("[openai] {text}");
            assert!(text.contains("gpt-4o"));
        }
        _ => panic!("expected Completed"),
    }

    // ── Switch model at runtime ──
    agent
        .set_model("claude-sonnet-4-20250514")
        .unwrap();
    println!("[agent] switched to claude-sonnet-4-20250514");
    println!("\n=== PASSED ===");
}
