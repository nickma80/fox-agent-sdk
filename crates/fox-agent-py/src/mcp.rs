//! MCP (Model Context Protocol) configuration bindings.
//!
//! Exposes McpServerConfig with static factory methods for stdio and SSE
//! transport modes.

use pyo3::prelude::*;

/// MCP transport mode: stdio subprocess or SSE HTTP.
#[pyclass(name = "McpTransportMode", module = "fox_agent_sdk._core")]
#[derive(Clone)]
pub struct PyMcpTransportMode {
    inner: fox_agent_sdk::McpTransportMode,
}

#[pymethods]
impl PyMcpTransportMode {
    /// Create a stdio transport mode.
    #[staticmethod]
    #[pyo3(signature = (command, args, *, env = None, cwd = None, startup_grace_ms = Some(5000_u64)))]
    fn stdio(
        command: String,
        args: Vec<String>,
        env: Option<Vec<(String, String)>>,
        cwd: Option<String>,
        startup_grace_ms: Option<u64>,
    ) -> Self {
        Self {
            inner: fox_agent_sdk::McpTransportMode::Stdio {
                command,
                args,
                env,
                cwd,
                startup_grace_ms,
            },
        }
    }

    /// Create an SSE transport mode.
    #[staticmethod]
    #[pyo3(signature = (url, headers, *, connect_timeout_secs = None))]
    fn sse(
        url: String,
        headers: Vec<(String, String)>,
        connect_timeout_secs: Option<u64>,
    ) -> Self {
        Self {
            inner: fox_agent_sdk::McpTransportMode::Sse {
                url,
                headers,
                connect_timeout_secs,
            },
        }
    }

    fn __repr__(&self) -> String {
        match &self.inner {
            fox_agent_sdk::McpTransportMode::Stdio { command, args, .. } => {
                format!("McpTransportMode.stdio({} {})", command, args.join(" "))
            }
            fox_agent_sdk::McpTransportMode::Sse { url, .. } => {
                format!("McpTransportMode.sse({})", url)
            }
        }
    }
}

impl PyMcpTransportMode {
    pub fn into_inner(self) -> fox_agent_sdk::McpTransportMode {
        self.inner
    }
}

/// MCP server configuration.
#[pyclass(name = "McpServerConfig", module = "fox_agent_sdk._core")]
#[derive(Clone)]
pub struct PyMcpServerConfig {
    inner: fox_agent_sdk::McpServerConfig,
}

#[pymethods]
impl PyMcpServerConfig {
    /// Create an MCP server config with custom transport.
    #[staticmethod]
    #[pyo3(signature = (name, transport, *, auto_approve = false, tools_only = None, request_timeout_ms = None))]
    fn new(
        name: String,
        transport: PyMcpTransportMode,
        auto_approve: bool,
        tools_only: Option<Vec<String>>,
        request_timeout_ms: Option<u64>,
    ) -> Self {
        Self {
            inner: fox_agent_sdk::McpServerConfig {
                name,
                transport: transport.into_inner(),
                auto_approve,
                profile: None,
                tools_only,
                request_timeout_ms,
            },
        }
    }

    /// Shortcut: create a stdio MCP server config.
    #[staticmethod]
    #[pyo3(signature = (name, command, args, *, auto_approve = false, env = None, cwd = None, startup_grace_ms = Some(5000_u64), tools_only = None, request_timeout_ms = None))]
    fn stdio(
        name: String,
        command: String,
        args: Vec<String>,
        auto_approve: bool,
        env: Option<Vec<(String, String)>>,
        cwd: Option<String>,
        startup_grace_ms: Option<u64>,
        tools_only: Option<Vec<String>>,
        request_timeout_ms: Option<u64>,
    ) -> Self {
        Self {
            inner: fox_agent_sdk::McpServerConfig {
                name,
                transport: fox_agent_sdk::McpTransportMode::Stdio {
                    command,
                    args,
                    env,
                    cwd,
                    startup_grace_ms,
                },
                auto_approve,
                profile: None,
                tools_only,
                request_timeout_ms,
            },
        }
    }

    /// Shortcut: create an SSE MCP server config.
    #[staticmethod]
    #[pyo3(signature = (name, url, *, auto_approve = false, headers = vec![], connect_timeout_secs = None, tools_only = None, request_timeout_ms = None))]
    #[allow(clippy::too_many_arguments)]
    fn sse(
        name: String,
        url: String,
        auto_approve: bool,
        headers: Vec<(String, String)>,
        connect_timeout_secs: Option<u64>,
        tools_only: Option<Vec<String>>,
        request_timeout_ms: Option<u64>,
    ) -> Self {
        Self {
            inner: fox_agent_sdk::McpServerConfig {
                name,
                transport: fox_agent_sdk::McpTransportMode::Sse {
                    url,
                    headers,
                    connect_timeout_secs,
                },
                auto_approve,
                profile: None,
                tools_only,
                request_timeout_ms,
            },
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "McpServerConfig(name='{}', auto_approve={})",
            self.inner.name, self.inner.auto_approve
        )
    }
}

impl PyMcpServerConfig {
    pub fn into_inner(self) -> fox_agent_sdk::McpServerConfig {
        self.inner
    }
}
