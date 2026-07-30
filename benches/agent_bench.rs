//! Agent-level benchmarks: cold/hot start latency, framework overhead, turn throughput.
//!
//! ```bash
//! # Run all
//! cargo bench --bench agent_bench
//!
//! # Enable Chrome trace output
//! BENCH_TRACE_DIR=./target/criterion cargo bench --bench agent_bench
//! ```

mod harness;

use criterion::{Criterion, criterion_group, criterion_main};
use harness::{EchoTool, build_mock_agent, drain_events, push_tool_then_text, text_done_script};
use std::sync::Arc;
use tokio::runtime::Runtime;

fn bench_run_once_streaming_cold(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("agent_cold_start");
    group.sample_size(30);

    group.bench_function("run_once_streaming_text_only", |b| {
        b.to_async(&rt).iter_with_large_drop(|| async {
            let (agent, provider) = build_mock_agent(vec![]).await;
            provider.push_script(harness::text_done_script("hello"));
            let (tx, mut rx) = tokio::sync::mpsc::channel(32);
            let _outcome = agent.run_once_streaming("hi", &tx).await.unwrap();
            drop(tx);
            let _events = drain_events(&mut rx).await;
        });
    });

    group.finish();
}

fn bench_run_once_streaming_with_tools(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("agent_with_tools");
    group.sample_size(20);

    group.bench_function("run_once_streaming_one_tool", |b| {
        b.to_async(&rt).iter_with_large_drop(|| async {
            let (agent, provider) = build_mock_agent(vec![Arc::new(EchoTool)]).await;
            push_tool_then_text(
                &provider,
                "c1",
                "echo",
                serde_json::json!({"text":"hi"}),
                "done",
            );
            let (tx, mut rx) = tokio::sync::mpsc::channel(64);
            let _outcome = agent.run_once_streaming("go", &tx).await.unwrap();
            drop(tx);
            let _events = drain_events(&mut rx).await;
        });
    });

    group.finish();
}

fn bench_resume_streaming(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("agent_resume");
    group.sample_size(20);

    group.bench_function("resume_after_permission", |b| {
        b.to_async(&rt).iter_with_large_drop(|| async {
            let (agent, provider) = build_mock_agent(vec![Arc::new(EchoTool)]).await;
            push_tool_then_text(
                &provider,
                "c1",
                "echo",
                serde_json::json!({"text":"bench"}),
                "resumed",
            );
            let (tx, mut rx) = tokio::sync::mpsc::channel(64);
            let _outcome = agent.run_once_streaming("go", &tx).await.unwrap();
            drop(tx);
            let _events = drain_events(&mut rx).await;
        });
    });

    group.finish();
}

criterion_group!(
    name = agent_benches;
    config = Criterion::default().with_plots();
    targets = bench_run_once_streaming_cold, bench_run_once_streaming_with_tools, bench_resume_streaming
);
criterion_main!(agent_benches);
