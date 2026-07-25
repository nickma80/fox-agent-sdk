//! MCP integration adapter — bridges fox-agent-mcp into the Agent SDK.
//!
//! This module wraps [`fox_agent_mcp::McpClient`] and adapts MCP tool
//! definitions into the SDK's `Tool` trait so that MCP tools appear to
//! the agent like any other registered tool.

use fox_agent_core::{
    McpServerKind, McpServerProfile, McpToolDescriptorSnapshot, McpTransportKind, Tool,
    ToolContext, ToolError, ToolOutput,
};
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
        /// How long (ms) to wait after spawn before verifying the child is
        /// alive.  Increase this for slow‑start wrappers like `uvx` / `npx`
        /// that install packages on first run.  Defaults to 5000.
        startup_grace_ms: Option<u64>,
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
    /// Optional server-level profile used by routing/safety layers.
    pub profile: Option<McpServerProfile>,
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
                startup_grace_ms: Some(5_000),
            },
            auto_approve: false,
            profile: None,
            tools_only: None,
            request_timeout_ms: Some(30_000),
        }
    }
}

// ── Tool wrapper ──

/// An MCP tool exposed through the SDK `Tool` trait.
///
/// The `name` exposed via [`Tool::name`] is sanitised for provider compatibility
/// (e.g. `mcp://akshare/stock_info` → `mcp_akshare_stock_info`).  The internal
/// `mcp_name` field keeps the original `mcp://server/tool` format for routing.
pub struct McpTool {
    /// Sanitised name safe for provider APIs (matches `^[a-zA-Z0-9_-]+$`).
    name: String,
    /// Original MCP routing name (`mcp://server/tool`).
    mcp_name: String,
    description: String,
    parameters_schema: Value,
    client: McpClient,
}

fn infer_transport_kind(mode: &McpTransportMode) -> McpTransportKind {
    match mode {
        McpTransportMode::Stdio { .. } => McpTransportKind::Stdio,
        McpTransportMode::Sse { .. } => McpTransportKind::Sse,
    }
}

fn infer_server_kind(cfg: &McpServerConfig) -> McpServerKind {
    if let Some(profile) = &cfg.profile
        && profile.kind != McpServerKind::Unknown
    {
        return profile.kind;
    }
    let name = cfg.name.to_lowercase();
    if name.contains("filesystem") || name.contains("file") {
        McpServerKind::Filesystem
    } else if name.contains("browser") {
        McpServerKind::Browser
    } else if name.contains("shell") || name.contains("terminal") {
        McpServerKind::Shell
    } else if matches!(&cfg.transport, McpTransportMode::Sse { .. }) {
        McpServerKind::ExternalApi
    } else {
        McpServerKind::Unknown
    }
}

pub fn effective_profile(cfg: &McpServerConfig) -> McpServerProfile {
    let mut profile = cfg.profile.clone().unwrap_or_default();
    if profile.server_name.is_empty() {
        profile.server_name = cfg.name.clone();
    }
    if profile.transport == McpTransportKind::Unknown {
        profile.transport = infer_transport_kind(&cfg.transport);
    }
    if profile.kind == McpServerKind::Unknown {
        profile.kind = infer_server_kind(cfg);
    }
    if cfg.auto_approve {
        profile.auto_approve = true;
    }
    profile
}

/// Convert `mcp://server/tool` to a provider-safe name.
fn sanitise_tool_name(mcp: &str) -> String {
    let stripped = mcp.strip_prefix("mcp://").unwrap_or(mcp);
    let name = stripped.replace('/', "__");
    format!("mcp__{}", name)
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
        match self.client.call_tool(&self.mcp_name, input).await {
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
        McpTransportMode::Stdio { command, args, env, cwd, startup_grace_ms } => {
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
                startup_grace_ms: startup_grace_ms.unwrap_or(5_000),
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
) -> Result<(Vec<Arc<dyn Tool>>, McpClient, Vec<McpToolDescriptorSnapshot>), McpClientError> {
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
    let snapshots: Vec<McpToolDescriptorSnapshot> = definitions
        .iter()
        .map(|def| {
            let sanitised = sanitise_tool_name(&def.name);
            let server_name = def
                .name
                .strip_prefix("mcp://")
                .and_then(|rest| rest.split('/').next())
                .unwrap_or_default()
                .to_string();
            McpToolDescriptorSnapshot {
                server_name,
                tool_name: sanitised,
                original_name: def.name.clone(),
                description: def.description.clone(),
                input_schema: def.parameters_schema.clone(),
                output_hint: None,
            }
        })
        .collect();

    // Phase 3: build tool wrappers (each gets a cloned client)
    let tools: Vec<Arc<dyn Tool>> = definitions
        .iter()
        .map(|def| {
            Arc::new(McpTool {
                name: sanitise_tool_name(&def.name),
                mcp_name: def.name.clone(),
                description: def.description.clone(),
                parameters_schema: def.parameters_schema.clone(),
                client: client.clone(),
            }) as Arc<dyn Tool>
        })
        .collect();

    Ok((tools, client, snapshots))
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
