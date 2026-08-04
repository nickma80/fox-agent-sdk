//! Agent runtime status — displayed as a structured block at the end of
//! the dynamic prompt section, replacing soft interrupts for task tracking.
//!
//! The status bar provides:
//! - Current task objective (from the latest user message)
//! - Plan progress (synchronized from GoalCheckpoint / todo_write)
//! - Runtime counters (turn, tools_called, compactions)
//! - Drift detection (consecutive_auto_turns vs limit)
//! - Optional token usage breakdown

use serde::{Deserialize, Serialize};

/// Agent runtime status displayed in the prompt's dynamic section.
///
/// Updated every turn; placed at the end of the dynamic prompt so it
/// is always visible to the model without polluting the message history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentStatus {
    /// The current task objective (from the latest user message).
    pub current_objective: String,
    /// Plan steps with completion status.
    pub plan_steps: Vec<PlanStepStatus>,
    /// Number of turns executed so far (including the current one).
    pub turn: u64,
    /// Total tool calls made across all turns.
    pub tools_called: u64,
    /// Number of compactions performed so far.
    pub compactions: u64,
    /// Consecutive turns without new user input.
    pub consecutive_auto_turns: u32,
    /// Threshold at which a drift warning is displayed.
    pub auto_turn_limit: u32,
    /// Time elapsed since session start (seconds).
    pub elapsed_secs: u64,
    /// Optional token usage breakdown (prompt / completion / total).
    pub token_usage: Option<TokenUsageStatus>,
}

/// Token usage breakdown reported by the provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsageStatus {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

impl Default for AgentStatus {
    fn default() -> Self {
        Self {
            current_objective: String::new(),
            plan_steps: Vec::new(),
            turn: 0,
            tools_called: 0,
            compactions: 0,
            consecutive_auto_turns: 0,
            auto_turn_limit: 5,
            elapsed_secs: 0,
            token_usage: None,
        }
    }
}

impl AgentStatus {
    /// Render the status bar as a markdown block for injection into the prompt.
    ///
    /// Returns `None` if there is no meaningful status to show (no objective,
    /// no plan steps, and no turn executed yet).
    pub fn render(&self) -> Option<String> {
        let has_content =
            !self.current_objective.is_empty() || !self.plan_steps.is_empty() || self.turn > 0;

        if !has_content {
            return None;
        }

        let mut out = String::from("<!-- AGENT_STATUS_BAR -->\n# Task Status\n");

        // ── Current Objective ──
        if !self.current_objective.is_empty() {
            out.push_str("\n## Current Objective\n");
            out.push_str(&self.current_objective);
            out.push('\n');
        }

        // ── Plan Progress ──
        if !self.plan_steps.is_empty() {
            out.push_str("\n## Plan Progress\n");
            for step in &self.plan_steps {
                let icon = match step.status {
                    StepStatus::Pending => "[ ]",
                    StepStatus::InProgress => "[~]",
                    StepStatus::Done => "[x]",
                    StepStatus::Skipped => "[-]",
                };
                out.push_str(&format!("- {icon} {}\n", step.description));
            }
        }

        // ── Runtime Stats ──
        out.push_str("\n## Runtime\n");
        out.push_str("| Metric              | Value |\n");
        out.push_str("|---------------------|-------|\n");
        out.push_str(&format!("| Turn                | {}     |\n", self.turn));
        out.push_str(&format!(
            "| Tools Called        | {}     |\n",
            self.tools_called
        ));
        out.push_str(&format!(
            "| Compactions         | {}     |\n",
            self.compactions
        ));
        out.push_str(&format!(
            "| Auto-Turns          | {}/{}  |\n",
            self.consecutive_auto_turns, self.auto_turn_limit
        ));

        if self.elapsed_secs > 0 {
            let mins = self.elapsed_secs / 60;
            let secs = self.elapsed_secs % 60;
            out.push_str(&format!("| Elapsed             | {mins}m {secs}s |\n"));
        }

        // ── Token Usage (optional) ──
        if let Some(ref tu) = self.token_usage {
            out.push_str(&format!(
                "| Prompt Tokens       | {}     |\n",
                tu.prompt_tokens
            ));
            out.push_str(&format!(
                "| Completion Tokens   | {}     |\n",
                tu.completion_tokens
            ));
            out.push_str(&format!(
                "| Total Tokens        | {}     |\n",
                tu.total_tokens
            ));
        }

        // ── Drift Warning ──
        if self.consecutive_auto_turns >= self.auto_turn_limit - 1 {
            out.push_str(&format!(
                "\n⚠️ **WARNING**: {}/{} consecutive auto-turns. \
                 If the current task is complete, stop and report your findings. \
                 If not, describe what specific step you are on.\n",
                self.consecutive_auto_turns, self.auto_turn_limit
            ));
        }

        out.push_str("\n<!-- /AGENT_STATUS_BAR -->");
        Some(out)
    }

    /// Increment counters after a tool call.
    pub fn record_tool_call(&mut self) {
        self.tools_called = self.tools_called.saturating_add(1);
    }

    /// Increment after a compaction.
    pub fn record_compaction(&mut self) {
        self.compactions = self.compactions.saturating_add(1);
    }

    /// Update elapsed time from a session start timestamp.
    pub fn update_elapsed(&mut self, session_start_secs: u64, now_secs: u64) {
        self.elapsed_secs = now_secs.saturating_sub(session_start_secs);
    }
}

