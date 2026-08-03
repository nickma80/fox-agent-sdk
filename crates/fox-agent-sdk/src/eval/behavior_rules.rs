//! Behavior correctness rules: programmatic assertions on agent event streams.
//!
//! These rules check invariants that should hold regardless of the LLM model used.
//! Unlike GoldenTranscript (which is model-specific), behavior rules are universal.

use fox_agent_core::AgentEvent;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Severity of a rule violation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum RuleSeverity {
    /// Failing this rule blocks CI.
    Error,
    /// Failing this rule emits a warning but does not block.
    Warning,
}

/// A single violation detected by a behavior rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleViolation {
    /// Name of the rule that was violated.
    pub rule_name: String,
    /// Human-readable description of the violation.
    pub message: String,
    /// Severity level.
    pub severity: RuleSeverity,
}

type RuleFn = Box<dyn Fn(&[AgentEvent]) -> Vec<RuleViolation> + Send + Sync>;

/// Registry of behavior rules that check agent event streams.
#[derive(Default)]
pub struct BehaviorRuleEngine {
    rules: Vec<RuleFn>,
}

impl BehaviorRuleEngine {
    /// Create an engine with the default rule set.
    pub fn with_default_rules() -> Self {
        let mut engine = Self::default();
        engine.add_rule("no_repeat_tool_storm", check_repeat_tool_storm);
        engine.add_rule("no_retry_after_deny", check_retry_after_deny);
        engine.add_rule("compaction_triggered", check_compaction_triggered);
        engine.add_rule("no_empty_turn", check_no_empty_turn);
        engine.add_rule("subagent_has_readback", check_subagent_readback);
        engine.add_rule("no_error_storm", check_error_storm);
        engine.add_rule("tool_output_not_orphaned", check_tool_output_not_orphaned);
        engine
    }

    /// Register a custom rule.
    pub fn add_rule<F>(&mut self, _name: &str, rule: F)
    where
        F: Fn(&[AgentEvent]) -> Vec<RuleViolation> + Send + Sync + 'static,
    {
        self.rules.push(Box::new(rule));
    }

    /// Run all rules on an event stream.
    pub fn check(&self, events: &[AgentEvent]) -> Vec<RuleViolation> {
        let mut all_violations = Vec::new();
        for rule in &self.rules {
            all_violations.extend(rule(events));
        }
        all_violations
    }

    /// Run all rules, return only Error-severity violations.
    pub fn check_errors(&self, events: &[AgentEvent]) -> Vec<RuleViolation> {
        self.check(events)
            .into_iter()
            .filter(|v| v.severity == RuleSeverity::Error)
            .collect()
    }
}

// ── Rule implementations ──

/// Check that no tool is called more than `MAX_REPEAT` times in a single turn.
const MAX_REPEAT_TOOL_CALLS: usize = 10;

fn check_repeat_tool_storm(events: &[AgentEvent]) -> Vec<RuleViolation> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for ev in events {
        if let AgentEvent::ToolCallStart { name, .. } = ev {
            *counts.entry(name.clone()).or_insert(0) += 1;
        }
    }
    counts
        .into_iter()
        .filter(|(_, c)| *c > MAX_REPEAT_TOOL_CALLS)
        .map(|(name, count)| RuleViolation {
            rule_name: "no_repeat_tool_storm".into(),
            message: format!(
                "Tool '{}' called {} times (limit: {})",
                name, count, MAX_REPEAT_TOOL_CALLS
            ),
            severity: RuleSeverity::Error,
        })
        .collect()
}

