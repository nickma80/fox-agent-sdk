//! Python binding for Agent — the main runtime interface.
//!
//! Runs the LLM → tool → LLM loop on the tokio runtime and
//! exposes events as an async generator via [`PyEventStream`].

use fox_agent_core::{AgentEvent, PermissionDecision, SessionSnapshot};
use fox_agent_sdk::Agent;
use pyo3::prelude::*;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::event_stream::PyEventStream;
use crate::session::PySessionSnapshot;
use crate::skills::PySkillRegistry;

/// Python-exposed Agent.
///
/// Created via [`super::builder::PyAgentBuilder::build`].
#[pyclass(name = "Agent", module = "fox_agent_sdk._core")]
pub struct PyAgent {
    agent: Arc<Agent>,
}

impl PyAgent {
    pub fn new<'py>(py: Python<'py>, agent: Arc<Agent>) -> PyResult<Bound<'py, Self>> {
        Bound::new(py, Self { agent })
    }
}

#[pymethods]
impl PyAgent {
    /// Get the session ID.
    #[getter]
    fn session_id(&self) -> String {
        self.agent.harness().session_id().to_string()
    }

    /// Access the skill registry for listing and inspecting loaded skills.
    ///
    /// Skills are automatically loaded from `.claude/skills/` when
    /// `with_default_tools()` is used during agent building.
    #[getter]
    fn skill_registry(&self) -> PySkillRegistry {
        PySkillRegistry::new(self.agent.harness().skill_registry.clone())
    }

    /// Run the agent on a user message.
    ///
    /// Returns an [`EventStream`] that yields events via `async for`:
    ///
    /// ```python
    /// async for event in agent.run("hello"):
    ///     print(event)
    /// ```
    fn run(&self, user_message: String) -> PyEventStream {
        let (tx, rx) = mpsc::channel::<AgentEvent>(256);
        let agent = self.agent.clone();

        let rt = crate::runtime::get_runtime();
        rt.spawn(async move {
            let _ = agent.run_once_streaming(&user_message, &tx).await;
        });

        PyEventStream::new(rx)
    }

    /// Resume execution after a permission request was intercepted.
    ///
    /// Returns an [`EventStream`] that continues yielding events.
    fn resume(&self, allow: bool) -> PyEventStream {
        let (tx, rx) = mpsc::channel::<AgentEvent>(256);
        let agent = self.agent.clone();

        let decision = if allow {
            PermissionDecision::Allow
        } else {
            PermissionDecision::Deny {
                reason: "user denied".to_string(),
            }
        };

        let rt = crate::runtime::get_runtime();
        rt.spawn(async move {
            let _ = agent.resume_streaming(decision, &tx).await;
        });

        PyEventStream::new(rx)
    }

    /// Take a snapshot of the current agent session state.
    ///
    /// Returns an awaitable that resolves to a [`SessionSnapshot`]:
    ///
    /// ```python
    /// snapshot = await agent.snapshot()
    /// ```
    fn snapshot<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let agent = self.agent.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let snap: SessionSnapshot = agent.snapshot().await;
            Python::with_gil(|py| {
                let py_snap = PySessionSnapshot::from_snapshot(&snap);
                Ok(py_snap.into_pyobject(py).unwrap().unbind())
            })
        })
    }
}
