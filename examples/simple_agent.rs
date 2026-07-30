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
    GoalScope, MilestoneStatus, PlanStatus, TodoStatus, load_goals_with_store,
    load_plan_with_store, load_todos_with_store,
};
use fox_agent_sdk::{
    AgentBuilder, AgentEvent, FoxAgentSdkConfig, InMemoryPlanningStore, PermissionDecision,
    PlanningStore, ProviderConfig, TurnOutcome,
};
use std::path::PathBuf;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ── Read API key from environment ──
    let api_key = std::env::var("DEEPSEEK_API_KEY")
        .unwrap_or_else(|_| "sk-eb41069e23244d0fb40e86d872238d92".to_string());
    // ── Shared planning store so we can read state after the turn ──
    let planning_store: Arc<dyn PlanningStore> = Arc::new(InMemoryPlanningStore::default());

    // ── Load project config (agent.toml + AGENTS.md from project root) ──
    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cfg = FoxAgentSdkConfig::load_from_file(project_root.join("agent.toml"))
        .unwrap_or_else(|_| FoxAgentSdkConfig::default());

    // ── Build agent with planning tools + store ──
    let agent = AgentBuilder::new()
        .working_dir(&project_root)
        .sdk_config(cfg)
        .with_global_agents_md_path(project_root.join("AGENTS.md"))
        .provider_config(ProviderConfig::deepseek(api_key))
        .model_id("deepseek-v4-flash")
        .with_planning_store(planning_store.clone())
        .with_default_tools()
        .build()
        .await?;

    let session_id = agent.harness().session_state.read().await.id.clone();

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
                    print!("\n [TextDelta:] {text}");
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

    // ── Run turn loop: handle permission requests interactively ──
    let mut outcome = agent.run_once_streaming(&prompt, &tx).await?;

    loop {
        match outcome {
            TurnOutcome::Completed { .. } => {
                println!("\n=== Done ===");
                break;
            }
            TurnOutcome::RequiresUserDecision { request } => {
                println!();
                println!("[!] Permission required — {}", request.prompt);
                println!(
                    "    Tool: {} | Risk: {:?} | Policy: {}",
                    request.tool_name, request.risk_level, request.policy_source
                );
                println!("    Summary: {}", request.tool_summary);
                println!();
                println!("    [a]llow  [d]eny  [A]llow all (this session)");

                use std::io::{self, Write};
                print!("> ");
                io::stdout().flush().ok();

                let mut input = String::new();
                io::stdin().read_line(&mut input).ok();
                let decision = match input.trim().to_lowercase().as_str() {
                    "a" => PermissionDecision::Allow,
                    "d" => PermissionDecision::Deny {
                        reason: "user denied".to_string(),
                    },
                    "A" => {
                        // Approve all: allow this one, then set a permissive policy
                        println!("    [Approving all future requests this session]");
                        // TODO: configure SafetySystem to auto-approve
                        PermissionDecision::Allow
                    }
                    _ => {
                        println!("    Unrecognised input, defaulting to deny.");
                        PermissionDecision::Deny {
                            reason: "unrecognised input".to_string(),
                        }
                    }
                };

                // Resume the turn with the user's decision
                outcome = agent.resume_streaming(decision, &tx).await?;
            }
            TurnOutcome::Cancelled => {
                println!("\n[!] Turn was cancelled");
                break;
            }
            TurnOutcome::Failed { error } => {
                println!("\n[!] Agent failed: {error}");
                break;
            }
        }
    }

    // drop(tx);
    handle.await.ok();

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
            println!(
                "{focus} [{:?}|{}%|{:?}] {}",
                g.status, g.progress, g.scope, g.title
            );
            for m in &g.milestones {
                let icon = match m.status {
                    MilestoneStatus::Completed => "✓",
                    MilestoneStatus::InProgress => "→",
                    MilestoneStatus::Pending => "○",
                };
                println!("    {icon} {}", m.content);
            }
            if !g.checkpoints.is_empty() {
                println!(
                    "  {} checkpoints (latest: {})",
                    g.checkpoints.len(),
                    g.checkpoints
                        .last()
                        .map(|c| c.summary.as_str())
                        .unwrap_or("-")
                );
            }
        }
    }

    // ── Plan ──
    let plan = load_plan_with_store(store, session_id);
    if !plan.items.is_empty() {
        let done = plan
            .items
            .iter()
            .filter(|i| i.status == PlanStatus::Completed)
            .count();
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
        let done = todos
            .iter()
            .filter(|t| t.status == TodoStatus::Completed)
            .count();
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
