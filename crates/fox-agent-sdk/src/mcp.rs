//! MCP integration adapter — bridges fox-agent-mcp into the Agent SDK.
//!
//! This module wraps [`fox_agent_mcp::McpClient`] and adapts MCP tool
//! definitions into the SDK's `Tool` trait so that MCP tools appear to
//! the agent like any other registered tool.

use fox_agent_core::{Tool, ToolContext, ToolError, ToolOutput};
use fox_agent_mcp::{McpClient, McpClientError, StdioTransport, StdioTransportConfig};
use serde_json::Value;
use std::sync::Arc;

/// Configuration for a single MCP server connection.
#[derive(Clone, Default)]
pub struct McpServerConfig {
    /// Human-readable name for this server.
    pub name: String,
    /// Transport configuration (stdio for now).
    pub command: String,
    /// Arguments for the subprocess.
    pub args: Vec<String>,
    /// Environment variables.
    pub env: Option<Vec<(String, String)>>,
    /// Working directory.
    pub cwd: Option<String>,
    /// If true, all tools from this server are auto-approved.
    pub auto_approve: bool,
    /// If set, only expose tools with these names.
    pub tools_only: Option<Vec<String>>,
    /// Request timeout in milliseconds.
    pub request_timeout_ms: Option<u64>,
}

// ── Tool wrapper ──

/// An MCP tool exposed through the SDK `Tool` trait.
pub struct McpTool {
    name: String,
    description: String,
    parameters_schema: Value,
    client: McpClient,
}

#[async_trait::async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Value {
        self.parameters_schema.clone()
    }

    async fn execute(&self, input: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        match self.client.call_tool(&self.name, input).await {
            Ok(text) => Ok(ToolOutput {
                text,
                is_error: false,
                json: None,
            }),
            Err(e) => Ok(ToolOutput {
                text: format!("MCP tool error: {e}"),
                is_error: true,
                json: None,
            }),
        }
    }
}

// ── Builder integration ──

fn build_transport(cfg: &McpServerConfig) -> Box<StdioTransport> {
    let timeout = cfg.request_timeout_ms.unwrap_or(30_000);

    let env = cfg.env.as_ref().map(|pairs| {
        pairs
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect::<std::collections::HashMap<_, _>>()
    });

    Box::new(StdioTransport::new(StdioTransportConfig {
        command: cfg.command.clone(),
        args: cfg.args.clone(),
        env,
        cwd: cfg.cwd.clone(),
        request_timeout_ms: timeout,
    }))
}

/// Connect to MCP servers and discover their tools.
///
/// Returns a tuple of:
/// - `Vec<Arc<dyn Tool>>` — tools to register with the agent
/// - `McpClient` — the client handle for runtime tool calls
///
/// # Errors
///
/// Returns `McpClientError` if a mandatory server fails to connect.
pub async fn connect_and_discover_tools(
    servers: &[McpServerConfig],
) -> Result<(Vec<Arc<dyn Tool>>, McpClient), McpClientError> {
    let client = McpClient::new();

    // Phase 1: connect all servers
    for cfg in servers {
        let transport = build_transport(cfg);
        let handle = McpClient::connect(
            transport,
            &cfg.name,
            cfg.auto_approve,
            cfg.tools_only.clone(),
        )
        .await?;
        client.add_server(handle).await;
    }

    // Phase 2: discover all tools
    let definitions = client.list_tools().await?;

    // Phase 3: build tool wrappers (each gets a cloned client)
    let tools: Vec<Arc<dyn Tool>> = definitions
        .iter()
        .map(|def| {
            Arc::new(McpTool {
                name: def.name.clone(),
                description: def.description.clone(),
                parameters_schema: def.parameters_schema.clone(),
                client: client.clone(),
            }) as Arc<dyn Tool>
        })
        .collect();

    Ok((tools, client))
}
