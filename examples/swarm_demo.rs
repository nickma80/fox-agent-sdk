/// swarm_demo: parent-worker task execution via SwarmRuntime.
///
/// Flow:
///   1. Coordinator upserts a plan with 2 blocked tasks.
///   2. Two workers spawned, each picks next runnable task.
///   3. Workers "execute" and report completion.
///   4. Drains reports and verifies broadcast delivery.
///
/// Uses MockProvider - no real LLM credentials needed.
use fox_agent_sdk::{
    AgentEvent, DefaultModel, FoxAgentSdkConfig, Harness, MockProvider, Model,
    PlanItem, StreamEvent, SwarmCoordinator, SwarmMessageKind, SwarmRuntime,
    TurnOutcome, WorkerAgent, WorkerStatus,
};
use std::sync::Arc;

#[tokio::main]
async fn main() {
    println!("=== Phase 4 Swarm Demo ===\n");

    let coordinator = Arc::new(SwarmCoordinator::new());
    let provider = Arc::new(MockProvider::new("mock"));
    let model: Arc<dyn Model> = Arc::new(DefaultModel::new(provider.clone(), "mock-1"));
    let harness = Harness::new(FoxAgentSdkConfig::default(), None);
    harness.register_default_tools().await;

    let runtime = SwarmRuntime::new(
        coordinator.clone(), model.clone(), harness,
    );

    // coordinator: upsert plan
    runtime.upsert_plan(vec![
        PlanItem {
            id: "task-1".into(), content: "Analyze project structure".into(),
            status: "pending".into(), priority: "high".into(),
            assigned_to: None, blocked_by: vec![],
        },
        PlanItem {
            id: "task-2".into(), content: "Count source files".into(),
            status: "pending".into(), priority: "medium".into(),
            assigned_to: None, blocked_by: vec!["task-1".into()],
        },
    ]).await;
    println!("[coordinator] plan with 2 items upserted");

    // spawn workers
    runtime.spawn_worker("worker-alpha", "analysis").await;
    runtime.spawn_worker("worker-beta", "reporting").await;
    println!("[coordinator] 2 workers spawned");

    // worker-alpha runs task-1
    provider.push_script(vec![
        StreamEvent::TextDelta { text: "Top-level modules: src/lib.rs, src/cli.rs".into() },
        StreamEvent::MessageStop { stop_reason: None },
    ]);

    let mut w1 = WorkerAgent::new(runtime.fork_agent(), coordinator.clone(), "worker-alpha".into());
    let (tx, _rx) = tokio::sync::mpsc::channel::<AgentEvent>(32);
    let outcome = w1.try_assign_and_run(&tx).await.unwrap();
    match outcome {
        TurnOutcome::Completed { ref text } => println!("[worker-alpha] done: {}", text),
        _ => panic!("unexpected"),
    }
    w1.report_completion("task-1", "Found 2 top-level modules").await;
    assert_eq!(w1.worker_status().await, Some(WorkerStatus::Completed));

    // worker-beta runs task-2 (now unblocked)
    provider.push_script(vec![
        StreamEvent::TextDelta { text: "2 source files counted".into() },
        StreamEvent::MessageStop { stop_reason: None },
    ]);

    let mut w2 = WorkerAgent::new(runtime.fork_agent(), coordinator.clone(), "worker-beta".into());
    let (tx2, _rx2) = tokio::sync::mpsc::channel::<AgentEvent>(32);
    let outcome = w2.try_assign_and_run(&tx2).await.unwrap();
    match outcome {
        TurnOutcome::Completed { ref text } => println!("[worker-beta] done: {}", text),
        _ => panic!("unexpected"),
    }
    w2.report_completion("task-2", "2 files counted").await;

    // drain reports
    let reports = runtime.reports().await;
    println!("\n[coordinator] {} report(s):", reports.len());
    for r in &reports {
        println!("  - {} task={:?} status={:?} summary={}", r.worker_id, r.task_id, r.status, r.summary);
    }
    assert_eq!(reports.len(), 2);

    // verify broadcast
    coordinator.broadcast("coordinator", "all done").await;
    let inbox = coordinator.drain_inbox("worker-alpha").await;
    assert!(inbox.iter().any(|m| m.kind == SwarmMessageKind::Broadcast));

    println!("\n=== Phase 4 Swarm Demo PASSED ===");
}
