/// governance — demonstrates runtime budget enforcement and real-time
/// metrics collection with GovernanceGuard.
///
/// Covers:
/// - `BudgetConfig` (token budget, cost cap, max turns, tool timeout)
/// - `turn_begin()` / `turn_end()` lifecycle hooks
/// - `record_usage()` with budget enforcement
/// - `add_metrics_hook()` for real-time observability callbacks
///
/// Uses MockProvider — no real LLM credentials needed.
use fox_agent_sdk::{
    AgentBuilder, AgentEvent, BudgetConfig, FoxAgentSdkConfig, GovernanceGuard,
    MockProvider, StreamEvent, TurnOutcome,
};
use std::path::PathBuf;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    println!("=== Governance Demo ===\n");

    // ── 1. Set up governance guard with budget constraints ──
    let guard = Arc::new(GovernanceGuard::new(BudgetConfig {
        token_budget: Some(10_000),     // Budget上限：10k tokens
        cost_budget_cents: Some(100),   // 费用上限：$1.00
        max_turns: 5,                   // 最大轮次
        tool_timeout_secs: 30,          // 工具超时
        tool_concurrency_limit: 4,      // 工具并发数
        ..Default::default()
    }));

    // Register a metrics hook for real-time observability
    guard
        .add_metrics_hook(|m| {
            println!(
                "  [metrics] tokens: {} in / {} out | cost: {}c | errors: {:.0}%",
                m.total_input_tokens,
                m.total_output_tokens,
                m.estimated_cost_cents,
                m.tool_error_rate() * 100.0,
            );
        })
        .await;

    // ── 2. Build agent with MockProvider ──
    let provider = Arc::new(MockProvider::new("mock"));

    provider.push_script(vec![
        StreamEvent::TextDelta {
            text: "我准备好了，请随时提问。".into(),
        },
        StreamEvent::MessageStop { stop_reason: None },
    ]);
    provider.push_script(vec![
        StreamEvent::TextDelta {
            text: "这是一个关于治理的简短回答。".into(),
        },
        StreamEvent::MessageStop { stop_reason: None },
    ]);

    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cfg = FoxAgentSdkConfig::load_from_file(project_root.join("agent.toml"))
        .unwrap_or_else(|_| FoxAgentSdkConfig::default());

    let mut agent = AgentBuilder::new()
        .working_dir(&project_root)
        .sdk_config(cfg)
        .with_global_agents_md_path(project_root.join("AGENTS.md"))
        .with_provider(provider.clone())
        .model_id("mock-1")
        .build()
        .await
        .expect("build agent");

    // ── 3. Run turns with governance lifecycle ──
    // Turn 1
    guard.turn_begin().await;
    let (tx, rx) = tokio::sync::mpsc::channel::<AgentEvent>(16);
    let _outcome = agent.run_once_streaming("打个招呼", &tx).await.unwrap();

    // Simulate recording model usage (normally done inside Agent loop)
    guard
        .record_usage(
            &fox_agent_sdk::TokenUsage {
            input_tokens: 150,
            output_tokens: 40,
            total_tokens: 190,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
        },
        520, // provider latency ms
        2,   // cost cents
    )
    .await
    .ok();

    // Drain events for Turn 1
    {
        let mut rx = rx;
        while rx.try_recv().is_ok() {}
    }

    if let Err(e) = guard.turn_end().await {
        println!("  [budget exceeded] {e}");
    }

    // Turn 2
    guard.turn_begin().await;
    let (tx2, rx2) = tokio::sync::mpsc::channel::<AgentEvent>(16);
    let outcome2 = agent
        .run_once_streaming("简单回答一下什么是治理", &tx2)
        .await
        .unwrap();

    guard
        .record_usage(
            &fox_agent_sdk::TokenUsage {
            input_tokens: 120,
            output_tokens: 35,
            total_tokens: 155,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
        },
            480,
            1,
        )
        .await
        .ok();

    {
        let mut rx = rx2;
        while rx.try_recv().is_ok() {}
    }

    if let Err(e) = guard.turn_end().await {
        println!("  [budget exceeded] {e}");
    }

    match outcome2 {
        TurnOutcome::Completed { text } => {
            println!("[agent] {text}");
        }
        _ => {}
    }

    // ── 4. Inspect final metrics snapshot ──
    let snapshot = guard.snapshot().await;
    println!("\n=== Final Metrics ===");
    println!("  Total tokens:  {} (in: {}, out: {})",
        snapshot.total_tokens,
        snapshot.total_input_tokens,
        snapshot.total_output_tokens,
    );
    println!("  Estimated cost: {} cents", snapshot.estimated_cost_cents);
    println!("  Tool calls:     {}", snapshot.tool_calls);
    println!("  Errors:         {} (rate: {:.1}%)",
        snapshot.tool_error_count,
        snapshot.tool_error_rate() * 100.0,
    );

    println!("\n=== PASSED ===");
}
