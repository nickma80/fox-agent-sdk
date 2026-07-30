/// swarm_workflow: demonstrates multi-agent swarm with supervisor,
/// building agents via `AgentBuilder` + `agent.toml`.
///
/// Covers:
/// - Loading `agent.toml` + `AGENTS.md` via `AgentBuilder`
/// - Building worker agents with the builder pattern
/// - Worker lifecycle (Ready → Running → Completed/Failed/TimedOut)
/// - Task assignment with dependency resolution
/// - Failure retry and task reassignment
/// - Summary report generation
///
/// Uses `SwarmCoordinator` + `SwarmSupervisor` with agents built
/// from the project config.
///
/// Uses MockProvider - no real LLM credentials needed.
use fox_agent_sdk::{
    AgentBuilder, AgentReport, FoxAgentSdkConfig, MockProvider, PlanItem, PlanPriority, PlanStatus,
    SwarmCoordinator, SwarmSupervisor, WorkerStatus,
};
use std::path::PathBuf;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    println!("=== Fox Agent SDK — Swarm Workflow Demo ===\n");

    // ── Load project config ──
    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cfg = FoxAgentSdkConfig::load_from_file(project_root.join("agent.toml"))
        .unwrap_or_else(|_| FoxAgentSdkConfig::default());
    println!("> Config loaded from agent.toml\n");

    // ── Build a supervisor agent via AgentBuilder ──
    let provider = Arc::new(MockProvider::new("mock-supervisor"));
    let _agent = AgentBuilder::new()
        .working_dir(&project_root)
        .sdk_config(cfg)
        .with_global_agents_md_path(project_root.join("AGENTS.md"))
        .with_provider(provider.clone())
        .model_id("mock-1")
        .with_default_tools()
        .build()
        .await
        .expect("build agent");

    println!("> Supervisor agent built\n");

    // ── Set up coordinator + supervisor ──
    let coordinator = Arc::new(SwarmCoordinator::new());
    let supervisor = SwarmSupervisor::with_defaults(coordinator.clone());

    // ── Create work plan with dependencies ──
    coordinator
        .upsert_plan(vec![
            PlanItem {
                id: "p1".into(),
                content: "Analyze project structure".into(),
                status: PlanStatus::Pending,
                priority: PlanPriority::High,
                assigned_to: None,
                blocked_by: vec![],
            },
            PlanItem {
                id: "p2".into(),
                content: "Review architecture".into(),
                status: PlanStatus::Pending,
                priority: PlanPriority::Medium,
                assigned_to: None,
                blocked_by: vec!["p1".into()],
            },
            PlanItem {
                id: "p3".into(),
                content: "Generate final report".into(),
                status: PlanStatus::Pending,
                priority: PlanPriority::Low,
                assigned_to: None,
                blocked_by: vec!["p1".into(), "p2".into()],
            },
        ])
        .await;

    println!(
        "[plan] {} items with dependency chain: p1 → p2 → p3\n",
        coordinator.shared_plan.read().await.items.len()
    );

    // ── Spawn 2 workers ──
    coordinator.spawn("w1", "analysis expert").await;
    coordinator.spawn("w2", "reporting expert").await;
    println!("[workers] w1, w2 spawned\n");

    // ── Worker 1 executes p1, completes it ──
    let task1 = coordinator.assign_next_runnable_task("w1").await.unwrap();
    println!("[w1] assigned: {}", task1.id);
    coordinator
        .report_completion("w1", &task1.id, "Analyzed project structure")
        .await
        .unwrap();
    println!("[w1] completed: {}", task1.id);

    // ── Worker 1 picks up p2, reports failure ──
    let task2 = coordinator.assign_next_runnable_task("w1").await.unwrap();
    println!("[w1] assigned: {} (will fail)", task2.id);
    coordinator.reports.write().await.push(AgentReport {
        worker_id: "w1".into(),
        task_id: Some(task2.id.clone()),
        status: WorkerStatus::Failed,
        summary: "Failed: p2".into(),
    });

    // ── Supervisor handles failure → retry ──
    let handled = supervisor.handle_failure("w1", &task2.id).await;
    println!(
        "[supervisor] failure handled: {} (task p2 retried)",
        handled
    );

    // ── Worker 2 picks up p2 (after retry reset) ──
    let task2b = coordinator.assign_next_runnable_task("w2").await.unwrap();
    println!("[w2] assigned: {}", task2b.id);
    coordinator
        .report_completion("w2", &task2b.id, "Reviewed architecture")
        .await
        .unwrap();
    println!("[w2] completed: {}", task2b.id);

    // ── Worker 2 also picks up p3 ──
    let task3 = coordinator.assign_next_runnable_task("w2").await.unwrap();
    println!("[w2] assigned: {}", task3.id);
    coordinator
        .report_completion("w2", &task3.id, "Final report generated")
        .await
        .unwrap();
    println!("[w2] completed: {}", task3.id);

    // ── Generate summary ──
    let summary = supervisor.generate_summary().await;
    println!("\n{}", summary.format());

    if summary.all_terminal() {
        println!("All workers reached terminal state.");
    }

    println!("\nDone.");
}
