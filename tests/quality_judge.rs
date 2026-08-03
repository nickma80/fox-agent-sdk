//! LLM-as-Judge 质量评估测试：使用自研 TaskJudge 对 Agent 输出进行主观评分。
//!
//! 此测试需要 `quality_judge` feature + 真实 LLM provider，默认 `#[ignore]`。
//!
//! ```bash
//! cargo test --test quality_judge --features quality_judge -- --ignored
//! ```

use fox_agent_core::AgentEvent;
use fox_agent_sdk::eval::{EvalReport, JudgeScores};

// ═══════════════════════════════════════════════════════════════════════════════
// Unit tests (不需要真实 LLM)
// ═══════════════════════════════════════════════════════════════════════════════

/// EvalReport 从事件流和断言结果正确构造。
#[test]
fn eval_report_construction() {
    let events = vec![
        AgentEvent::ToolCallStart {
            call_id: "t1".into(),
            name: "write".into(),
            input: serde_json::json!({"file_path": "/tmp/test.txt", "content": "hello"}),
        },
        AgentEvent::ToolCallEnd {
            call_id: "t1".into(),
            output: fox_agent_core::ToolOutput {
                text: "ok".into(),
                is_error: false,
                json: None,
            },
        },
    ];

    let report = EvalReport::from_events(
        "test-task-1",
        "Write a test file",
        "I have created the file.",
        &events,
        true,
    );

    assert_eq!(report.task_id, "test-task-1");
    assert!(report.assertions_passed);
    assert!(!report.tool_summary.is_empty());
    assert!(report.scores.is_none());
    assert!(report.agent_response.contains("created"));
}

/// JudgeScores 加权平均计算验证。
#[test]
fn judge_scores_weighted_average() {
    let scores = JudgeScores {
        completeness: 5,
        solution_quality: 4,
        error_recovery: None,
        redundancy: 5,
    };
    // error_recovery None → defaults to 5 in weighted_average
    // 5*0.4 + 4*0.3 + 5*0.15 + 5*0.15 = 2.0 + 1.2 + 0.75 + 0.75 = 4.70
    let avg = scores.weighted_average();
    assert!(
        (avg - 4.70).abs() < 0.01,
        "weighted average should be ~4.70, got {avg}"
    );

    // total: error_recovery None → unwrap_or(5) in total()
    // 5 + 4 + 5 + 5 = 19
    let total = scores.total();
    assert_eq!(total, 19);
}

/// with_scores 链式调用正确赋值。
#[test]
fn eval_report_with_scores() {
    let report = EvalReport::from_events("t1", "prompt", "response", &[], true);
    let scores = JudgeScores {
        completeness: 3,
        solution_quality: 3,
        error_recovery: Some(3),
        redundancy: 3,
    };
    let report = report.with_scores(scores);
    assert!(report.scores.is_some());
    assert_eq!(report.scores.as_ref().unwrap().total(), 12);
}

/// 验证 EvalReport 序列化。
#[test]
fn eval_report_serialization() {
    let report = EvalReport::from_events("t1", "prompt", "response", &[], true);
    let json = serde_json::to_string(&report).unwrap();
    assert!(json.contains("t1"));
    assert!(json.contains("assertions_passed"));

    let report = report.with_scores(JudgeScores {
        completeness: 4,
        solution_quality: 4,
        error_recovery: Some(4),
        redundancy: 4,
    });
    let json = serde_json::to_string(&report).unwrap();
    assert!(json.contains("completeness"));
    assert!(json.contains("error_recovery"));
}

// ═══════════════════════════════════════════════════════════════════════════════
// Integration test (需要真实 LLM — 默认 ignored)
// ═══════════════════════════════════════════════════════════════════════════════

/// TaskJudge 端到端评估（需要真实 LLM provider）。
#[tokio::test]
#[cfg(feature = "quality_judge")]
#[ignore = "requires real LLM provider"]
async fn task_judge_e2e() -> anyhow::Result<()> {
    use fox_agent_core::StreamEvent;
    use fox_agent_sdk::eval::TaskJudge;
    use fox_agent_sdk::MockProvider;
    use std::sync::Arc;

    let judge_provider = MockProvider::new("judge");

    judge_provider.push_script(vec![
        StreamEvent::TextDelta {
            text: "{\n  \"completeness\": 4,\n  \"solution_quality\": 5,\n  \"redundancy\": 4\n}".into(),
        },
        StreamEvent::MessageStop {
            stop_reason: Some("end_turn".into()),
        },
    ]);

    let judge = TaskJudge::new(Arc::new(judge_provider), "mock-judge");

    let report = EvalReport::from_events(
        "quality-test-1",
        "Implement a thread-safe counter",
        "Here's my implementation using Arc<Mutex<i32>>...",
        &[],
        true,
    );

    let scores = judge.evaluate(&report).await?;
    assert_eq!(scores.completeness, 4);
    assert_eq!(scores.solution_quality, 5);

    let avg = scores.weighted_average();
    assert!(
        avg >= 4.0,
        "quality score should be at least 4.0, got {avg}"
    );

    let report = report.with_scores(scores);
    let json = serde_json::to_string_pretty(&report)?;
    println!("Judge report:\n{json}");
    Ok(())
}
