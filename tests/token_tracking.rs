//! Token 效率追踪：从 Agent 事件流中提取 Token 消耗和压缩指标。
//!
//! 利用 SDK 内置的 `TokenReport` 聚合 usage 事件，生成 token 效率统计。

use fox_agent_core::{AgentEvent, TokenReport, TokenUsage};

// ═══════════════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════════════

/// 从事件流收集 TokenReport。
fn collect_token_report(events: &[AgentEvent]) -> TokenReport {
    let mut report = TokenReport::default();
    for ev in events {
        match ev {
            AgentEvent::ModelUsage { usage } => report.record_usage(usage),
            AgentEvent::ToolCallStart { .. } => report.record_tool_call(),
            AgentEvent::Compaction { .. } => report.record_compaction(),
            _ => {}
        }
    }
    report
}

// ═══════════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════════

/// 空事件流 → 空 TokenReport。
#[test]
fn empty_events_zero_report() {
    let events: Vec<AgentEvent> = vec![];
    let report = collect_token_report(&events);
    assert_eq!(report.total_input, 0);
    assert_eq!(report.total_output, 0);
    assert_eq!(report.total_tokens(), 0);
    assert_eq!(report.cache_hit_ratio(), 0.0);
}

/// 单次 usage → 正确累加。
#[test]
fn single_usage_accumulates() {
    let events = vec![AgentEvent::ModelUsage {
        usage: TokenUsage {
            input_tokens: 100,
            output_tokens: 50,
            total_tokens: 150,
            cache_read_input_tokens: Some(20),
            cache_creation_input_tokens: None,
        },
    }];
    let report = collect_token_report(&events);
    assert_eq!(report.total_input, 100);
    assert_eq!(report.total_output, 50);
    assert_eq!(report.total_tokens(), 150);
    assert_eq!(report.api_calls, 1);
}

/// 多次 usage + 工具调用 → 正确聚合。
#[test]
fn multi_turn_token_accumulation() {
    let events = vec![
        AgentEvent::TurnStart { turn_id: 1 },
        AgentEvent::ToolCallStart {
            call_id: "t1".into(),
            name: "read".into(),
            input: serde_json::json!({"file_path": "/tmp/test.txt"}),
        },
        AgentEvent::ToolCallEnd {
            call_id: "t1".into(),
            output: fox_agent_core::ToolOutput {
                text: "ok".into(),
                is_error: false,
                json: None,
            },
        },
        AgentEvent::ModelTextDelta {
            text: "I read the file.".into(),
        },
        AgentEvent::ModelUsage {
            usage: TokenUsage {
                input_tokens: 200,
                output_tokens: 80,
                total_tokens: 280,
                cache_read_input_tokens: Some(50),
                cache_creation_input_tokens: Some(30),
            },
        },
    ];

    let report = collect_token_report(&events);

    assert_eq!(report.total_input, 200);
    assert_eq!(report.total_output, 80);
    assert_eq!(report.tool_calls, 1);
    assert_eq!(report.api_calls, 1);
    assert_eq!(report.cache_read, 50);
    assert_eq!(report.cache_write, 30);

    let hit_ratio = report.cache_hit_ratio();
    assert!(hit_ratio > 0.0);

    println!(
        "Token Report: input={} output={} tools={} cache_hit={:.2}%",
        report.total_input,
        report.total_output,
        report.tool_calls,
        hit_ratio * 100.0,
    );
}

/// 压实场景：多轮工具调用后 Token 累积。
#[test]
fn compaction_tracking() {
    let mut events = vec![AgentEvent::TurnStart { turn_id: 1 }];
    for i in 0..5 {
        events.push(AgentEvent::ToolCallStart {
            call_id: format!("t{i}"),
            name: "read".into(),
            input: serde_json::json!({"file_path": format!("/tmp/f{i}.txt")}),
        });
        events.push(AgentEvent::ToolCallEnd {
            call_id: format!("t{i}"),
            output: fox_agent_core::ToolOutput {
                text: "content".into(),
                is_error: false,
                json: None,
            },
        });
    }
    events.push(AgentEvent::ModelUsage {
        usage: TokenUsage {
            input_tokens: 1500,
            output_tokens: 100,
            total_tokens: 1600,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
        },
    });
    events.push(AgentEvent::TurnEnd {
        turn_id: 1,
        outcome: fox_agent_core::TurnOutcome::Completed {
            text: String::new(),
        },
    });

    let report = collect_token_report(&events);

    assert_eq!(report.tool_calls, 5);
    assert_eq!(report.total_input, 1500);
    assert_eq!(report.total_output, 100);
    assert_eq!(report.api_calls, 1);
    println!(
        "Compaction test: tokens={} tool_calls={}",
        report.total_tokens(),
        report.tool_calls,
    );
}

/// TokenReport 合并：两个报告累加。
#[test]
fn merge_two_reports() {
    let mut r1 = TokenReport {
        total_input: 100,
        total_output: 50,
        cache_read: 20,
        cache_write: 10,
        tool_calls: 2,
        compactions: 1,
        api_calls: 1,
    };
    let r2 = TokenReport {
        total_input: 200,
        total_output: 100,
        cache_read: 40,
        cache_write: 20,
        tool_calls: 3,
        compactions: 0,
        api_calls: 2,
    };

    r1.merge(&r2);
    assert_eq!(r1.total_input, 300);
    assert_eq!(r1.total_output, 150);
    assert_eq!(r1.tool_calls, 5);
    assert_eq!(r1.api_calls, 3);
    assert_eq!(r1.compactions, 1);
    assert_eq!(r1.cache_read, 60);
    assert_eq!(r1.cache_write, 30);
}

/// 零除保护：缓存命中率为 0 时不应 panic。
#[test]
fn zero_cache_ratio() {
    let report = TokenReport {
        total_input: 0,
        total_output: 0,
        cache_read: 0,
        cache_write: 0,
        tool_calls: 0,
        compactions: 0,
        api_calls: 0,
    };
    assert_eq!(report.cache_hit_ratio(), 0.0);
}
