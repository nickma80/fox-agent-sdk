/// simple_agent: runs a single Agent with the real DeepSeek API.
///
/// Demonstrates the full planning lifecycle — the Agent autonomously creates
/// goals, plans, and todos via tool calls, then the example reads planning
/// state back after execution.
///
/// Usage:
///   DEEPSEEK_API_KEY="sk-xxx" cargo run --example simple_agent "Plan a Rust CLI project"
///
/// Supports all DeepSeek v4 features (thinking, prefix caching, streaming).
use fox_agent_core::{
    GoalScope, MilestoneStatus, PlanStatus, TodoStatus,
    load_goals_with_store, load_plan_with_store, load_todos_with_store,
};
use fox_agent_sdk::{
    AgentBuilder, AgentEvent, InMemoryPlanningStore, PlanningStore,
    ProviderConfig, TurnOutcome,
};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ── Read API key from environment ──
    // let api_key = std::env::var("DEEPSEEK_API_KEY")
    //     .expect("Set DEEPSEEK_API_KEY to your DeepSeek API key");
    let api_key = "sk-eb41069e23244d0fb40e86d872238d92".to_string();
    // ── Shared planning store so we can read state after the turn ──
    let planning_store: Arc<dyn PlanningStore> = Arc::new(InMemoryPlanningStore::default());

    // ── Build agent with planning tools + store ──
    let mut agent = AgentBuilder::new()
        .provider_config(ProviderConfig::deepseek(api_key))
        .model_id("deepseek-v4-flash")
        .with_planning_store(planning_store.clone())
        .with_default_tools()
        .build()
        .await?;

    let session_id = agent.harness().session_state.id.clone();

    println!("=== Fox Agent SDK — Simple Agent (DeepSeek v4) ===\n");

    // Optional: provide a custom query via CLI args
    let prompt = std::env::args().nth(1).unwrap_or_else(|| {
        "Build a `git-summary` Rust CLI tool that prints per-author commit \
         statistics for a git repo. Create the full Cargo project with \
         Cargo.toml, a src/main.rs using the git2 crate, and verify it compiles."
            .to_string()
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
                    let preview: String = output.text.chars().take(200).collect();
                    println!("  result: {preview} (error: {})", output.is_error);
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
        }
        TurnOutcome::Cancelled => {
            println!("\n[!] Turn was cancelled");
        }
        TurnOutcome::Failed { error } => {
            println!("\n[!] Agent failed: {error}");
        }
    }

    // ── Read planning state (goals, plan, todos) ──
    println!();
    print_planning_state(planning_store.as_ref(), &session_id);

    Ok(())
}

/// Print the current planning state: goals, plan, and todos.
fn print_planning_state(store: &dyn PlanningStore, session_id: &str) {
    // ── Goals ──
    let session_goals = load_goals_with_store(store, session_id, GoalScope::Session);
    let global_goals = load_goals_with_store(store, session_id, GoalScope::Global);

    if !session_goals.is_empty() || !global_goals.is_empty() {
        println!("──── GOALS ────");
        for g in session_goals.iter().chain(global_goals.iter()) {
            let focus = if g.focused { "★" } else { " " };
            println!("{focus} [{:?}|{}%|{:?}] {}",
                g.status, g.progress, g.scope, g.title);
            for m in &g.milestones {
                let icon = match m.status {
                    MilestoneStatus::Completed => "✓",
                    MilestoneStatus::InProgress => "→",
                    MilestoneStatus::Pending => "○",
                };
                println!("    {icon} {}", m.content);
            }
            if !g.checkpoints.is_empty() {
                println!("  {} checkpoints (latest: {})",
                    g.checkpoints.len(),
                    g.checkpoints.last().map(|c| c.summary.as_str()).unwrap_or("-"));
            }
        }
    }

    // ── Plan ──
    let plan = load_plan_with_store(store, session_id);
    if !plan.items.is_empty() {
        let done = plan.items.iter().filter(|i| i.status == PlanStatus::Completed).count();
        println!("\n──── PLAN v{} ────", plan.version);
        for item in &plan.items {
            let icon = match item.status {
                PlanStatus::Completed => "✓",
                PlanStatus::InProgress => "→",
                PlanStatus::Pending => "○",
            };
            let deps = if item.blocked_by.is_empty() {
                String::new()
            } else {
                format!(" [← {}]", item.blocked_by.join(", "))
            };
            println!("{icon} [{:?}] {}{deps}", item.priority, item.content);
        }
        println!("{done}/{} completed", plan.items.len());
    }

    // ── Todos ──
    let todos = load_todos_with_store(store, session_id);
    if !todos.is_empty() {
        let done = todos.iter().filter(|t| t.status == TodoStatus::Completed).count();
        println!("\n──── TODOS ────");
        for t in &todos {
            let icon = match t.status {
                TodoStatus::Completed => "✓",
                TodoStatus::InProgress => "→",
                TodoStatus::Pending => "○",
            };
            println!("{icon} [{:?}] {}", t.priority, t.content);
        }
        println!("{done}/{} done", todos.len());
    }

    if session_goals.is_empty() && plan.items.is_empty() && todos.is_empty() {
        println!("[planning] No state — the Agent did not create goals/plans/todos this turn.");
    }
}