/// A single step in the plan with its current status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStepStatus {
    /// Human-readable description of this step.
    pub description: String,
    /// Current completion status.
    pub status: StepStatus,
    /// Number of tool calls made specifically for this step.
    pub tool_calls: u64,
}

/// Completion status of a plan step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StepStatus {
    Pending,
    InProgress,
    Done,
    Skipped,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_empty_returns_none() {
        let status = AgentStatus::default();
        assert!(status.render().is_none(), "empty status should return None");
    }

    #[test]
    fn test_render_with_objective() {
        let status = AgentStatus {
            current_objective: "Implement Phase A".to_string(),
            ..Default::default()
        };
        let rendered = status.render().unwrap();
        assert!(rendered.contains("Implement Phase A"));
        assert!(rendered.contains("# Task Status"));
        assert!(rendered.contains("<!-- AGENT_STATUS_BAR -->"));
        assert!(rendered.contains("<!-- /AGENT_STATUS_BAR -->"));
    }

    #[test]
    fn test_render_with_turn_only() {
        let status = AgentStatus {
            turn: 5,
            ..Default::default()
        };
        let rendered = status.render().unwrap();
        assert!(rendered.contains("| Turn"), "should have runtime table");
    }

    #[test]
    fn test_render_with_plan_steps() {
        let status = AgentStatus {
            current_objective: "Refactor context".to_string(),
            plan_steps: vec![
                PlanStepStatus {
                    description: "Phase A: Status Bar".to_string(),
                    status: StepStatus::Done,
                    tool_calls: 3,
                },
                PlanStepStatus {
                    description: "Phase B: L2 Compression".to_string(),
                    status: StepStatus::InProgress,
                    tool_calls: 1,
                },
                PlanStepStatus {
                    description: "Phase C: L3 Summary".to_string(),
                    status: StepStatus::Pending,
                    tool_calls: 0,
                },
            ],
            ..Default::default()
        };
        let rendered = status.render().unwrap();
        assert!(rendered.contains("[x] Phase A: Status Bar"));
        assert!(rendered.contains("[~] Phase B: L2 Compression"));
        assert!(rendered.contains("[ ] Phase C: L3 Summary"));
        assert!(rendered.contains("## Plan Progress"));
    }

    #[test]
    fn test_render_runtime_stats() {
        let status = AgentStatus {
            current_objective: "Test".to_string(),
            turn: 10,
            tools_called: 42,
            compactions: 3,
            elapsed_secs: 125,
            ..Default::default()
        };
        let rendered = status.render().unwrap();
        assert!(rendered.contains("| Turn"), "should have runtime table");
        assert!(rendered.contains("2m 5s"), "should format elapsed time");
    }

    #[test]
    fn test_render_drift_warning() {
        let status = AgentStatus {
            current_objective: "Search codebase".to_string(),
            consecutive_auto_turns: 4,
            auto_turn_limit: 5,
            ..Default::default()
        };
        let rendered = status.render().unwrap();
        assert!(rendered.contains("⚠️"), "should show drift warning");
        assert!(rendered.contains("WARNING"));
        assert!(rendered.contains("4/5"));
    }

    #[test]
    fn test_render_no_drift_warning_below_threshold() {
        let status = AgentStatus {
            current_objective: "Search codebase".to_string(),
            consecutive_auto_turns: 2,
            auto_turn_limit: 5,
            ..Default::default()
        };
        let rendered = status.render().unwrap();
        assert!(!rendered.contains("⚠️"), "should NOT show drift warning");
        assert!(!rendered.contains("WARNING"));
    }

    #[test]
    fn test_record_tool_call_increments() {
        let mut status = AgentStatus::default();
        status.record_tool_call();
        status.record_tool_call();
        status.record_tool_call();
        assert_eq!(status.tools_called, 3);
    }

    #[test]
    fn test_record_compaction_increments() {
        let mut status = AgentStatus::default();
        status.record_compaction();
        status.record_compaction();
        assert_eq!(status.compactions, 2);
    }

    #[test]
    fn test_record_tool_call_saturating() {
        let mut status = AgentStatus::default();
        // Saturating means never overflows — just smoke test with high value.
        for _ in 0..1000 {
            status.record_tool_call();
        }
        assert_eq!(status.tools_called, 1000);
    }

    #[test]
    fn test_update_elapsed() {
        let mut status = AgentStatus::default();
        status.update_elapsed(100, 250);
        assert_eq!(status.elapsed_secs, 150);
    }

    #[test]
    fn test_update_elapsed_saturating() {
        let mut status = AgentStatus::default();
        status.update_elapsed(250, 100); // now < start → saturating to 0
        assert_eq!(status.elapsed_secs, 0);
    }

    #[test]
    fn test_step_status_icons() {
        let mut status = AgentStatus::default();
        status.current_objective = "Test".to_string();
        status.plan_steps = vec![
            PlanStepStatus {
                description: "Done step".to_string(),
                status: StepStatus::Done,
                tool_calls: 5,
            },
            PlanStepStatus {
                description: "Skipped step".to_string(),
                status: StepStatus::Skipped,
                tool_calls: 0,
            },
        ];
        let rendered = status.render().unwrap();
        assert!(rendered.contains("[x] Done step"));
        assert!(rendered.contains("[-] Skipped step"));
    }
}
