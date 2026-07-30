/// MCP Integration — demonstrates connecting to MCP servers and using their tools.
///
/// This example uses a mock MCP transport so it runs without external
/// dependencies (npx/uvx). For production use, see the AgentBuilder
/// pattern in the comments below.
///
/// # Real-world usage (AgentBuilder + real MCP servers):
///
/// ```ignore
/// use fox_agent_sdk::{AgentBuilder, McpServerConfig, ProviderConfig};
///
/// let mut agent = AgentBuilder::new()
///     .provider_config(ProviderConfig::deepseek(key))
///     .with_mcp_server(McpServerConfig {
///         name: "filesystem".into(),
///         command: "npx".into(),
///         args: vec!["-y".into(),
///             "@modelcontextprotocol/server-filesystem".into(),
///             "/tmp".into()],
///         ..Default::default()
///     })
///     .build()
///     .await?;
/// ```
use fox_agent_mcp::{
    McpClient, McpRequest, McpResponse, McpToolDefinition, McpTransport, TransportError,
};
use fox_agent_sdk::{
    AgentBuilder, AgentEvent, MockProvider, StreamEvent, Tool, ToolContext, ToolError, ToolOutput,
    TurnOutcome,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// ═══════════════════════════════════════════════════════════════════════════
// Mock MCP Transport
// ═══════════════════════════════════════════════════════════════════════════

/// A mock transport that returns pre‑scripted JSON‑RPC responses keyed by
/// method name. Useful for testing without spawning real MCP server processes.
struct MockTransport {
    responses: Arc<Mutex<HashMap<String, McpResponse>>>,
}

impl MockTransport {
    fn new() -> Self {
        Self {
            responses: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn set(&self, method: &str, result: Value) {
        self.responses.lock().unwrap().insert(
            method.to_string(),
            McpResponse::ok(Value::Number(0.into()), result),
        );
    }
}

#[async_trait::async_trait]
impl McpTransport for MockTransport {
    async fn send(&self, request: &McpRequest) -> Result<McpResponse, TransportError> {
        let guard = self.responses.lock().unwrap();
        if let Some(resp) = guard.get(&request.method) {
            Ok(resp.clone())
        } else {
            // notifications (e.g. "notifications/initialized") return empty ok
            Ok(McpResponse::ok(Value::Null, Value::Null))
        }
    }

    async fn start(&self) -> Result<(), TransportError> {
        Ok(())
    }
    async fn shutdown(&self) -> Result<(), TransportError> {
        Ok(())
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Mock tool responses — what the MCP server would return
// ═══════════════════════════════════════════════════════════════════════════

fn init_result() -> Value {
    json!({
        "protocolVersion": "2024-11-05",
        "capabilities": {
            "tools": { "listChanged": false }
        },
        "serverInfo": { "name": "mock-calc", "version": "1.0" }
    })
}

fn tools_list_result() -> Value {
    json!({
        "tools": [
            {
                "name": "add",
                "description": "Add two numbers",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "a": {"type": "number"},
                        "b": {"type": "number"}
                    },
                    "required": ["a", "b"]
                }
            },
            {
                "name": "multiply",
                "description": "Multiply two numbers",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "x": {"type": "number"},
                        "y": {"type": "number"}
                    },
                    "required": ["x", "y"]
                }
            }
        ]
    })
}

fn tool_call_result(text: &str) -> Value {
    json!({
        "content": [{"type": "text", "text": text}],
        "isError": false
    })
}

// ═══════════════════════════════════════════════════════════════════════════
// Tool wrapper — bridges MCP client calls to the Agent's Tool trait
// ═══════════════════════════════════════════════════════════════════════════

struct McpToolAdapter {
    client: McpClient,
    full_name: String,
}

#[async_trait::async_trait]
impl Tool for McpToolAdapter {
    fn name(&self) -> &str {
        &self.full_name
    }

    fn description(&self) -> &str {
        "MCP tool (mock calculator)"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    async fn execute(&self, input: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        match self.client.call_tool(&self.full_name, input).await {
            Ok(text) => Ok(ToolOutput {
                text,
                is_error: false,
                json: None,
            }),
            Err(e) => Ok(ToolOutput {
                text: format!("MCP error: {e}"),
                is_error: true,
                json: None,
            }),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Main
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::main]
async fn main() {
    println!("=== MCP Integration Demo ===\n");

    // ── 1. Build mock MCP server transport ──
    let transport = MockTransport::new();
    transport.set("initialize", init_result());
    transport.set("tools/list", tools_list_result());
    // tools/call is matched by method name — the MCP client sends
    // the method "tools/call" regardless of which tool is being invoked
    transport.set("tools/call", tool_call_result("42"));

    // ── 2. Connect via McpClient::connect() (handshake + discovery) ──
    let handle = McpClient::connect(
        Box::new(transport),
        "calc",
        true,                // auto_approve
        None::<Vec<String>>, // no tools filter
    )
    .await
    .expect("failed to connect to mock MCP server");
    println!("[OK] Connected to mock MCP server 'calc'");

    let client = McpClient::new();
    client.add_server(handle).await;

    // ── 3. Discover tools ──
    let definitions: Vec<McpToolDefinition> = client.list_tools().await.unwrap();
    println!("[OK] Discovered {} tool(s):", definitions.len());
    for def in &definitions {
        println!("  • {} — {}", def.name, def.description);
    }
    assert_eq!(definitions.len(), 2);
    assert!(definitions.iter().any(|d| d.name.contains("calc/add")));
    assert!(definitions.iter().any(|d| d.name.contains("calc/multiply")));

    // ── 4. Call a tool directly via McpClient ──
    println!("\n[>>] Calling calc/add via McpClient");
    let result = client
        .call_tool("mcp://calc/add", json!({"a": 12, "b": 30}))
        .await
        .unwrap();
    println!("[<<] result: {result}");
    assert_eq!(result, "42");

    // ── 5. Build agent with AgentBuilder, then register MCP tools ──
    let provider = Arc::new(MockProvider::new("mock"));

    // Build tools from discovered MCP definitions
    let mcp_tools: Vec<Arc<dyn Tool>> = definitions
        .iter()
        .map(|def| {
            Arc::new(McpToolAdapter {
                client: client.clone(),
                full_name: def.name.clone(),
            }) as Arc<dyn Tool>
        })
        .collect();

    // ── 6. Build agent, then attach MCP client and register tools ──
    provider.push_script(vec![
        StreamEvent::ToolUse {
            id: "c1".into(),
            name: "mcp://calc/add".into(),
            input: json!({"a": 12, "b": 30}),
        },
        StreamEvent::MessageStop { stop_reason: None },
    ]);
    provider.push_script(vec![
        StreamEvent::TextDelta {
            text: "12 + 30 = 42 (via MCP)".into(),
        },
        StreamEvent::MessageStop { stop_reason: None },
    ]);

    let mut agent = AgentBuilder::new()
        .with_provider(provider.clone())
        .model_id("mock-1")
        .build()
        .await
        .expect("build agent");

    // Attach the manually-connected MCP client
    agent.mcp_client = Some(client);

    // Register the pre-built MCP tools into the agent harness
    for tool in mcp_tools {
        agent.harness().register_tool(tool).await;
    }

    let (tx, mut rx) = tokio::sync::mpsc::channel::<AgentEvent>(32);
    let outcome = agent.run_once_streaming("12 + 30 = ?", &tx).await.unwrap();

    let mut saw_tool = false;
    for _ in 0..16 {
        let ev = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
            .await
            .ok()
            .flatten();
        let Some(ev) = ev else { break };
        if let AgentEvent::ToolCallEnd { ref output, .. } = ev {
            if output.text.contains("42") {
                saw_tool = true;
                println!("[OK] Agent routed call through MCP: {}", output.text);
            }
        }
    }
    assert!(saw_tool, "expected tool call end event");

    match outcome {
        TurnOutcome::Completed { text } => {
            println!("[agent] {text}");
            assert!(text.contains("42"));
        }
        other => panic!("expected Completed, got {other:?}"),
    }

    println!("\n=== PASSED: MCP Integration ===");
    println!("  [x] MCP server handshake (initialize + tools/list)");
    println!("  [x] Tool discovery via tools/list");
    println!("  [x] Direct tool call via McpClient::call_tool");
    println!("  [x] Tool → Agent harness registration");
    println!("  [x] Agent loop routes MCP tool calls correctly");
}
