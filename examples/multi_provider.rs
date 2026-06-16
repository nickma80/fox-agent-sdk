/// multi_provider: demonstrates switching providers/models at runtime.
use fox_agent_sdk::{
    Agent, AgentEvent, DefaultModel, FoxAgentSdkConfig, Harness, MockProvider, Model,
    StreamEvent, TurnOutcome,
};
use std::sync::Arc;

#[tokio::main]
async fn main() {
    println!("=== Multi-Provider Demo ===\n");

    let provider_openai = Arc::new(MockProvider::new("openai"));
    let model_openai: Arc<dyn Model> = Arc::new(DefaultModel::new(provider_openai.clone(), "gpt-4o"));
    let harness = Harness::new(FoxAgentSdkConfig::default(), None);
    let mut agent = Agent::new(model_openai.clone(), harness);

    provider_openai.push_script(vec![
        StreamEvent::TextDelta { text: "gpt-4o responding".to_string() },
        StreamEvent::MessageStop { stop_reason: None },
    ]);

    let (tx, _rx) = tokio::sync::mpsc::channel::<AgentEvent>(16);
    let outcome = agent.run_once_streaming("which model?", &tx).await.unwrap();
    match outcome {
        TurnOutcome::Completed { text } => {
            println!("[openai] {}", text);
            assert!(text.contains("gpt-4o"));
        }
        _ => panic!("expected Completed"),
    }

    agent.model().set_model("claude-sonnet-4-20250514").unwrap();
    println!("[agent] switched to claude-sonnet-4-20250514");
    println!("\n=== PASSED ===");
}
