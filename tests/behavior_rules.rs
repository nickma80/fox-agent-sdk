//! Behavior rules integration tests.
//!
//! Each test constructs AgentEvent sequences and checks them through
//! BehaviorRuleEngine, asserting that the correct rules are triggered.

use fox_agent_core::AgentEvent;
use fox_agent_sdk::eval::behavior_rules::{BehaviorRuleEngine, RuleSeverity};

/// Empty event stream should not trigger any rules.
#[test]
fn empty_events_no_violations() {
    let engine = BehaviorRuleEngine::with_default_rules();
    let events: Vec<AgentEvent> = vec![];
    let violations = engine.check(&events);
    assert!(violations.is_empty(), "empty events should have no violations");
}

/// Normal task completion should not trigger Error-level violations.
#[test]
fn normal_tool_use_no_errors() {
    let events = vec![
        AgentEvent::TurnStart { turn_id: 1 },
        AgentEvent::ModelMessageStart { message_id: "msg1".into() },
        AgentEvent::ToolCallStart {
            call_id: "t1".into(),
            name: "write".into(),
            input: serde_json::json!({"file_path": "/tmp/test.txt", "content": "hello"}),
        },
        AgentEvent::ToolCallEnd {
            call_id: "t1".into(),
            output: fox_agent_core::ToolOutput { text: "ok".into(), is_error: false, json: None },
        },
        AgentEvent::ModelTextDelta { text: "Done!".into() },
        AgentEvent::ModelMessageEnd { message_id: "msg1".into() },
        AgentEvent::TurnEnd {
            turn_id: 1,
            outcome: fox_agent_core::TurnOutcome::Completed { text: "Done!".into() },
        },
    ];

    let engine = BehaviorRuleEngine::with_default_rules();
    let errors = engine.check_errors(&events);
    assert!(errors.is_empty(), "normal tool use should have no errors, got: {errors:?}");
}

/// 11 consecutive tool calls in one turn should trigger no_repeat_tool_storm.
#[test]
fn excessive_tool_calls_triggers_violation() {
    let mut events = vec![AgentEvent::TurnStart { turn_id: 1 }];
    for i in 0..11 {
        events.push(AgentEvent::ToolCallStart {
            call_id: format!("t{i}"),
            name: "read".into(),
            input: serde_json::json!({"file_path": format!("/tmp/file{i}.txt")}),
        });
        events.push(AgentEvent::ToolCallEnd {
            call_id: format!("t{i}"),
            output: fox_agent_core::ToolOutput { text: "content".into(), is_error: false, json: None },
        });
    }
    events.push(AgentEvent::TurnEnd {
        turn_id: 1,
        outcome: fox_agent_core::TurnOutcome::Completed { text: String::new() },
    });

    let engine = BehaviorRuleEngine::with_default_rules();
    let violations = engine.check(&events);
    let storm: Vec<_> = violations.iter().filter(|v| v.rule_name == "no_repeat_tool_storm").collect();
    assert!(!storm.is_empty(), "should detect tool storm, got: {violations:?}");
    assert_eq!(storm[0].severity, RuleSeverity::Error);
}

/// Retry same tool after deny should trigger no_retry_after_deny.
#[test]
fn deny_then_retry_triggers_warning() {
    let events = vec![
        AgentEvent::TurnStart { turn_id: 1 },
        AgentEvent::PermissionRequest {
            request_id: "req1".into(),
            tool_name: "bash".into(),
            prompt: "rm -rf /".into(),
            risk_level: "high".into(),
            policy_source: "default".into(),
            tool_summary: "destructive".into(),
        },
        AgentEvent::ToolCallStart {
            call_id: "t1".into(), name: "bash".into(),
            input: serde_json::json!({"command": "rm -rf /"}),
        },
        AgentEvent::ToolCallEnd {
            call_id: "t1".into(),
            output: fox_agent_core::ToolOutput { text: "Permission denied".into(), is_error: true, json: None },
        },
        AgentEvent::ToolCallStart {
            call_id: "t2".into(), name: "bash".into(),
            input: serde_json::json!({"command": "rm -rf /"}),
        },
        AgentEvent::ToolCallEnd {
            call_id: "t2".into(),
            output: fox_agent_core::ToolOutput { text: "Permission denied".into(), is_error: true, json: None },
        },
        AgentEvent::TurnEnd {
            turn_id: 1,
            outcome: fox_agent_core::TurnOutcome::Completed { text: String::new() },
        },
    ];

    let engine = BehaviorRuleEngine::with_default_rules();
    let violations = engine.check(&events);
    let retry: Vec<_> = violations
        .iter()
        .filter(|v| v.rule_name == "no_retry_after_deny")
        .collect();
    assert!(
        !retry.is_empty(),
        "should detect deny-then-retry, got violations: {violations:?}"
    );
    assert_eq!(retry[0].severity, RuleSeverity::Warning);
}

/// Empty turn (no tool calls, no text output) should trigger no_empty_turn.
#[test]
fn empty_turn_triggers_warning() {
    let events = vec![
        AgentEvent::TurnStart { turn_id: 1 },
        AgentEvent::ModelMessageStart { message_id: "msg1".into() },
        AgentEvent::ModelMessageEnd { message_id: "msg1".into() },
        AgentEvent::TurnEnd {
            turn_id: 1,
            outcome: fox_agent_core::TurnOutcome::Completed { text: String::new() },
        },
    ];

    let engine = BehaviorRuleEngine::with_default_rules();
    let violations = engine.check(&events);
    let empty: Vec<_> = violations.iter().filter(|v| v.rule_name == "no_empty_turn").collect();
    assert!(!empty.is_empty(), "should detect empty turn, got: {violations:?}");
}

/// Orphaned ToolCallStart without ToolCallEnd triggers tool_output_not_orphaned.
#[test]
fn orphaned_tool_call_triggers_error() {
    let events = vec![
        AgentEvent::TurnStart { turn_id: 1 },
        AgentEvent::ToolCallStart {
            call_id: "t1".into(),
            name: "read".into(),
            input: serde_json::json!({"file_path": "/tmp/test.txt"}),
        },
        AgentEvent::ModelTextDelta { text: "done".into() },
        AgentEvent::TurnEnd {
            turn_id: 1,
            outcome: fox_agent_core::TurnOutcome::Completed { text: String::new() },
        },
    ];

    let engine = BehaviorRuleEngine::with_default_rules();
    let violations = engine.check(&events);
    let orphan: Vec<_> = violations.iter().filter(|v| v.rule_name == "tool_output_not_orphaned").collect();
    assert!(!orphan.is_empty(), "should detect orphaned tool call, got: {violations:?}");
}
