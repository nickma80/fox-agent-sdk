//! Evaluation system bindings for Phase 3.
//!
//! Exposes:
//! - TaskAssertions / CommandAssertion / AssertionReport (world-state checks)
//! - BehaviorRuleEngine / RuleViolation (behavior rules)
//! - TaskJudge / JudgeScores / EvalReport (LLM-as-judge)

use fox_agent_core::{self as core, AssertionReport, CommandAssertion, Provider, TaskAssertions};
use fox_agent_sdk::eval::behavior_rules::{BehaviorRuleEngine, RuleSeverity, RuleViolation};
use fox_agent_sdk::eval::{EvalReport, JudgeScores, TaskJudge};
use pyo3::prelude::*;
use pyo3::types::PyList;
use std::sync::Arc;

// ── Helpers ──

/// Build an `Arc<dyn Provider>` from a Python provider config dict.
///
/// The dict must contain at minimum `provider_name`, `base_url`, and `api_key`.
fn build_provider_from_config(cfg: &core::ProviderConfig) -> Arc<dyn Provider> {
    match cfg.provider_name.as_str() {
        "openai" => Arc::new(
            fox_agent_providers::OpenAiCompatibleProvider::new(cfg.clone())
                .expect("failed to construct OpenAI provider"),
        ),
        "anthropic" => Arc::new(
            fox_agent_providers::AnthropicCompatibleProvider::new(cfg.clone())
                .expect("failed to construct Anthropic provider"),
        ),
        "deepseek" => Arc::new(fox_agent_providers::DeepSeekProvider::new(cfg.clone())),
        other => {
            panic!("unknown provider name: {other}, expected: openai, anthropic, deepseek")
        }
    }
}

// ── PyJudgeScores ──

/// Scores assigned by the LLM judge.
#[pyclass(name = "JudgeScores", module = "fox_agent_sdk._core")]
#[derive(Clone)]
pub struct PyJudgeScores {
    inner: JudgeScores,
}

#[pymethods]
impl PyJudgeScores {
    /// Completeness score (1-5): did the agent solve the problem?
    #[getter]
    fn completeness(&self) -> u8 {
        self.inner.completeness
    }

    /// Solution quality (1-5): was the solution reasonable and efficient?
    #[getter]
    fn solution_quality(&self) -> u8 {
        self.inner.solution_quality
    }

    /// Error recovery (1-5 or None if not applicable).
    #[getter]
    fn error_recovery(&self) -> Option<u8> {
        self.inner.error_recovery
    }

    /// Redundancy score (1-5): was the output free of waste? (5 = clean)
    #[getter]
    fn redundancy(&self) -> u8 {
        self.inner.redundancy
    }

    /// Weighted average score (completeness * 0.4 + quality * 0.3 +
    /// error_recovery * 0.15 + redundancy * 0.15).
    fn weighted_average(&self) -> f64 {
        self.inner.weighted_average()
    }

    fn __repr__(&self) -> String {
        format!(
            "JudgeScores(completeness={}, quality={}, avg={:.2})",
            self.inner.completeness,
            self.inner.solution_quality,
            self.inner.weighted_average()
        )
    }
}

// ── PyEvalReport ──

/// Evaluation report built from an agent's session events.
#[pyclass(name = "EvalReport", module = "fox_agent_sdk._core")]
#[derive(Clone)]
pub struct PyEvalReport {
    inner: EvalReport,
}

#[pymethods]
impl PyEvalReport {
    /// Build an EvalReport from session events.
    ///
    /// `events` should be a list of event dicts from `agent.run(...)`.
    #[new]
    fn new(
        task_id: String,
        user_prompt: String,
        agent_response: String,
        events: &Bound<'_, PyList>,
        assertions_passed: bool,
    ) -> PyResult<Self> {
        let mut rust_events = Vec::new();
        for ev in events.iter() {
            if let Ok(dict) = ev.downcast::<pyo3::types::PyDict>() {
                if let Some(ae) = crate::types::py_event_to_agent_event(&dict) {
                    rust_events.push(ae);
                }
            }
        }
        Ok(Self {
            inner: EvalReport::from_events(
                &task_id,
                &user_prompt,
                &agent_response,
                &rust_events,
                assertions_passed,
            ),
        })
    }

    #[getter]
    fn task_id(&self) -> &str {
        &self.inner.task_id
    }

    #[getter]
    fn user_prompt(&self) -> &str {
        &self.inner.user_prompt
    }

    #[getter]
    fn agent_response(&self) -> &str {
        &self.inner.agent_response
    }

    #[getter]
    fn tool_summary(&self) -> Vec<(String, usize)> {
        self.inner.tool_summary.clone()
    }

    #[getter]
    fn assertions_passed(&self) -> bool {
        self.inner.assertions_passed
    }

