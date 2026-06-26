/// planning_demo: Goal + Plan + Todo — Agent-driven orchestration.
///
/// This demo shows how a Fox Agent autonomously manages the three-tier
/// planning system through tool calls. The Agent:
///
///   1. Receives a high-level task from the user
///   2. Creates a Goal (strategic intent with milestones)
///   3. Breaks it down into a dependency-aware Plan
///   4. Generates a Todo list for immediate execution
///   5. Updates progress across turns (auto-checkpoint fires)
///
/// Unlike hard-coded examples, every Goal/Plan/Todo mutation is
/// performed by the Agent calling `goal`, `plan`, `todo` tools.
/// A shared InMemoryPlanningStore lets us verify state after each turn.
///
/// Run: cargo run --example planning_demo
use fox_agent_sdk::{
    AgentBuilder, AgentEvent, MockProvider, StreamEvent,
    InMemoryPlanningStore, PlanningStore,
    load_goals_with_store, load_plan_with_store, load_todos_with_store,
};
use fox_agent_core::{MilestoneStatus, PlanStatus, TodoStatus, GoalScope};
use std::sync::Arc;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() {
    println!(
        "╔══════════════════════════════════════════════════════╗\n\
         ║  Fox Agent SDK — Agent-Driven Goal / Plan / Todo ║\n\
         ╚══════════════════════════════════════════════════════╝\n"
    );

    // ── Shared store: Agent and demo code share the same PlanningStore ──
    let store: Arc<dyn PlanningStore> = Arc::new(InMemoryPlanningStore::default());

    // ── Build Agent with shared store + all default tools ──
    let provider = Arc::new(MockProvider::new("mock-agent"));

    let mut agent = AgentBuilder::new()
        .with_provider(provider.clone())
        .model_id("mock-1")
        .with_planning_store(store.clone())
        .with_default_tools()
        .build()
        .await
        .expect("build agent");

    let session_id = agent.harness().session_state.id.clone();
    println!("[setup] session_id = {session_id}");
    println!("[setup] Agent built with goal/plan/todo tools registered\n");

    // ═══════════════════════════════════════════════════════════════════════
    // Phase 1: Agent creates a Goal
    // ═══════════════════════════════════════════════════════════════════════
    println!("══════ Phase 1: Agent creates Goal ══════\n");

    // Mock: LLM responds with a goal tool call
    provider.push_script(vec![
        StreamEvent::ToolUse {
            id: "c1".into(),
            name: "goal".into(),
            input: serde_json::json!({
                "action": "create",
                "scope": "session",
                "title": "Develop a file deduplication CLI tool",
                "description": "Build a `dedup` CLI that scans directories, finds \
                    duplicate files by BLAKE3 content hash, and offers interactive removal.",
                "progress": 0,
                "milestones": [
                    {"id": "m1", "content": "Project scaffold + clap argument parser", "status": "pending"},
                    {"id": "m2", "content": "Recursive directory walker with BLAKE3 hashing", "status": "pending"},
                    {"id": "m3", "content": "Duplicate detection engine + report formatter", "status": "pending"},
                    {"id": "m4", "content": "Interactive removal with dry-run support", "status": "pending"}
                ]
            }),
        },
        StreamEvent::MessageStop { stop_reason: None },
    ]);
    // Mock: follow-up text
    provider.push_script(vec![
        StreamEvent::TextDelta {
            text: "I've created a focused goal: 'Develop a file deduplication CLI tool' \
                   with 4 milestones. Let me now break this down into an execution plan.".into(),
        },
        StreamEvent::MessageStop { stop_reason: None },
    ]);

    let (tx, mut rx) = mpsc::channel::<AgentEvent>(64);
    let handle = tokio::spawn(async move { while let Some(_) = rx.recv().await {} });
    let _ = agent.run_once_streaming(
        "I want to build a CLI tool called `dedup` that finds and removes \
         duplicate files by content hash.",
        &tx,
    ).await;
    drop(tx); handle.await.ok();

    // ── Verify: goal was actually created ──
    let goals = load_goals_with_store(store.as_ref(), &session_id, GoalScope::Session);
    assert!(!goals.is_empty(), "Agent should have created the goal");
    let g = &goals[0];
    println!("[verify] Goal created by Agent:");
    println!("  id      : {}", g.id);
    println!("  title   : {}", g.title);
    println!("  focused : {}  |  progress: {}%  |  status: {:?}", g.focused, g.progress, g.status);
    println!("  milestones ({}):", g.milestones.len());
    for m in &g.milestones {
        println!("    [{:>10}] {}", format!("{:?}", m.status), m.content);
    }
    println!("  checkpoints ({}):", g.checkpoints.len());
    for c in &g.checkpoints { println!("    · {}", c.summary); }
    println!();

    // ═══════════════════════════════════════════════════════════════════════
    // Phase 2: Agent breaks goal into a dependency-aware Plan
    // ═══════════════════════════════════════════════════════════════════════
    println!("══════ Phase 2: Agent creates Plan ══════\n");

    provider.push_script(vec![
        StreamEvent::ToolUse {
            id: "c2".into(),
            name: "plan".into(),
            input: serde_json::json!({
                "items": [
                    {"id": "p1", "content": "Scaffold Cargo project with clap", "status": "pending", "priority": "high"},
                    {"id": "p2", "content": "Implement recursive dir walker with BLAKE3", "status": "pending", "priority": "high", "blocked_by": ["p1"]},
                    {"id": "p3", "content": "Build duplicate group detector + report", "status": "pending", "priority": "high", "blocked_by": ["p2"]},
                    {"id": "p4", "content": "Interactive removal with dry-run", "status": "pending", "priority": "medium", "blocked_by": ["p3"]},
                    {"id": "p5", "content": "Integration tests + README", "status": "pending", "priority": "low", "blocked_by": ["p1"]}
                ]
            }),
        },
        StreamEvent::MessageStop { stop_reason: None },
    ]);
    provider.push_script(vec![
        StreamEvent::TextDelta {
            text: "Plan created with 5 items. Dependency chain: p1→p2→p3→p4, \
                   with p5 running in parallel after p1. Let me create the todo list.".into(),
        },
        StreamEvent::MessageStop { stop_reason: None },
    ]);

    let (tx, mut rx) = mpsc::channel::<AgentEvent>(64);
    let handle = tokio::spawn(async move { while let Some(_) = rx.recv().await {} });
    let _ = agent.run_once_streaming("Create an execution plan with dependencies.", &tx).await;
    drop(tx); handle.await.ok();

    let plan = load_plan_with_store(store.as_ref(), &session_id);
    assert_eq!(plan.items.len(), 5, "Agent should have created 5 plan items");
    println!("[verify] Plan created by Agent (v{}):", plan.version);
    println!("  Dependency graph: p1 ─┬─ p2 ── p3 ── p4");
    println!("                      └─ p5 (parallel)");
    for item in &plan.items {
        let deps = if item.blocked_by.is_empty() { "─".into() } else { item.blocked_by.join(", ") };
        println!("  [{:>7}|{:>6}] {}  ← {}", format!("{:?}", item.status),
            format!("{:?}", item.priority), item.content, deps);
    }
    println!();

    // ═══════════════════════════════════════════════════════════════════════
    // Phase 3: Agent creates Todo list
    // ═══════════════════════════════════════════════════════════════════════
    println!("══════ Phase 3: Agent creates Todos ══════\n");

    provider.push_script(vec![
        StreamEvent::ToolUse {
            id: "c3".into(),
            name: "todo".into(),
            input: serde_json::json!({
                "todos": [
                    {"id": "t1", "content": "Research clap derive API for subcommands", "status": "pending", "priority": "high"},
                    {"id": "t2", "content": "Compare BLAKE3 vs SHA-256 perf on large files", "status": "pending", "priority": "high"},
                    {"id": "t3", "content": "Set up indicatif progress bar", "status": "pending", "priority": "medium"},
                    {"id": "t4", "content": "Design terminal UI with crossterm", "status": "pending", "priority": "medium"}
                ]
            }),
        },
        StreamEvent::MessageStop { stop_reason: None },
    ]);
    provider.push_script(vec![
        StreamEvent::TextDelta {
            text: "Todo list ready with 4 immediate tasks. I'll start working on t1: \
                   Research clap derive API.".into(),
        },
        StreamEvent::MessageStop { stop_reason: None },
    ]);

    let (tx, mut rx) = mpsc::channel::<AgentEvent>(64);
    let handle = tokio::spawn(async move { while let Some(_) = rx.recv().await {} });
    let _ = agent.run_once_streaming("Create a todo list for the first tasks.", &tx).await;
    drop(tx); handle.await.ok();

    let todos = load_todos_with_store(store.as_ref(), &session_id);
    assert_eq!(todos.len(), 4, "Agent should have created 4 todos");
    println!("[verify] Todos created by Agent:");
    for t in &todos {
        println!("  [{:>11}|{:>6}] {}", format!("{:?}", t.status),
            format!("{:?}", t.priority), t.content);
    }
    println!();

    // ═══════════════════════════════════════════════════════════════════════
    // Phase 4: Agent advances progress — updates todos, plan, goal
    // ═══════════════════════════════════════════════════════════════════════
    println!("══════ Phase 4: Agent advances progress ══════\n");

    // Turn 4a: Agent marks t1+2 done, completes p1 in plan
    println!("── Turn 4a: Agent completes research todos + p1 ──");

    provider.push_script(vec![
        StreamEvent::ToolUse {
            id: "c4a".into(),
            name: "todo".into(),
            input: serde_json::json!({
                "todos": [
                    {"id": "t1", "content": "Research clap derive API for subcommands", "status": "completed", "priority": "high"},
                    {"id": "t2", "content": "Compare BLAKE3 vs SHA-256 perf on large files", "status": "completed", "priority": "high"}
                ],
                "merge": true
            }),
        },
        StreamEvent::ToolUse {
            id: "c4b".into(),
            name: "plan".into(),
            input: serde_json::json!({
                "items": [
                    {"id": "p1", "content": "Scaffold Cargo project with clap", "status": "completed", "priority": "high"}
                ],
                "merge": true
            }),
        },
        StreamEvent::MessageStop { stop_reason: None },
    ]);
    provider.push_script(vec![
        StreamEvent::TextDelta {
            text: "Research complete. Chose clap with derive macros for subcommand support. \
                   BLAKE3 confirmed — 6x faster than SHA-256 on 1GB files. \
                   p1 (scaffold) is done.".into(),
        },
        StreamEvent::MessageStop { stop_reason: None },
    ]);

    let (tx, mut rx) = mpsc::channel::<AgentEvent>(64);
    let handle = tokio::spawn(async move { while let Some(_) = rx.recv().await {} });
    let _ = agent.run_once_streaming(
        "I finished the research. clap and BLAKE3 are the right choices. \
         Project scaffold is complete too. Update the plan and todos.",
        &tx,
    ).await;
    drop(tx); handle.await.ok();

    // Verify todos updated
    let todos = load_todos_with_store(store.as_ref(), &session_id);
    let done = todos.iter().filter(|t| t.status == TodoStatus::Completed).count();
    println!("  [verify] todos: {done}/4 done");
    for t in &todos {
        println!("    [{:>11}] {}", format!("{:?}", t.status), t.content);
    }

    // Verify plan updated
    let plan = load_plan_with_store(store.as_ref(), &session_id);
    let p1 = plan.items.iter().find(|i| i.id == "p1").unwrap();
    println!("  [verify] plan: p1 status = {:?} (version {})", p1.status, plan.version);
    println!();

    // Turn 4b: Agent updates goal progress → triggers auto-checkpoint
    println!("── Turn 4b: Agent updates goal progress + auto-checkpoint ──");

    provider.push_script(vec![
        StreamEvent::ToolUse {
            id: "c5a".into(),
            name: "goal".into(),
            input: serde_json::json!({
                "action": "update",
                "scope": "session",
                "id": goals[0].id.clone(),
                "progress": 30,
                "milestones": [
                    {"id": "m1", "content": "Project scaffold + clap argument parser", "status": "completed"},
                    {"id": "m2", "content": "Recursive directory walker with BLAKE3 hashing", "status": "in_progress"},
                    {"id": "m3", "content": "Duplicate detection engine + report formatter", "status": "pending"},
                    {"id": "m4", "content": "Interactive removal with dry-run support", "status": "pending"}
                ]
            }),
        },
        StreamEvent::MessageStop { stop_reason: None },
    ]);
    provider.push_script(vec![
        StreamEvent::TextDelta {
            text: "Goal progress updated to 30%. Milestone 1 is done, \
                   milestone 2 is in progress. The auto-checkpoint system \
                   will track this progress point.".into(),
        },
        StreamEvent::MessageStop { stop_reason: None },
    ]);

    let (tx, mut rx) = mpsc::channel::<AgentEvent>(64);
    let handle = tokio::spawn(async move { while let Some(_) = rx.recv().await {} });
    let _ = agent.run_once_streaming(
        "Great progress! Update the goal: m1 is complete, m2 is started, \
         overall progress is 30%.",
        &tx,
    ).await;
    drop(tx); handle.await.ok();

    // Verify goal updated + auto-checkpoint fired
    let goals = load_goals_with_store(store.as_ref(), &session_id, GoalScope::Session);
    let g = &goals[0];
    println!("  [verify] goal progress: {}%", g.progress);
    for m in &g.milestones {
        println!("    milestone [{:>11}]: {}", format!("{:?}", m.status), m.content);
    }
    println!("    checkpoints: {} (includes auto-checkpoint from turn end)", g.checkpoints.len());
    for c in &g.checkpoints {
        println!("      · t={} | {} (progress: {}%)",
            c.at_secs, c.summary, c.progress.unwrap_or(0));
    }
    println!();

    // ═══════════════════════════════════════════════════════════════════════
    // Phase 5: Final multi-tier snapshot
    // ═══════════════════════════════════════════════════════════════════════
    println!("══════ Phase 5: Final Three-Tier Snapshot ══════\n");

    let goals = load_goals_with_store(store.as_ref(), &session_id, GoalScope::Session);
    let plan = load_plan_with_store(store.as_ref(), &session_id);
    let todos = load_todos_with_store(store.as_ref(), &session_id);

    let plan_done = plan.items.iter().filter(|i| i.status == PlanStatus::Completed).count();
    let todo_done = todos.iter().filter(|t| t.status == TodoStatus::Completed).count();

    println!("┌─────────────── GOAL ────────────────┐");
    for g in &goals {
        println!("│ ★ {:<31} │", g.title);
        println!("│ progress: {}%  status: {:?}", g.progress, g.status);
        println!("│ milestones: {}                         │", g.milestones.len());
        for m in &g.milestones {
            let icon = match m.status { MilestoneStatus::Completed => "✓", MilestoneStatus::InProgress => "→", MilestoneStatus::Pending => "○" };
            println!("│   {icon} {:<29} │", m.content);
        }
        println!("│ checkpoints: {}                        │", g.checkpoints.len());
    }
    println!("└─────────────────────────────────────┘\n");

    println!("┌─────────────── PLAN v{} ────────────────┐", plan.version);
    for item in &plan.items {
        let icon = match item.status { PlanStatus::Completed => "✓", PlanStatus::InProgress => "→", PlanStatus::Pending => "○" };
        println!("│ {icon} [{:>5}] {:<22} │", format!("{:?}", item.priority), item.content);
    }
    println!("│ {plan_done}/{} completed                    │", plan.items.len());
    println!("└─────────────────────────────────────┘\n");

    println!("┌─────────────── TODO ────────────────┐");
    for t in &todos {
        let icon = match t.status { TodoStatus::Completed => "✓", TodoStatus::InProgress => "→", TodoStatus::Pending => "○" };
        println!("│ {icon} [{:>5}] {:<22} │", format!("{:?}", t.priority), t.content);
    }
    println!("│ {todo_done}/{} done                           │", todos.len());
    println!("└─────────────────────────────────────┘");

    println!("\n══════ Summary ══════");
    println!("  All Goal/Plan/Todo mutations were performed by the Agent calling tools.");
    println!("  The shared PlanningStore allows external verification of Agent state.");
    println!("  Auto-checkpoint fired after turn 4b (see goal.checkpoints).");
}
