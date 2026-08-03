//! 自定义任务评估：使用 MockProvider 模拟完整的 Agent 工作流，
//! 通过物证断言（文件存在、内容匹配、编译通过）验证任务完成质量。
//!
//! 注意：在 MockProvider 模式下，Agent Loop 会处理 ToolCallStart/ToolCallEnd 事件，
//! 但工具本身不会实际执行。物证断言验证的是 Agent 的事件流程正确性。

use std::path::PathBuf;
use std::sync::Arc;

use fox_agent_core::{
    AgentEvent, DefaultSafetyPolicy, FoxAgentSdkConfig, SafetyConfig, StreamEvent, TokenUsage,
};
use fox_agent_sdk::{Agent, Harness, MockProvider};

// ═══════════════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════════════

/// 创建 Agent 并使用预录脚本运行，收集事件流。
async fn run_agent_with_script(
    name: &str,
    script: Vec<StreamEvent>,
    working_dir: Option<PathBuf>,
) -> (Vec<AgentEvent>, PathBuf) {
    let provider = MockProvider::new(name);
    provider.push_script(script);

    let wd = working_dir.unwrap_or_else(|| {
        let dir = std::env::temp_dir().join(format!("fox-eval-{name}"));
        let _ = std::fs::create_dir_all(&dir);
        dir
    });

    let harness = Harness::new(
        FoxAgentSdkConfig {
            safety: SafetyConfig {
                default_policy: DefaultSafetyPolicy::Allow,
                productive_tool_confirm: false,
                ..Default::default()
            },
            ..Default::default()
        },
        Some(wd.clone()),
    );

    let model = Arc::new(fox_agent_core::DefaultModel::new(
        Arc::new(provider),
        "mock-model",
    ));
    let agent = Agent::new(model, harness, Arc::new(tokio::sync::RwLock::new(None)));
    let (tx, mut rx) = tokio::sync::mpsc::channel(128);
    let _ = agent.run_once_streaming("task", &tx).await;
    drop(tx);

    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        events.push(ev);
    }
    (events, wd)
}

// ═══════════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════════

/// 验证：Agent 事件流包含正确的 ToolCallStart/ToolCallEnd 序列。
#[tokio::test]
async fn create_file_event_flow() {
    let script = vec![
        StreamEvent::ToolUse {
            id: "t1".into(),
            name: "write".into(),
            input: serde_json::json!({
                "file_path": "/tmp/hello.txt",
                "content": "Hello World"
            }),
        },
        StreamEvent::TextDelta {
            text: "Created file.".into(),
        },
        StreamEvent::MessageStop {
            stop_reason: Some("end_turn".into()),
        },
        StreamEvent::Usage {
            usage: TokenUsage {
                input_tokens: 50,
                output_tokens: 10,
                total_tokens: 60,
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
            },
        },
    ];

    let (events, _wd) = run_agent_with_script("create_file", script, None).await;

    // 验证事件流包含正确的工具调用序列
    let tool_starts: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::ToolCallStart { .. }))
        .collect();
    assert!(
        !tool_starts.is_empty(),
        "should have tool call start events"
    );

    let tool_ends: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::ToolCallEnd { .. }))
        .collect();
    assert!(!tool_ends.is_empty(), "should have tool call end events");

    // Phase 2 物证断言：需要真实 LLM 或挂载真实工具后端
    // assert!(file_path.exists(), "file should have been created");
}

/// 验证：Agent 事件流包含完整的工具调用序列（创建项目剧本）。
#[tokio::test]
async fn multi_step_project_event_flow() {
    let script = vec![
        // mkdir
        StreamEvent::ToolUse {
            id: "t1".into(),
            name: "bash".into(),
            input: serde_json::json!({"command": "mkdir -p /tmp/demo-project/src"}),
        },
        // write Cargo.toml
        StreamEvent::ToolUse {
            id: "t2".into(),
            name: "write".into(),
            input: serde_json::json!({
                "file_path": "/tmp/demo-project/Cargo.toml",
                "content": "[package]\nname = \"demo-project\"\nversion = \"0.1.0\"\nedition = \"2024\"\n"
            }),
        },
        // write src/main.rs
        StreamEvent::ToolUse {
            id: "t3".into(),
            name: "write".into(),
            input: serde_json::json!({
                "file_path": "/tmp/demo-project/src/main.rs",
                "content": "fn main() { println!(\"Hello\"); }"
            }),
        },
        // cargo build
        StreamEvent::ToolUse {
            id: "t4".into(),
            name: "bash".into(),
            input: serde_json::json!({"command": "cd /tmp/demo-project && cargo build"}),
        },
        StreamEvent::TextDelta {
            text: "Done.".into(),
        },
        StreamEvent::MessageStop {
            stop_reason: Some("end_turn".into()),
        },
    ];

    let (events, _wd) = run_agent_with_script("rust_project", script, None).await;

    let tool_starts: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::ToolCallStart { .. }))
        .collect();
    assert_eq!(tool_starts.len(), 4, "should have 4 tool calls");

    let tool_ends: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::ToolCallEnd { .. }))
        .collect();
    assert_eq!(tool_ends.len(), 4, "should have 4 tool completions");
}

/// 验证：错误诊断与修复流程的行为正确性。
#[tokio::test]
async fn error_diagnosis_and_fix() {
    let wd = std::env::temp_dir().join("fox-eval-error-fix");
    let _ = std::fs::create_dir_all(&wd);

    let script = vec![
        StreamEvent::ToolUse {
            id: "t1".into(),
            name: "bash".into(),
            input: serde_json::json!({"command": "cargo build"}),
        },
        StreamEvent::ToolUse {
            id: "t2".into(),
            name: "write".into(),
            input: serde_json::json!({
                "file_path": "src/main.rs",
                "content": "fn main() { let x = 42; println!(\"{x}\"); }"
            }),
        },
        StreamEvent::ToolUse {
            id: "t3".into(),
            name: "bash".into(),
            input: serde_json::json!({"command": "cargo build"}),
        },
        StreamEvent::TextDelta {
            text: "Fixed and rebuilt.".into(),
        },
        StreamEvent::MessageStop {
            stop_reason: Some("end_turn".into()),
        },
    ];

    let (events, _wd) = run_agent_with_script("error_fix", script, Some(wd.clone())).await;

    // 验证事件流：应有 3 次工具调用
    let tool_use_count = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::ToolCallStart { .. }))
        .count();
    assert_eq!(tool_use_count, 3, "should have 3 tool calls");

    let _ = std::fs::remove_dir_all(&wd);
}