    fn __repr__(&self) -> String {
        format!(
            "EvalReport(task_id='{}', assertions_passed={})",
            self.inner.task_id, self.inner.assertions_passed
        )
    }
}

impl PyEvalReport {
    pub fn into_inner(self) -> EvalReport {
        self.inner
    }
}

// ── PyTaskJudge ──

/// LLM-as-Judge evaluator that scores agent responses.
///
/// Uses a separate LLM call to evaluate completeness, quality,
/// error recovery, and redundancy of agent outputs.
#[pyclass(name = "TaskJudge", module = "fox_agent_sdk._core")]
pub struct PyTaskJudge {
    inner: TaskJudge,
}

#[pymethods]
impl PyTaskJudge {
    /// Create a TaskJudge using the provider config and model id for evaluation.
    ///
    /// The provider config should have `provider_name`, `base_url`, and `api_key`.
    /// The model_id should be a capable model suitable for evaluation (e.g., "deepseek-v4-flash").
    #[new]
    fn new(provider_config: crate::config::PyProviderConfig, model_id: String) -> Self {
        let cfg = provider_config.into_inner();
        let provider = build_provider_from_config(&cfg);
        Self {
            inner: TaskJudge::new(provider, &model_id),
        }
    }

    /// Evaluate an EvalReport using the LLM judge.
    ///
    /// Returns JudgeScores on success, or raises RuntimeError on failure.
    fn evaluate(&self, report: PyEvalReport, py: Python<'_>) -> PyResult<PyJudgeScores> {
        let rt = crate::runtime::get_runtime();
        let scores = py
            .allow_threads(|| {
                rt.block_on(async { self.inner.evaluate(&report.into_inner()).await })
            })
            .map_err(|e| {
                pyo3::exceptions::PyRuntimeError::new_err(format!(
                    "task judge evaluation failed: {}",
                    e
                ))
            })?;
        Ok(PyJudgeScores { inner: scores })
    }

    fn __repr__(&self) -> String {
        "TaskJudge()".to_string()
    }
}

// ── PyRuleSeverity ──

/// Severity level constants for rule violations.
#[pyclass(name = "RuleSeverity", module = "fox_agent_sdk._core")]
pub struct PyRuleSeverity;

#[pymethods]
impl PyRuleSeverity {
    #[classattr]
    const WARNING: &'static str = "warning";
    #[classattr]
    const ERROR: &'static str = "error";
}

// ── PyRuleViolation ──

/// A single behavior rule violation found during evaluation.
#[pyclass(name = "RuleViolation", module = "fox_agent_sdk._core")]
#[derive(Clone)]
pub struct PyRuleViolation {
    #[pyo3(get)]
    rule_name: String,
    #[pyo3(get)]
    message: String,
    #[pyo3(get)]
    severity: String,
}

impl From<RuleViolation> for PyRuleViolation {
    fn from(v: RuleViolation) -> Self {
        let severity = match v.severity {
            RuleSeverity::Error => "error",
            RuleSeverity::Warning => "warning",
        };
        Self {
            rule_name: v.rule_name,
            message: v.message,
            severity: severity.to_string(),
        }
    }
}

// ── PyBehaviorRuleEngine ──

/// Engine that checks agent event streams for known anti-patterns.
///
/// Comes with 7 built-in rules: no repeat-tool storms, no retry-after-deny,
/// compaction-triggered check, no empty turns, subagent-has-readback,
/// no error storms, and tool-output-not-orphaned.
#[pyclass(name = "BehaviorRuleEngine", module = "fox_agent_sdk._core")]
pub struct PyBehaviorRuleEngine {
    inner: BehaviorRuleEngine,
}

#[pymethods]
impl PyBehaviorRuleEngine {
    /// Create a rule engine with all default rules pre-loaded.
    #[new]
    fn new() -> Self {
        Self {
            inner: BehaviorRuleEngine::with_default_rules(),
        }
    }

    /// Check a list of event dicts against all rules.
    ///
    /// `events` should be the list from `agent.run(...)`.
    ///
    /// Returns a list of `RuleViolation` objects.
    fn check(&self, events: &Bound<'_, PyList>) -> PyResult<Vec<PyRuleViolation>> {
        let mut rust_events = Vec::new();
        for ev in events.iter() {
            if let Ok(dict) = ev.downcast::<pyo3::types::PyDict>() {
                if let Some(ae) = crate::types::py_event_to_agent_event(&dict) {
                    rust_events.push(ae);
                }
            }
        }
        let violations = self.inner.check(&rust_events);
        Ok(violations.into_iter().map(PyRuleViolation::from).collect())
    }

    /// Check and return only Error-level violations.
    fn check_errors(&self, events: &Bound<'_, PyList>) -> PyResult<Vec<PyRuleViolation>> {
        let mut rust_events = Vec::new();
        for ev in events.iter() {
            if let Ok(dict) = ev.downcast::<pyo3::types::PyDict>() {
                if let Some(ae) = crate::types::py_event_to_agent_event(&dict) {
                    rust_events.push(ae);
                }
            }
        }
        let violations = self.inner.check_errors(&rust_events);
        Ok(violations.into_iter().map(PyRuleViolation::from).collect())
    }

