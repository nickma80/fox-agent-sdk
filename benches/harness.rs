//! Shared benchmark harness: builds agent, initialises tracing subscriber.
//!
//! Import this module from individual bench files to avoid duplication.

use fox_agent_core::{AgentEvent, FoxAgentSdkConfig, StreamEvent, Tool, ToolContext, ToolError, ToolOutput};
use fox_agent_sdk::{Agent, Harness, MockProvider};
use serde_json::{Value, json};
use std::sync::Arc;
use tracing_chrome::ChromeLayerBuilder;
use tracing_subscriber::prelude::*;

/// Build an Agent backed by a MockProvider with the given tools registered.
pub async fn build_mock_agent(
    tools: Vec<Arc<dyn Tool>>,
) -> (Agent, MockProvider) {
    let provider = MockProvider::new("bench-mock");
    let harness = Harness::new(FoxAgentSdkConfig::default(), None);
    for t in tools {
        harness.register_tool(t).await;
    }
    let model = Arc::new(fox_agent_core::DefaultModel::new(
        Arc::new(provider.clone()),
        "bench-model",
    ));
    let agent = Agent::new(model, harness, Arc::new(tokio::sync::RwLock::new(None)));
    (agent, provider)
}

/// Initialise a tracing subscriber for Chrome trace output.
/// Set `BENCH_TRACE_DIR` env var to enable; otherwise no-op.
pub fn init_tracing() {
    let Ok(dir) = std::env::var("BENCH_TRACE_DIR") else { return };
    let (chrome_layer, _guard) = ChromeLayerBuilder::new()
        .file(std::path::PathBuf::from(&dir).join("bench-trace.json"))
        .build();
    let _ = tracing_subscriber::registry()
        .with(chrome_layer)
        .try_init();
}

// ── Standard test tools ──

/// A tool that echoes back its `text` input field.
pub struct EchoTool;

#[async_trait::async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str { "echo" }
    fn description(&self) -> &str { "Echo tool" }
    fn parameters_schema(&self) -> Value {
        json!({"type":"object","properties":{"text":{"type":"string"}},"required":["text"]})
    }
    async fn execute(&self, input: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let text = input.get("text").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        Ok(ToolOutput { text, is_error: false, json: None })
    }
}

/// A tool that returns a fixed response.
pub struct StaticTool {
    name: &'static str,
    description: &'static str,
    text: String,
}

impl StaticTool {
    pub fn new(name: &'static str, text: impl Into<String>) -> Self {
        Self { name, description: name, text: text.into() }
    }
}

#[async_trait::async_trait]
impl Tool for StaticTool {
    fn name(&self) -> &str { self.name }
    fn description(&self) -> &str { self.description }
    fn parameters_schema(&self) -> Value {
        json!({"type":"object","properties":{}})
    }
    async fn execute(&self, _input: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput { text: self.text.clone(), is_error: false, json: None })
    }
}

/// Helper: build a simple "text-done" script for MockProvider.
pub fn text_done_script(text: &str) -> Vec<StreamEvent> {
    vec![
        StreamEvent::TextDelta { text: text.to_string() },
        StreamEvent::MessageStop { stop_reason: None },
    ]
}

/// Push two scripts to the provider: a tool-use turn then a text-done turn.
///
/// Agent loops call `complete()` once per model turn, so we need one
/// [`push_script`] per API invocation.
pub fn push_tool_then_text(
    provider: &MockProvider,
    call_id: &str,
    tool_name: &str,
    input: Value,
    text: &str,
) {
    // Turn 1: model decides to call a tool
    provider.push_script(vec![
        StreamEvent::ToolUse { id: call_id.into(), name: tool_name.into(), input },
        StreamEvent::MessageStop { stop_reason: None },
    ]);
    // Turn 2: model responds after seeing the tool result
    provider.push_script(vec![
        StreamEvent::TextDelta { text: text.to_string() },
        StreamEvent::MessageStop { stop_reason: None },
    ]);
}

/// Helper: drain the event channel into a Vec.
pub async fn drain_events(rx: &mut tokio::sync::mpsc::Receiver<AgentEvent>) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    for _ in 0..256 {
        match tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await {
            Ok(Some(ev)) => events.push(ev),
            _ => break,
        }
    }
    events
}
