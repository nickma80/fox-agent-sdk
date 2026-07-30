//! Individual tool execution benchmarks.
//!
//! ```bash
//! cargo bench --bench tool_bench
//! ```

mod harness;

use criterion::{Criterion, criterion_group, criterion_main};
use fox_agent_core::{Tool, ToolContext, ToolExecutionMode};
use fox_agent_sdk::BashTool;
use harness::EchoTool;
use std::sync::Arc;
use tokio::runtime::Runtime;

fn bench_tool_execute(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("tool_execution");
    group.sample_size(50);

    // ── Echo (fastest path) ──
    group.bench_function("echo", |b| {
        let tool = Arc::new(EchoTool);
        let ctx = empty_ctx();
        b.to_async(&rt).iter(|| async {
            let _ = tool
                .execute(serde_json::json!({"text":"hello"}), ctx.clone())
                .await
                .unwrap();
        });
    });

    // ── Bash simple command ──
    group.bench_function("bash_echo", |b| {
        let tool = Arc::new(BashTool::new());
        let ctx = empty_ctx();
        b.to_async(&rt).iter(|| async {
            let _ = tool
                .execute(
                    serde_json::json!({"command":"echo hello","timeout":5000}),
                    ctx.clone(),
                )
                .await
                .unwrap();
        });
    });

    group.finish();
}

fn empty_ctx() -> ToolContext {
    ToolContext {
        session_id: "bench".into(),
        message_id: "m1".into(),
        tool_call_id: "c1".into(),
        working_dir: None,
        execution_mode: ToolExecutionMode::Foreground,
        graceful_shutdown_requested: false,
        progress_tx: None,
    }
}

criterion_group!(
    name = tool_benches;
    config = Criterion::default().with_plots();
    targets = bench_tool_execute
);
criterion_main!(tool_benches);
