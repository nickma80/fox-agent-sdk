//! MCP integration adapter — bridges fox-agent-mcp into the Agent SDK.
//!
//! This module wraps [`fox_agent_mcp::McpClient`] and adapts MCP tool
//! definitions into the SDK's `Tool` trait so that MCP tools appear to
//! the agent like any other registered tool.

use fox_agent_core::{Tool, ToolContext, ToolError, ToolOutput};
use fox_agent_mcp::{
    McpClient, McpClientError, StdioTransport, StdioTransportConfig,
    SseTransport, SseTransportConfig,
};
use serde_json::Value;
use std::sync::Arc;

/// Transport mode for an MCP server connection.
#[derive(Clone)]
pub enum McpTransportMode {
    /// stdio subprocess (local MCP server)
    Stdio {
        command: String,
        args: Vec<String>,
        env: Option<Vec<(String, String)>>,
        cwd: Option<String>,
    },
    /// SSE HTTP long‑poll (remote MCP server)
    Sse {
        url: String,
        headers: Vec<(String, String)>,
        /// Connection timeout in seconds. Defaults to 30.
        connect_timeout_secs: Option<u64>,
    },
}

/// Configuration for a single MCP server connection.
#[derive(Clone)]
pub struct McpServerConfig {
    /// Human-readable name for this server.
    pub name: String,
    /// Transport mode.
    pub transport: McpTransportMode,
    /// If true, all tools from this server are auto-approved.
    pub auto_approve: bool,
    /// If set, only expose tools with these names.
    pub tools_only: Option<Vec<String>>,
    /// Request timeout in milliseconds (for stdio) or seconds (for SSE).
    pub request_timeout_ms: Option<u64>,
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            transport: McpTransportMode::Stdio {
                command: String::new(),
                args: Vec::new(),
                env: None,
                cwd: None,
            },
            auto_approve: false,
            tools_only: None,
            request_timeout_ms: Some(30_000),
        }
    }
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

fn build_transport(cfg: &McpServerConfig) -> Box<dyn fox_agent_mcp::McpTransport> {
    match &cfg.transport {
        McpTransportMode::Stdio { command, args, env, cwd } => {
            let timeout = cfg.request_timeout_ms.unwrap_or(30_000);
            let env_map = env.as_ref().map(|pairs| {
                pairs
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect::<std::collections::HashMap<_, _>>()
            });

            Box::new(StdioTransport::new(StdioTransportConfig {
                command: command.clone(),
                args: args.clone(),
                env: env_map,
                cwd: cwd.clone(),
                request_timeout_ms: timeout,
            }))
        }
        McpTransportMode::Sse { url, headers, connect_timeout_secs } => {
            let timeout = cfg.request_timeout_ms.unwrap_or(30_000);
            Box::new(SseTransport::new(SseTransportConfig {
                url: url.clone(),
                headers: headers.clone(),
                connect_timeout_secs: connect_timeout_secs.unwrap_or(30),
                request_timeout_secs: std::time::Duration::from_millis(timeout).as_secs_f64().ceil() as u64,
            }))
        }
    }
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

/// Build a system-prompt section listing connected MCP resources and prompts.
pub async fn build_mcp_context_summary(client: &McpClient) -> String {
    let mut sections = String::new();

    // Resources
    if let Ok(resources) = client.list_resources().await {
        let filtered = resources.iter()
            .take(5) // max 5 resources shown in prompt
            .collect::<Vec<_>>();
        if !filtered.is_empty() {
            sections.push_str("# MCP Resources\n\n");
            sections.push_str("The following resources are available from connected MCP servers:\n\n");
            for res in &filtered {
                sections.push_str(&format!(
                    "- **{}** (`{}`)\n",
                    res.name,
                    res.uri
                ));
            }
            if resources.len() > 5 {
                sections.push_str(&format!(
                    "\n... and {} more resources. Use `read_resource` to access them.\n",
                    resources.len() - 5
                ));
            }
            sections.push('\n');
        }
    }

    // Prompts
    if let Ok(prompts) = client.list_prompts().await {
        let filtered = prompts.iter()
            .take(5)
            .collect::<Vec<_>>();
        if !filtered.is_empty() {
            sections.push_str("# MCP Prompts\n\n");
            sections.push_str("The following prompt templates are available from connected MCP servers:\n\n");
            for p in &filtered {
                sections.push_str(&format!(
                    "- **{}**: {}\n",
                    p.name,
                    p.description.as_deref().unwrap_or("no description")
                ));
            }
            if prompts.len() > 5 {
                sections.push_str(&format!(
                    "\n... and {} more prompts. Use `get_prompt` to retrieve them.\n",
                    prompts.len() - 5
                ));
            }
            sections.push('\n');
        }
    }

    sections
}
