//! Python bindings for the Hooks system.
//!
//! Hooks allow users to intercept agent lifecycle events
//! (PreToolUse, PostToolUse, SessionStart, etc.) and run
//! custom scripts or inject context.

use fox_agent_sdk::{HookEvent, HooksConfig};
use pyo3::prelude::*;
use std::path::PathBuf;

// ── PyHookEvent ──

/// Lifecycle events that can trigger hooks.
#[pyclass(name = "HookEvent", module = "fox_agent_sdk._core")]
#[derive(Clone)]
pub struct PyHookEvent {
    #[allow(dead_code)]
    inner: HookEvent,
}

#[pymethods]
impl PyHookEvent {
    /// Session started.
    #[classattr]
    const SESSION_START: &'static str = "session-start";

    /// User submitted a prompt.
    #[classattr]
    const USER_PROMPT_SUBMIT: &'static str = "user-prompt-submit";

    /// Before a tool executes.
    #[classattr]
    const PRE_TOOL_USE: &'static str = "pre-tool-use";

    /// After a tool executed.
    #[classattr]
    const POST_TOOL_USE: &'static str = "post-tool-use";

    /// One-way notification (does not alter flow).
    #[classattr]
    const NOTIFICATION: &'static str = "notification";

    /// Agent stopped (error, budget, etc.).
    #[classattr]
    const STOP: &'static str = "stop";

    /// Sub-agent completed.
    #[classattr]
    const SUBAGENT_STOP: &'static str = "subagent-stop";

    /// Before context compaction.
    #[classattr]
    const PRE_COMPACT: &'static str = "pre-compact";

    /// Permission prompt triggered.
    #[classattr]
    const PERMISSION_PROMPT: &'static str = "permission-prompt";

    /// Before a file write.
    #[classattr]
    const PRE_FILE_WRITE: &'static str = "pre-file-write";

    /// After a file write.
    #[classattr]
    const POST_FILE_WRITE: &'static str = "post-file-write";

    fn __repr__(&self) -> String {
        format!("HookEvent({:?})", self.inner)
    }
}

// ── PyHooksConfig ──

/// Configuration for the hooks system.
#[pyclass(name = "HooksConfig", module = "fox_agent_sdk._core")]
#[derive(Clone)]
pub struct PyHooksConfig {
    inner: HooksConfig,
}

#[pymethods]
impl PyHooksConfig {
    #[new]
    #[pyo3(signature = (enabled=true, timeout_secs=30, max_concurrent=5, load_global=true))]
    fn new(
        enabled: bool,
        timeout_secs: u64,
        max_concurrent: usize,
        load_global: bool,
    ) -> Self {
        Self {
            inner: HooksConfig {
                enabled,
                timeout_secs,
                max_concurrent,
                additional_directories: vec![],
                load_global,
            },
        }
    }

    /// Add an additional directory to scan for hook definitions.
    fn add_directory(&mut self, dir: String) {
        self.inner.additional_directories.push(PathBuf::from(dir));
    }

    fn __repr__(&self) -> String {
        format!(
            "HooksConfig(enabled={}, timeout={}s, max_concurrent={})",
            self.inner.enabled, self.inner.timeout_secs, self.inner.max_concurrent
        )
    }
}
