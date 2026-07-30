//! Python binding for AgentBuilder.
//!
//! Usage:
//! ```python
//! builder = AgentBuilder()
//! builder.provider_config(cfg)
//! builder.model_id("deepseek-v4-flash")
//! builder.with_default_tools()
//! builder.with_tool(my_custom_tool)        # Phase 2: custom Python tools
//! builder.with_mcp_server(mcp_config)      # Phase 2: MCP servers
//! agent = await builder.build()
//! ```

use crate::agent::PyAgent;
use crate::config::{PyProviderConfig, PySafetyConfig, PySdkConfig};
use crate::mcp::PyMcpServerConfig;
use crate::session::PyFileSessionStore;
use fox_agent_core::{
    DefaultSafetyPolicy, FoxAgentSdkConfig, SafetyConfig, SessionStore as CoreSessionStore,
};
use fox_agent_sdk::AgentBuilder;
use pyo3::prelude::*;
use std::sync::Arc;

#[pyclass(name = "AgentBuilder", module = "fox_agent_sdk._core")]
pub struct PyAgentBuilder {
    builder: AgentBuilder,
}

#[pymethods]
impl PyAgentBuilder {
    #[new]
    fn new() -> Self {
        Self {
            builder: AgentBuilder::new(),
        }
    }

    fn provider_config(&mut self, config: PyProviderConfig) {
        self.builder = std::mem::take(&mut self.builder).provider_config(config.into_inner());
    }

    fn sdk_config_file(&mut self, path: String) -> PyResult<()> {
        let cfg = FoxAgentSdkConfig::load_from_file(&path).map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!(
                "failed to load config from '{}': {}",
                path, e
            ))
        })?;
        self.builder = std::mem::take(&mut self.builder).sdk_config(cfg);
        Ok(())
    }

    fn sdk_config(&mut self, config: PySdkConfig) {
        self.builder = std::mem::take(&mut self.builder).sdk_config(config.into_inner());
    }

    fn model_id(&mut self, id: String) {
        self.builder = std::mem::take(&mut self.builder).model_id(id);
    }

    fn working_dir(&mut self, dir: String) {
        self.builder = std::mem::take(&mut self.builder).working_dir(dir);
    }

    fn with_default_tools(&mut self) {
        self.builder = std::mem::take(&mut self.builder).with_default_tools();
    }

    /// Register a custom Python tool.
    ///
    /// The tool object must implement `name()`, `description()`,
    /// `parameters_schema()`, and `execute(input, ctx)`.
    fn with_tool(&mut self, tool: Py<PyAny>) -> PyResult<()> {
        let tool_arc = crate::tools::register_python_tool(tool)?;
        self.builder = std::mem::take(&mut self.builder).with_tool(tool_arc);
        Ok(())
    }

    /// Register an MCP server connection.
    fn with_mcp_server(&mut self, config: PyMcpServerConfig) {
        self.builder = std::mem::take(&mut self.builder).with_mcp_server(config.into_inner());
    }

    /// Set a file-backed session store for persistence.
    fn with_session_store(&mut self, store: PyFileSessionStore) {
        let dyn_store: Arc<dyn CoreSessionStore> = store.into_arc();
        self.builder = std::mem::take(&mut self.builder).with_session_store(dyn_store);
    }

    fn with_safety_policy(&mut self, config: PySafetyConfig) {
        self.builder = std::mem::take(&mut self.builder).with_safety_policy(config.into_inner());
    }

    fn with_system_prompt(&mut self, template: String) {
        self.builder = std::mem::take(&mut self.builder).with_system_prompt(template);
    }

    /// Build the agent (async).
    ///
    /// Returns a Python awaitable that resolves to an [`Agent`]:
    ///
    /// ```python
    /// agent = await builder.build()
    /// ```
    fn build<'py>(&mut self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let mut builder = std::mem::take(&mut self.builder);

        builder = builder.with_safety_policy(SafetyConfig {
            default_policy: DefaultSafetyPolicy::Allow,
            productive_tool_confirm: false,
            ..Default::default()
        });

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let agent = builder.build().await.map_err(|e| {
                pyo3::exceptions::PyRuntimeError::new_err(format!("failed to build agent: {}", e))
            })?;

            Python::with_gil(|py| PyAgent::new(py, Arc::new(agent)).map(|bound| bound.unbind()))
        })
    }
}
