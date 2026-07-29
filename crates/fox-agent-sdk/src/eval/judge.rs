//! LLM-as-Judge: quality scoring for agent task executions.
//!
//! Uses a separate evaluation model to assess agent output across four dimensions:
//! completeness, solution quality, error recovery, and redundancy.
//!
//! ## Architecture
//!
//! ```text
//! Agent 执行完毕 → EvalReport → build_judge_prompt()
//! → TaskJudge.evaluate() → provider.complete() → 评分 JSON
//! → parse_judge_response() → JudgeScores → EvalReport::with_scores()
//! ```

use fox_agent_core::{AgentEvent, Message, Provider, ProviderError, StreamEvent};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::SystemTime;

/// Quality scores assigned by the judge model (1–5 per dimension).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeScores {
    /// Did the agent solve the user's problem? (1=no, 5=yes)
    pub completeness: u8,

    /// Was the approach reasonable and efficient? (1=poor, 5=excellent)
    pub solution_quality: u8,

    /// How well did the agent recover from tool failures? (1=poor, 5=excellent)
    /// N/A if no errors occurred.
    pub error_recovery: Option<u8>,

    /// Was there unnecessary repetition? (1=much redundancy, 5=no redundancy)
    pub redundancy: u8,
}

impl JudgeScores {
    /// Weighted average score (completeness ×0.4, quality ×0.3,
    /// recovery ×0.15, redundancy ×0.15).
    pub fn weighted_average(&self) -> f64 {
        let rec = self.error_recovery.unwrap_or(5) as f64;
        self.completeness as f64 * 0.4
            + self.solution_quality as f64 * 0.3
            + rec * 0.15
            + self.redundancy as f64 * 0.15
    }

    /// Total raw score out of max possible.
    pub fn total(&self) -> u8 {
        self.completeness + self.solution_quality + self.error_recovery.unwrap_or(5) + self.redundancy
    }
}

/// Full evaluation report for a single task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalReport {
    /// Task identifier for correlation.
    pub task_id: String,

    /// User's original prompt.
    pub user_prompt: String,

    /// Agent's final text response (truncated to 2000 chars).
    pub agent_response: String,

    /// Summary of tool calls: list of (tool_name, count) pairs.
    pub tool_summary: Vec<(String, usize)>,

    /// Whether end-to-end task assertions passed.
    pub assertions_passed: bool,

    /// Judge scores (None if judge was not configured / not run).
    pub scores: Option<JudgeScores>,

    /// Timestamp of evaluation.
    pub evaluated_at: SystemTime,

    /// Additional context or notes.
    pub notes: Vec<String>,
}

impl EvalReport {
    /// Create a report from agent events.
    pub fn from_events(
        task_id: &str,
        user_prompt: &str,
        agent_response: &str,
        events: &[AgentEvent],
        assertions_passed: bool,
    ) -> Self {
        let mut tool_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for ev in events {
            if let AgentEvent::ToolCallStart { name, .. } = ev {
                *tool_counts.entry(name.clone()).or_insert(0) += 1;
            }
        }
        let mut tool_summary: Vec<_> = tool_counts.into_iter().collect();
        tool_summary.sort_by(|a, b| b.1.cmp(&a.1)); // most used first

        // Truncate agent response
        let truncated: String = agent_response.chars().take(2000).collect();

        Self {
            task_id: task_id.to_string(),
            user_prompt: user_prompt.to_string(),
            agent_response: truncated,
            tool_summary,
            assertions_passed,
            scores: None,
            evaluated_at: SystemTime::now(),
            notes: Vec::new(),
        }
    }

    /// Attach judge scores to this report.
    pub fn with_scores(mut self, scores: JudgeScores) -> Self {
        self.scores = Some(scores);
        self
    }

    /// Add a note.
    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }
}

/// Generate the evaluation prompt to send to the judge model.
///
/// The judge is asked to output JSON with the scoring fields.
pub fn build_judge_prompt(report: &EvalReport) -> String {
    let tool_list = report
        .tool_summary
        .iter()
        .map(|(name, count)| format!("  - {} (×{})", name, count))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"You are a quality evaluator for an AI coding agent. Score the agent's performance on a scale of 1 (poor) to 5 (excellent) across these dimensions:

**User's task:**
{task}

**Agent's final response:**
{response}

**Tools used:**
{tools}

**End-to-end assertions:** {assertions}

Please output a JSON object with these fields:
- "completeness": 1-5 — Did the agent fully solve the user's problem?
- "solution_quality": 1-5 — Was the approach efficient and reasonable?
- "error_recovery": 1-5 or null — How well did the agent recover from any errors? (null if no errors)
- "redundancy": 1-5 — Was the agent efficient with tool calls? (5 = no waste)
- "rationale": string — Brief explanation (1-3 sentences)

Output ONLY the JSON object, no other text."#,
        task = report.user_prompt,
        response = report.agent_response,
        tools = if tool_list.is_empty() { "(none)" } else { &tool_list },
        assertions = if report.assertions_passed { "PASSED" } else { "FAILED" },
    )
}

