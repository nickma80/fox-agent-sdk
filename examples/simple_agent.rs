/// simple_agent: runs a single Agent with the real DeepSeek API.
///
/// Usage:
///   DEEPSEEK_API_KEY="sk-xxx" cargo run --example simple_agent
///
/// Supports all DeepSeek v4 features (thinking, prefix caching, streaming).
use fox_agent_sdk::{
    Agent, AgentEvent, DeepSeekProvider, DefaultModel, FoxAgentSdkConfig, Harness, Model,
    ProviderConfig, TurnOutcome,
};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ── Read API key from environment ──
    let api_key = std::env::var("DEEPSEEK_API_KEY")
        .expect("Set DEEPSEEK_API_KEY to your DeepSeek API key");

    // ── Provider + Model ──
    // ProviderConfig::deepseek() sets the correct base URL, auth, and defaults.
    let provider = Arc::new(DeepSeekProvider::new(ProviderConfig::deepseek(api_key)));
    let model: Arc<dyn Model> = Arc::new(DefaultModel::new(provider, "deepseek-v4-flash"));

    // ── Harness + Agent ──
    let harness = Harness::new(FoxAgentSdkConfig::default(), None);
    harness.register_default_tools().await;
    let mut agent = Agent::new(model, harness);

    println!("=== Fox Agent SDK — Simple Agent (DeepSeek v4) ===\n");

    // Optional: provide a custom query via CLI args
    let prompt = std::env::args().nth(1).unwrap_or_else(|| {
        "Write a hello world Rust program that prints the current date.".to_string()
    });
    println!("[user] {prompt}\n");

    // ── Run agent and display events in real-time ──
    let (tx, mut rx) = tokio::sync::mpsc::channel::<AgentEvent>(64);

    let handle = tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            match ev {
                AgentEvent::ModelTextDelta { text } => {
                    print!("{text}");
                    use std::io::Write;
                    std::io::stdout().flush().ok();
                }
                AgentEvent::ModelThinkingDelta { text } => {
                    // Thinking/reasoning content shown in dimmed style
                    print!("\x1b[90m{text}\x1b[0m");
                    use std::io::Write;
                    std::io::stdout().flush().ok();
                }
                AgentEvent::ModelUsage { usage } => {
                    println!(
                        "\n\n--- Usage ---\n  input: {}  output: {}  total: {}",
                        usage.input_tokens, usage.output_tokens, usage.total_tokens,
                    );
                }
                AgentEvent::ToolCallStart { name, input, .. } => {
                    println!("\n\n[⚡ Tool: {name}]");
                    println!("  input: {input}");
                }
                AgentEvent::ToolCallEnd { output, .. } => {
                    println!("  result: {} (error: {})", &output.text[..output.text.len().min(200)], output.is_error);
                }
                AgentEvent::Error { error } => {
                    eprintln!("\n[ERROR] {error}");
                }
                _ => {}
            }
        }
    });

    let outcome = agent.run_once_streaming(&prompt, &tx).await?;

    handle.await.ok();

    println!();

    match outcome {
        TurnOutcome::Completed { text: _ } => {
            println!("\n=== Done ===");
        }
        TurnOutcome::RequiresUserDecision { request } => {
            println!("\n[!] Agent needs permission: {}", request.prompt);
            // In a real app you'd prompt the user here, then call
            // agent.resume_streaming(PermissionDecision::Allow, &tx).await;
        }
        TurnOutcome::Cancelled => {
            println!("\n[!] Turn was cancelled");
        }
        TurnOutcome::Failed { error } => {
            println!("\n[!] Agent failed: {error}");
        }
    }

    Ok(())
}
