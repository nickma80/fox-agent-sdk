/// Custom Tool — demonstrates registering and using a custom `Tool` via `AgentBuilder`.
///
/// Covers:
/// - Implementing the `Tool` trait (name, description, json schema, execute)
/// - Registering a custom tool with `AgentBuilder::with_tool()`
/// - Using `MockProvider` for deterministic testing
use fox_agent_sdk::{
    AgentBuilder, AgentEvent, FoxAgentSdkConfig, MockProvider, StreamEvent,
    Tool, ToolContext, ToolError, ToolOutput, TurnOutcome,
};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;

struct ReverseTool;

#[async_trait::async_trait]
impl Tool for ReverseTool {
    fn name(&self) -> &str {
        "reverse"
    }

    fn description(&self) -> &str {
        "Reverse a given string. Returns the reversed string."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "text": { "type": "string", "description": "The text to reverse" }
            },
            "required": ["text"]
        })
    }

    async fn execute(&self, input: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let text = input
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let rev: String = text.chars().rev().collect();
        Ok(ToolOutput {
            text: rev.clone(),
            is_error: false,
            json: Some(json!({"reversed": rev})),
        })
    }
}

#[tokio::main]
async fn main() {
    println!("=== Custom Tool Demo ===\n");

    // ── Build agent with AgentBuilder + custom tool ──
    let provider = Arc::new(MockProvider::new("mock"));

    // Script deterministic LLM responses
    provider.push_script(vec![
        StreamEvent::ToolUse {
            id: "c1".into(),
            name: "reverse".into(),
            input: json!({"text": "hi"}),
        },
        StreamEvent::MessageStop { stop_reason: None },
    ]);
    provider.push_script(vec![
        StreamEvent::TextDelta {
            text: "reversed: ih".to_string(),
        },
        StreamEvent::MessageStop { stop_reason: None },
    ]);

    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cfg = FoxAgentSdkConfig::load_from_file(project_root.join("agent.toml"))
        .unwrap_or_else(|_| FoxAgentSdkConfig::default());

    let mut agent = AgentBuilder::new()
        .working_dir(&project_root)
        .sdk_config(cfg)
        .with_global_agents_md_path(project_root.join("AGENTS.md"))
        .with_provider(provider.clone())
        .model_id("mock-1")
        .with_tool(Arc::new(ReverseTool))
        .build()
        .await
        .expect("build agent");

    // ── Run agent ──
    let (tx, mut rx) = tokio::sync::mpsc::channel::<AgentEvent>(32);
    let outcome = agent
        .run_once_streaming("reverse hi", &tx)
        .await
        .unwrap();

    let mut saw_tool = false;
    for _ in 0..16 {
        let ev = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
            .await
            .ok()
            .flatten();
        let Some(ev) = ev else { break };
        if let AgentEvent::ToolCallEnd { ref output, .. } = ev {
            if output.text == "ih" {
                saw_tool = true;
                break;
            }
        }
    }
    assert!(saw_tool);

    match outcome {
        TurnOutcome::Completed { text } => {
            println!("[agent] {text}");
            assert!(text.contains("ih"));
        }
        _ => panic!("expected Completed"),
    }
    println!("\n=== PASSED ===");
}