/// Parse judge model response into scores.
pub fn parse_judge_response(raw: &str) -> Option<JudgeScores> {
    // Find JSON object in the response (handle markdown code fences)
    let json_str = raw
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    #[derive(Deserialize)]
    struct RawScores {
        completeness: i32,
        solution_quality: i32,
        error_recovery: Option<i32>,
        redundancy: i32,
    }

    serde_json::from_str::<RawScores>(json_str).ok().map(|s| JudgeScores {
        completeness: s.completeness.clamp(1, 5) as u8,
        solution_quality: s.solution_quality.clamp(1, 5) as u8,
        error_recovery: s.error_recovery.map(|v| v.clamp(1, 5) as u8),
        redundancy: s.redundancy.clamp(1, 5) as u8,
    })
}

/// LLM-as-Judge evaluator that calls an independent model to score agent performance.
///
/// Holds a reference to any [`Provider`] (real or mock) and a model ID.
/// The `evaluate()` method orchestrates the full pipeline:
/// prompt construction → model call → response parsing.
pub struct TaskJudge {
    provider: Arc<dyn Provider>,
    model_id: String,
}

impl TaskJudge {
    /// Create a new judge backed by the given provider and model.
    ///
    /// The evaluator model should be capable of following JSON output instructions.
    /// Lightweight models (e.g. `deepseek-chat`) are sufficient for scoring tasks.
    pub fn new(provider: Arc<dyn Provider>, model_id: impl Into<String>) -> Self {
        Self {
            provider,
            model_id: model_id.into(),
        }
    }