    fn __repr__(&self) -> String {
        "BehaviorRuleEngine()".to_string()
    }
}

// ── PyCommandAssertion ──

/// Defines a command to run and its expected outcome.
#[pyclass(name = "CommandAssertion", module = "fox_agent_sdk._core")]
#[derive(Clone)]
pub struct PyCommandAssertion {
    inner: CommandAssertion,
}

#[pymethods]
impl PyCommandAssertion {
    #[new]
    #[pyo3(signature = (
        command,
        *,
        working_dir = ".",
        expected_exit_code = 0,
        stdout_contains = None,
        stderr_not_contains = None,
    ))]
    fn new(
        command: String,
        working_dir: &str,
        expected_exit_code: i32,
        stdout_contains: Option<String>,
        stderr_not_contains: Option<String>,
    ) -> Self {
        Self {
            inner: CommandAssertion {
                working_dir: std::path::PathBuf::from(working_dir),
                command,
                expected_exit_code,
                stdout_contains,
                stderr_not_contains,
            },
        }
    }

    fn __repr__(&self) -> String {
        format!("CommandAssertion('{}')", self.inner.command)
    }
}

impl PyCommandAssertion {
    pub fn into_inner(self) -> CommandAssertion {
        self.inner
    }
}

// ── PyTaskAssertions ──

/// World-state assertions to verify agent task completion.
///
/// Checks file existence, file contents, directory existence,
/// and command exit codes after the agent runs.
#[pyclass(name = "TaskAssertions", module = "fox_agent_sdk._core")]
#[derive(Clone)]
pub struct PyTaskAssertions {
    inner: TaskAssertions,
}

#[pymethods]
impl PyTaskAssertions {
    /// Create an empty assertions object.
    #[new]
    fn new() -> Self {
        Self {
            inner: TaskAssertions {
                file_exists: Vec::new(),
                file_contains: Vec::new(),
                file_not_contains: Vec::new(),
                dir_exists: Vec::new(),
                commands: Vec::new(),
                max_duration_secs: None,
            },
        }
    }

    /// Assert that a file must exist.
    fn file_exists(&mut self, path: String) {
        self.inner.file_exists.push(std::path::PathBuf::from(path));
    }

    /// Assert that a file must contain a substring.
    fn file_contains(&mut self, path: String, substring: String) {
        self.inner
            .file_contains
            .push((std::path::PathBuf::from(path), substring));
    }

    /// Assert that a file must NOT contain a substring.
    fn file_not_contains(&mut self, path: String, substring: String) {
        self.inner
            .file_not_contains
            .push((std::path::PathBuf::from(path), substring));
    }

    /// Assert that a directory must exist.
    fn dir_exists(&mut self, path: String) {
        self.inner.dir_exists.push(std::path::PathBuf::from(path));
    }

    /// Add a command assertion.
    fn command(&mut self, cmd: PyCommandAssertion) {
        self.inner.commands.push(cmd.into_inner());
    }

    /// Set max allowed duration in seconds.
    fn max_duration(&mut self, secs: u64) {
        self.inner.max_duration_secs = Some(secs);
    }

    /// Run all assertions against the given working directory.
    ///
    /// Returns an AssertionReport with pass/fail status and failure messages.
    fn run(&self, working_dir: String) -> PyAssertionReport {
        let report = core::run_task_assertions(&self.inner, &std::path::PathBuf::from(working_dir));
        PyAssertionReport { inner: report }
    }

    fn __repr__(&self) -> String {
        let total = self.inner.file_exists.len()
            + self.inner.file_contains.len()
            + self.inner.file_not_contains.len()
            + self.inner.dir_exists.len()
            + self.inner.commands.len();
        format!("TaskAssertions(checks={total})")
    }
}

// ── PyAssertionReport ──

/// Report from running task assertions.
#[pyclass(name = "AssertionReport", module = "fox_agent_sdk._core")]
#[derive(Clone)]
pub struct PyAssertionReport {
    inner: AssertionReport,
}

#[pymethods]
impl PyAssertionReport {
    #[getter]
    fn passed(&self) -> bool {
        self.inner.passed
    }

    #[getter]
    fn total(&self) -> usize {
        self.inner.total
    }

    #[getter]
    fn passed_count(&self) -> usize {
        self.inner.passed_count
    }

    #[getter]
    fn failures(&self) -> Vec<String> {
        self.inner.failures.clone()
    }

    fn __repr__(&self) -> String {
        format!(
            "AssertionReport(passed={}, {}/{})",
            self.inner.passed, self.inner.passed_count, self.inner.total
        )
    }
}
