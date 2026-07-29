//! Evaluation module: LLM-as-Judge quality scoring and behavior correctness rules.
//!
//! Two complementary evaluation strategies:
//! 1. **LLM-as-Judge** (`judge.rs`): uses an independent evaluator model to score
//!    agent task completion quality (1–5 scale). The [`TaskJudge`] struct
//!    orchestrates the full pipeline: prompt → model call → response parsing.
//! 2. **Behavior rules** (`behavior_rules.rs`): programmatic assertions that check
//!    agent event streams for invariant violations.

pub mod behavior_rules;
pub mod judge;

pub use judge::{EvalReport, JudgeScores, TaskJudge};