    /// Evaluate an agent's task execution and return quality scores.
    ///
    /// # Flow
    ///
    /// 1. Build the evaluation prompt from the [`EvalReport`]
    /// 2. Send it to the evaluator model via `provider.complete()`
    /// 3. Collect text deltas from the streaming response
    /// 4. Parse the response JSON into [`JudgeScores`]
    ///
    /// # Errors
    ///
    /// Returns `ProviderError` if the model call fails or the response cannot be parsed.
    pub async fn evaluate(&self, report: &EvalReport) -> Result<JudgeScores, ProviderError> {
        let prompt = build_judge_prompt(report);
        let messages = vec![Message::user(prompt)];

        let mut stream = self
            .provider
            .complete(
                &self.model_id,
                &messages,
                &[], // no tools — judge only produces text
                "",  // system prompt is embedded in the user message
                "",
                None,
            )
            .await?;

        let mut response_text = String::new();
        while let Some(event) = stream.next().await {
            match event {
                Ok(StreamEvent::TextDelta { text }) => response_text.push_str(&text),
                Ok(StreamEvent::MessageStop { .. }) => break,
                Err(e) => return Err(e),
                _ => {} // ignore Usage, ThinkingDelta, etc.
            }
        }

        parse_judge_response(&response_text).ok_or_else(|| ProviderError::Message {
            message: format!(
                "failed to parse judge response: {}",
                &response_text[..response_text.len().min(300)]
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fox_agent_providers::MockProvider;

    // --- Pure function tests (existing) ---

    #[test]
    fn test_weighted_average() {
        let scores = JudgeScores {
            completeness: 5,
            solution_quality: 4,
            error_recovery: None,
            redundancy: 4,
        };
        let avg = scores.weighted_average();
        assert!(avg > 4.0 && avg < 5.0);
    }

    #[test]
    fn test_parse_judge_response() {
        let raw = r#"{"completeness":5,"solution_quality":4,"error_recovery":null,"redundancy":4,"rationale":"Good job"}"#;
        let scores = parse_judge_response(raw).unwrap();
        assert_eq!(scores.completeness, 5);
        assert_eq!(scores.solution_quality, 4);
        assert!(scores.error_recovery.is_none());
    }

    #[test]
    fn test_parse_with_markdown_fence() {
        let raw = "```json\n{\"completeness\":3,\"solution_quality\":2,\"error_recovery\":3,\"redundancy\":2}\n```";
        let scores = parse_judge_response(raw).unwrap();
        assert_eq!(scores.completeness, 3);
    }

    #[test]
    fn test_score_clamping() {
        // Test that out-of-range values are clamped (only JSON-parseable ints)
        let raw = r#"{"completeness":10,"solution_quality":0,"error_recovery":7,"redundancy":6}"#;
        let scores = parse_judge_response(raw).unwrap();
        assert_eq!(scores.completeness, 5);
        assert_eq!(scores.solution_quality, 1);
        assert_eq!(scores.error_recovery, Some(5));
    }

    // --- Full pipeline tests with MockProvider ---

    fn make_report() -> EvalReport {
        EvalReport {
            task_id: "test-001".into(),
            user_prompt: "Create a Rust project".into(),
            agent_response: "I created the project successfully.".into(),
            tool_summary: vec![("bash".into(), 2), ("write".into(), 1)],
            assertions_passed: true,
            scores: None,
            evaluated_at: SystemTime::now(),
            notes: vec![],
        }
    }

    #[tokio::test]
    async fn test_task_judge_evaluate_success() {
        let mut provider = MockProvider::new("eval-judge");
        // Simulate the evaluator model returning a JSON score
        provider.push_script(vec![
            StreamEvent::TextDelta {
                text: r#"{"completeness":5,"solution_quality":4,"error_recovery":null,"redundancy":4,"rationale":"Good job"}"#.into(),
            },
            StreamEvent::MessageStop {
                stop_reason: Some("end_turn".into()),
            },
        ]);

        let judge = TaskJudge::new(Arc::new(provider), "eval-model");
        let scores = judge.evaluate(&make_report()).await.unwrap();

        assert_eq!(scores.completeness, 5);
        assert_eq!(scores.solution_quality, 4);
        assert!(scores.error_recovery.is_none());
        assert_eq!(scores.redundancy, 4);
    }

    #[tokio::test]
    async fn test_task_judge_evaluate_with_markdown_fence() {
        let mut provider = MockProvider::new("eval-judge");
        // Simulate a model that wraps JSON in markdown code fence
        provider.push_script(vec![
            StreamEvent::TextDelta {
                text: "```json\n".into(),
            },
            StreamEvent::TextDelta {
                text: r#"{"completeness":3,"solution_quality":2,"error_recovery":3,"redundancy":2}"#.into(),
            },
            StreamEvent::TextDelta {
                text: "\n```".into(),
            },
            StreamEvent::MessageStop {
                stop_reason: Some("end_turn".into()),
            },
        ]);

        let judge = TaskJudge::new(Arc::new(provider), "eval-model");
        let scores = judge.evaluate(&make_report()).await.unwrap();

        assert_eq!(scores.completeness, 3);
        assert_eq!(scores.solution_quality, 2);
        assert_eq!(scores.error_recovery, Some(3));
    }

    #[tokio::test]
    async fn test_task_judge_evaluate_scores_clamped() {
        let mut provider = MockProvider::new("eval-judge");
        // Out-of-range values should be clamped by parse_judge_response
        provider.push_script(vec![
            StreamEvent::TextDelta {
                text: r#"{"completeness":10,"solution_quality":0,"error_recovery":7,"redundancy":-1}"#.into(),
            },
            StreamEvent::MessageStop {
                stop_reason: Some("end_turn".into()),
            },
        ]);

        let judge = TaskJudge::new(Arc::new(provider), "eval-model");
        let scores = judge.evaluate(&make_report()).await.unwrap();

        assert_eq!(scores.completeness, 5);
        assert_eq!(scores.solution_quality, 1);
        assert_eq!(scores.error_recovery, Some(5));
    }

    #[tokio::test]
    async fn test_task_judge_evaluate_unparseable_response() {
        let mut provider = MockProvider::new("eval-judge");
        // Garbage response — should fail to parse
        provider.push_script(vec![
            StreamEvent::TextDelta {
                text: "I'm sorry, I cannot evaluate this task.".into(),
            },
            StreamEvent::MessageStop {
                stop_reason: Some("end_turn".into()),
            },
        ]);

        let judge = TaskJudge::new(Arc::new(provider), "eval-model");
        let result = judge.evaluate(&make_report()).await;

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("failed to parse judge response"));
    }

    #[tokio::test]
    async fn test_task_judge_evaluate_empty_response() {
        let mut provider = MockProvider::new("eval-judge");
        // Empty response — should fail to parse
        provider.push_script(vec![StreamEvent::MessageStop {
            stop_reason: Some("end_turn".into()),
        }]);

        let judge = TaskJudge::new(Arc::new(provider), "eval-model");
        let result = judge.evaluate(&make_report()).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_eval_report_with_scores_roundtrip() {
        // Full round-trip: create report → evaluate → attach scores
        let mut provider = MockProvider::new("eval-judge");
        provider.push_script(vec![
            StreamEvent::TextDelta {
                text: r#"{"completeness":4,"solution_quality":5,"error_recovery":null,"redundancy":5,"rationale":"Clean execution"}"#.into(),
            },
            StreamEvent::MessageStop {
                stop_reason: Some("end_turn".into()),
            },
        ]);

        let judge = TaskJudge::new(Arc::new(provider), "eval-model");
        let report = make_report();
        let scores = judge.evaluate(&report).await.unwrap();
        let report = report.with_scores(scores);

        let s = report.scores.unwrap();
        assert_eq!(s.completeness, 4);
        assert_eq!(s.solution_quality, 5);
        let avg = s.weighted_average();
        assert!(avg > 4.0);
    }
}