/// Check that a denied permission does not result in immediate retry of the same tool.
fn check_retry_after_deny(events: &[AgentEvent]) -> Vec<RuleViolation> {
    let mut violations = Vec::new();
    let mut last_denied_tool: Option<String> = None;
    let mut tool_names: HashMap<String, String> = HashMap::new();

    for ev in events {
        match ev {
            AgentEvent::ToolCallStart {
                call_id, name, ..
            } => {
                if let Some(ref denied) = last_denied_tool
                    && denied == name
                {
                    violations.push(RuleViolation {
                        rule_name: "no_retry_after_deny".into(),
                        message: format!(
                            "Tool '{}' retried immediately after being denied",
                            name
                        ),
                        severity: RuleSeverity::Warning,
                    });
                }
                last_denied_tool = None;
                tool_names.insert(call_id.clone(), name.clone());
            }
            AgentEvent::ToolCallEnd {
                call_id, output, ..
            } if output.is_error => {
                let t = &output.text;
                if t.contains("denied") || t.contains("blocked") || t.contains("Deny") {
                    if let Some(name) = tool_names.get(call_id) {
                        last_denied_tool = Some(name.clone());
                    }
                }
            }
            _ => {}
        }
    }
    violations
}

/// Threshold: more than this many tool calls without compaction triggers a warning.
const MESSAGES_THRESHOLD_FOR_COMPACTION: usize = 50;

/// Context-pressure keywords that indicate the model hit a limit.
const CONTEXT_LIMIT_KEYWORDS: &[&str] = &["context_length_exceeded", "token limit reached"];

/// Check that long conversations trigger compaction.
fn check_compaction_triggered(events: &[AgentEvent]) -> Vec<RuleViolation> {
    let mut violations = Vec::new();

    let has_compaction = events
        .iter()
        .any(|ev| matches!(ev, AgentEvent::Compaction { .. }));

    let tool_count = events
        .iter()
        .filter(|ev| matches!(ev, AgentEvent::ToolCallStart { .. }))
        .count();

    if tool_count > MESSAGES_THRESHOLD_FOR_COMPACTION && !has_compaction {
        violations.push(RuleViolation {
            rule_name: "compaction_triggered".into(),
            message: format!(
                "{} tool calls observed but no compaction event detected (threshold: {})",
                tool_count, MESSAGES_THRESHOLD_FOR_COMPACTION,
            ),
            severity: RuleSeverity::Warning,
        });
    }

    let hit_context_limit = events.iter().any(|ev| match ev {
        AgentEvent::Error { error } => {
            let t = error.to_string().to_lowercase();
            CONTEXT_LIMIT_KEYWORDS.iter().any(|k| t.contains(k))
        }
        _ => false,
    });
    if hit_context_limit {
        violations.push(RuleViolation {
            rule_name: "compaction_triggered".into(),
            message: "Context limit was reached but no compaction event was detected".into(),
            severity: RuleSeverity::Warning,
        });
    }

    violations
}

/// Check for empty turns (no tool calls, no text output).
fn check_no_empty_turn(events: &[AgentEvent]) -> Vec<RuleViolation> {
    let has_tool = events
        .iter()
        .any(|ev| matches!(ev, AgentEvent::ToolCallStart { .. }));
    let has_text = events
        .iter()
        .any(|ev| matches!(ev, AgentEvent::ModelTextDelta { .. }));

    if !has_tool && !has_text && !events.is_empty() {
        return vec![RuleViolation {
            rule_name: "no_empty_turn".into(),
            message: "Turn produced no tool calls and no text output".into(),
            severity: RuleSeverity::Warning,
        }];
    }
    Vec::new()
}

/// Check that subagent delegation is followed by artifact readback.
fn check_subagent_readback(events: &[AgentEvent]) -> Vec<RuleViolation> {
    let mut violations = Vec::new();
    let mut saw_subagent = false;

    for ev in events {
        match ev {
            AgentEvent::ToolCallStart { name, .. }
                if name == "subagent" || name.starts_with("subagent") =>
            {
                saw_subagent = true;
            }
            AgentEvent::ToolCallStart { name, .. } if name == "artifact_read" && saw_subagent => {
                saw_subagent = false; // satisfied
            }
            _ => {}
        }
    }

    if saw_subagent {
        violations.push(RuleViolation {
            rule_name: "subagent_has_readback".into(),
            message: "Subagent was delegated but no artifact_read was observed".into(),
            severity: RuleSeverity::Warning,
        });
    }
    violations
}

