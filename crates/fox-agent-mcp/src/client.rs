//! MCP client — connects to one MCP server, discovers tools, executes calls.

use crate::transport::McpTransport;
use crate::types::*;
use crate::tool_adapter::{McpToolDefinition, mcp_tool_to_definition};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Errors that can occur during MCP client operations.
#[derive(Debug, thiserror::Error)]
pub enum McpClientError {
    #[error("connection failed for server '{server}': {message}")]
    ConnectionFailed { server: String, message: String },
    #[error("tool not found: {tool}")]
    ToolNotFound { tool: String },
    #[error("tool call failed: {0}")]
    ToolCallFailed(String),
    #[error("transport: {0}")]
    Transport(#[from] crate::transport::TransportError),
    #[error("initialize failed: {0}")]
    InitializeFailed(String),
    #[error("serialization: {0}")]
    Serialization(#[from] serde_json::Error),
}

/// A handle to a connected MCP server.
///
/// Created by [`McpClient::connect`]; used internally by [`McpClient`].
pub struct McpServerHandle {
    pub name: String,
    transport: Box<dyn McpTransport>,
    pub auto_approve: bool,
    pub tools_only: Option<Vec<String>>,
    pub capabilities: ServerCapabilities,
}

/// An MCP client that manages connections to MCP servers.
///
/// This is the main entry point for agent-side MCP integration. It:
/// 1. Connects to servers (initialize handshake)
/// 2. Discovers tools via `tools/list`
/// 3. Executes tool calls via `tools/call`
#[derive(Clone)]
pub struct McpClient {
    servers: Arc<RwLock<Vec<McpServerHandle>>>,
}

impl McpClient {
    /// Create an empty client — call [`connect_server`] to add servers.
    pub fn new() -> Self {
        Self { servers: Arc::new(RwLock::new(Vec::new())) }
    }

    /// Connect to an MCP server via the given transport.
    ///
    /// Performs initialize handshake and tools/list.
    pub async fn connect(
        transport: Box<dyn McpTransport>,
        server_name: impl Into<String>,
        auto_approve: bool,
        tools_only: Option<Vec<String>>,
    ) -> Result<McpServerHandle, McpClientError> {
        let name: String = server_name.into();

        transport.start().await.map_err(|e| McpClientError::ConnectionFailed {
            server: name.clone(),
            message: e.to_string(),
        })?;

        // ── 1. Initialize ──
        let init_req = McpRequest::new(
            Value::Number(1.into()),
            "initialize",
            Some(serde_json::to_value(InitializeParams {
                protocol_version: "2024-11-05".into(),
                capabilities: ClientCapabilities::default(),
                client_info: ClientInfo {
                    name: "fox-agent-sdk".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                },
            })?),
        );

        let init_resp = transport.send(&init_req).await.map_err(|e| {
            McpClientError::ConnectionFailed { server: name.clone(), message: e.to_string() }
        })?;

        if let Some(err) = &init_resp.error {
            return Err(McpClientError::InitializeFailed(format!(
                "server returned error: {} (code {})",
                err.message, err.code,
            )));
        }

        let init_result: InitializeResult = serde_json::from_value(
            init_resp.result.ok_or_else(|| {
                McpClientError::InitializeFailed("no result in initialize response".into())
            })?,
        )
        .map_err(|e| McpClientError::InitializeFailed(e.to_string()))?;

        // Send initialized notification
        let initialized_notif = McpRequest::new(
            Value::Number(2.into()),
            "notifications/initialized",
            None,
        );
        let _ = transport.send(&initialized_notif).await;

        // ── 2. tools/list ──
        let tools_req = McpRequest::new(
            Value::Number(3.into()),
            "tools/list",
            None,
        );
        let tools_resp = transport.send(&tools_req).await.map_err(|e| {
            McpClientError::ConnectionFailed { server: name.clone(), message: e.to_string() }
        })?;

        // tools/list is optional — gracefully degrade
        let _ = tools_resp;

        Ok(McpServerHandle {
            name,
            transport,
            auto_approve,
            tools_only,
            capabilities: init_result.capabilities,
        })
    }

    /// Add a pre-connected server handle.
    pub async fn add_server(&self, handle: McpServerHandle) {
        self.servers.write().await.push(handle);
    }

    /// List all tool definitions from all connected servers.
    pub async fn list_tools(&self) -> Result<Vec<McpToolDefinition>, McpClientError> {
        let servers = self.servers.read().await;
        let mut all_tools = Vec::new();
        for server in servers.iter() {
            let req = McpRequest::new(
                Value::String(Uuid::new_v4().to_string()),
                "tools/list",
                None,
            );
            match server.transport.send(&req).await {
                Ok(resp) => {
                    if let Some(result) = resp.result {
                        if let Ok(list) = serde_json::from_value::<ToolsListResult>(result) {
                            for tool in &list.tools {
                                // Apply tools_only filter
                                if let Some(ref only) = server.tools_only {
                                    if !only.iter().any(|n| n == &tool.name) {
                                        continue;
                                    }
                                }
                                all_tools.push(mcp_tool_to_definition(&server.name, tool));
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        server = %server.name,
                        error = %e,
                        "tools/list failed for MCP server — skipping"
                    );
                }
            }
        }
        Ok(all_tools)
    }

    /// Execute a tool call on the appropriate MCP server.
    ///
    /// The `tool_name` should be in `mcp://server/tool` format.
    pub async fn call_tool(
        &self,
        tool_name: &str,
        arguments: Value,
    ) -> Result<String, McpClientError> {
        // Parse "mcp://server/tool" → (server, tool)
        let stripped = tool_name.strip_prefix("mcp://").unwrap_or(tool_name);
        let (server_name, bare_tool) = stripped
            .split_once('/')
            .ok_or_else(|| McpClientError::ToolNotFound { tool: tool_name.into() })?;

        let servers = self.servers.read().await;
        let server = servers.iter().find(|s| s.name == server_name).ok_or_else(|| {
            McpClientError::ToolNotFound { tool: tool_name.into() }
        })?;

        let params = ToolCallParams {
            name: bare_tool.into(),
            arguments,
        };

        let req = McpRequest::new(
            Value::String(Uuid::new_v4().to_string()),
            "tools/call",
            Some(serde_json::to_value(&params)?),
        );

        let resp = server.transport.send(&req).await?;

        if let Some(err) = &resp.error {
            return Err(McpClientError::ToolCallFailed(format!(
                "{}: {} (code {})",
                tool_name, err.message, err.code,
            )));
        }

        let result: ToolCallResult = serde_json::from_value(
            resp.result.ok_or_else(|| {
                McpClientError::ToolCallFailed("no result in tools/call response".into())
            })?,
        )?;

        // Concatenate all text content blocks.
        let text = result
            .content
            .iter()
            .filter_map(|c| c.text.as_deref())
            .collect::<Vec<_>>()
            .join("\n");

        if text.is_empty() {
            Ok(format!("Tool {} returned empty result", tool_name))
        } else {
            Ok(text)
        }
    }

    /// Return the number of connected servers.
    pub async fn server_count(&self) -> usize {
        self.servers.read().await.len()
    }

    /// Return server names.
    pub async fn server_names(&self) -> Vec<String> {
        self.servers.read().await.iter().map(|s| s.name.clone()).collect()
    }

    /// Check if a server is set to auto-approve.
    pub async fn is_auto_approve(&self, server_name: &str) -> bool {
        self.servers
            .read()
            .await
            .iter()
            .find(|s| s.name == server_name)
            .map(|s| s.auto_approve)
            .unwrap_or(false)
    }
}
