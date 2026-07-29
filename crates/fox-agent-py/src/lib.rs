//! Python module entry point for fox-agent-sdk native bindings.
//!
//! Registers all #[pyclass] types and #[pyfunction]s under the
//! `fox_agent_sdk._core` module.

use pyo3::prelude::*;

mod agent;
mod builder;
mod config;
mod eval;
mod event_stream;
mod hooks;
mod mcp;
mod memory;
mod plugin;
mod runtime;
mod session;
mod skills;
mod swarm;
mod tools;
mod types;

/// The `_core` native module imported by `fox_agent_sdk`.
#[pymodule]
fn _core(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {

    // Config types
    m.add_class::<config::PyProviderConfig>()?;
    m.add_class::<config::PySafetyConfig>()?;
    m.add_class::<config::PySdkConfig>()?;

    // Builder
    m.add_class::<builder::PyAgentBuilder>()?;

    // Agent
    m.add_class::<agent::PyAgent>()?;

    // Event stream (async iteration)
    m.add_class::<event_stream::PyEventStream>()?;

    // Tool system (Phase 2)
    m.add_class::<tools::PyToolContext>()?;
    m.add_class::<tools::PyToolOutput>()?;
    m.add_class::<tools::PyTool>()?;

    // MCP (Phase 2)
    m.add_class::<mcp::PyMcpTransportMode>()?;
    m.add_class::<mcp::PyMcpServerConfig>()?;

    // Memory (Phase 2)
    m.add_class::<memory::PyMemoryConfig>()?;
    m.add_class::<memory::PyMemoryScope>()?;
    m.add_class::<memory::PyRecallMode>()?;
    m.add_class::<memory::PyMemoryManager>()?;

    // Session (Phase 2)
    m.add_class::<session::PySessionSnapshot>()?;
    m.add_class::<session::PyFileSessionStore>()?;

    // Skills (Phase 3)
    m.add_class::<skills::PySkill>()?;
    m.add_class::<skills::PySkillRegistry>()?;

    // Evaluation (Phase 3)
    m.add_class::<eval::PyJudgeScores>()?;
    m.add_class::<eval::PyEvalReport>()?;
    m.add_class::<eval::PyTaskJudge>()?;
    m.add_class::<eval::PyRuleSeverity>()?;
    m.add_class::<eval::PyRuleViolation>()?;
    m.add_class::<eval::PyBehaviorRuleEngine>()?;
    m.add_class::<eval::PyCommandAssertion>()?;
    m.add_class::<eval::PyTaskAssertions>()?;
    m.add_class::<eval::PyAssertionReport>()?;

    // Hooks (Phase 3)
    m.add_class::<hooks::PyHookEvent>()?;
    m.add_class::<hooks::PyHooksConfig>()?;

    // Plugins (Phase 3)
    m.add_class::<plugin::PyPluginsConfig>()?;
    m.add_class::<plugin::PyPluginManifest>()?;

    // Swarm (Phase 3)
    m.add_class::<swarm::PyWorkerStatus>()?;
    m.add_class::<swarm::PyWorkerHandle>()?;
    m.add_class::<swarm::PyAgentReport>()?;
    m.add_class::<swarm::PySwarmSummaryReport>()?;
    m.add_class::<swarm::PyRetryPolicy>()?;
    m.add_class::<swarm::PySwarmCoordinator>()?;
    m.add_class::<swarm::PySwarmSupervisor>()?;

    // Register all event type constants for Python match/case
    let event_types = pyo3::types::PyDict::new(py);
    event_types.set_item("TEXT_DELTA", "text_delta")?;
    event_types.set_item("THINKING_DELTA", "thinking_delta")?;
    event_types.set_item("TOOL_START", "tool_start")?;
    event_types.set_item("TOOL_END", "tool_end")?;
    event_types.set_item("TOOL_PROGRESS", "tool_progress")?;
    event_types.set_item("USAGE", "usage")?;
    event_types.set_item("ERROR", "error")?;
    event_types.set_item("PERMISSION_REQUEST", "permission_request")?;
    event_types.set_item("TURN_START", "turn_start")?;
    event_types.set_item("TURN_END", "turn_end")?;
    event_types.set_item("ARTIFACT_STORED", "artifact_stored")?;
    event_types.set_item("ARTIFACT_READ", "artifact_read")?;
    event_types.set_item("MCP_CONNECTED", "mcp_connected")?;
    event_types.set_item("MCP_DISCONNECTED", "mcp_disconnected")?;
    event_types.set_item("PLAN_PROGRESS", "plan_progress")?;
    m.add("EventType", event_types)?;

    Ok(())
}