/// Check for error storms: too many consecutive error tool results.
const MAX_CONSECUTIVE_ERRORS: usize = 5;

fn check_error_storm(events: &[AgentEvent]) -> Vec<RuleViolation> {
    let mut consecutive_errors = 0usize;
    for ev in events {
        if let AgentEvent::ToolCallEnd { output, .. } = ev {
            if output.is_error {
                consecutive_errors += 1;
            } else {
                consecutive_errors = 0;
            }
        }
        if consecutive_errors > MAX_CONSECUTIVE_ERRORS {
            return vec![RuleViolation {
                rule_name: "no_error_storm".into(),
                message: format!(
                    "{} consecutive tool errors (threshold: {})",
                    consecutive_errors, MAX_CONSECUTIVE_ERRORS,
                ),
                severity: RuleSeverity::Error,
            }];
        }
    }
    Vec::new()
}

/// Check that tool calls always produce corresponding ends.
fn check_tool_output_not_orphaned(events: &[AgentEvent]) -> Vec<RuleViolation> {
    let starts = events
        .iter()
        .filter(|ev| matches!(ev, AgentEvent::ToolCallStart { .. }))
        .count();
    let ends = events
        .iter()
        .filter(|ev| matches!(ev, AgentEvent::ToolCallEnd { .. }))
        .count();

    if starts != ends {
        return vec![RuleViolation {
            rule_name: "tool_output_not_orphaned".into(),
            message: format!(
                "Tool call start/end mismatch: {} starts, {} ends",
                starts, ends,
            ),
            severity: RuleSeverity::Error,
        }];
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fox_agent_core::ToolOutput;

    #[test]
    fn test_repeat_tool_storm() {
        let mut events = Vec::new();
        for _ in 0..15 {
            events.push(AgentEvent::ToolCallStart {
                call_id: "c1".into(),
                name: "echo".into(),
                input: serde_json::json!({"text":"hi"}),
            });
            events.push(AgentEvent::ToolCallEnd {
                call_id: "c1".into(),
                output: ToolOutput {
                },
            });
        }
        let engine = BehaviorRuleEngine::with_default_rules();
        let violations = engine.check_errors(&events);
        assert!(
            violations.is_empty(),
            "expected no storm violations, got: {violations:?}"
        );
    }

    #[test]
    fn test_error_storm() {
        let mut events = Vec::new();
        for i in 0..6 {
            events.push(AgentEvent::ToolCallStart {
                call_id: format!("c{}", i),
                name: "bash".into(),
                input: serde_json::json!({"command":"foo"}),
            });
            events.push(AgentEvent::ToolCallEnd {
                call_id: format!("c{}", i),
                output: ToolOutput {
                    text: "error".into(),
                    is_error: true,
                    json: None,
                },
            });
        }
        let engine = BehaviorRuleEngine::with_default_rules();
        let violations = engine.check_errors(&events);
        assert!(!violations.is_empty());
    }

    #[test]
    fn test_orphaned_tool_calls() {
        let events = vec![AgentEvent::ToolCallStart {
            call_id: "c1".into(),
            name: "echo".into(),
            input: serde_json::json!({}),
        }];
        let engine = BehaviorRuleEngine::with_default_rules();
        let violations = engine.check_errors(&events);
        assert!(!violations.is_empty());
    }

    #[test]
    fn test_subagent_without_readback() {
        let events = vec![
            AgentEvent::ToolCallStart {
                call_id: "c1".into(),
                name: "subagent".into(),
                input: serde_json::json!({"task":"search"}),
            },
            AgentEvent::ToolCallEnd {
                call_id: "c1".into(),
                output: ToolOutput {
                    text: "done".into(),
                    is_error: false,
                    json: None,
                },
            },
        ];
        let engine = BehaviorRuleEngine::with_default_rules();
        let violations = engine.check(&events);
        let sub_violations: Vec<_> = violations
            .iter()
            .filter(|v| v.rule_name == "subagent_has_readback")
            .collect();
        assert!(!sub_violations.is_empty());
    }
}
