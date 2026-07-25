/// tool_routing — demonstrates the unified tool routing pipeline end-to-end:
/// Externalize → ArtifactStore → artifact_read → Metrics → Subagent delegation.
///
/// Covers:
/// - `RoutingPolicyConfig` — size-based and context-pressure-based routing
/// - `ArtifactStoreConfig` — quota, TTL, eviction
/// - `ToolResultRouting::Externalize` — large outputs stored as artifacts
/// - `artifact_read` tool — paged read-back of externalized data
/// - `RoutingDecision` / `ArtifactStored` / `ArtifactRead` audit events
/// - `GovernanceMetrics` — aggregated routing statistics
///
/// Uses MockProvider — no real LLM credentials needed.
use fox_agent_core::{
    ArtifactStoreConfig, ArtifactEvictionPolicy, ArtifactCompression,
    RoutingPolicyConfig, Tool, ToolContext, ToolError, ToolOutput,
    ToolExecutionMode, DefaultSafetyPolicy, SafetyConfig,
};
use fox_agent_sdk::{
    AgentBuilder, AgentEvent, FoxAgentSdkConfig, MockProvider, StreamEvent,
    TurnOutcome,
};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

/// A tool that returns very large output — guaranteed to trigger externalization.
struct LargeEchoTool {
    text: String,
}

#[async_trait::async_trait]
impl Tool for LargeEchoTool {
    fn name(&self) -> &str { "large_echo" }

    fn description(&self) -> &str {
        "Returns a very large text payload that simulates a full-repo search result."
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }

    async fn execute(
        &self,
        _input: Value,
        _ctx: ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput {
            text: self.text.clone(),
            is_error: false,
            json: None,
        })
    }
}

#[tokio::main]
async fn main() {
    println!("=== Tool Routing Pipeline Demo ===\n");

    // ── 1. Configure aggressive routing: 200-char threshold triggers externalization ──
    let cfg = FoxAgentSdkConfig {
        artifact_store: ArtifactStoreConfig {
            enabled: true,
            ephemeral_ttl_hours: 1,
            max_artifact_bytes: 10 * 1024 * 1024,
            max_session_bytes: 100 * 1024 * 1024,
            compression: ArtifactCompression::None,
            eviction_policy: ArtifactEvictionPolicy::Lru,
            gc_on_startup: false,
            gc_on_session_end: false,
            gc_after_write: false,
            ..Default::default()
        },
        routing_policy: RoutingPolicyConfig {
            enabled: true,
            // Low threshold — even ~300 chars triggers externalization
            local_externalize_threshold_chars: 200,
            local_delegate_threshold_chars: 50_000, // avoid accidental delegation
            context_pressure_threshold: 0.50,
            delegate_candidate_tools: vec![], // only demonstrate externalization
        },
        safety: SafetyConfig {
            default_policy: DefaultSafetyPolicy::Allow,
            ..Default::default()
        },
        ..Default::default()
    };

    // ── 2. Build agent with MockProvider ──
    let provider = Arc::new(MockProvider::new("mock"));

    // Turn 1: agent calls large_echo → triggers externalization
    provider.push_script(vec![
        StreamEvent::ToolUse {
            id: "call_1".into(),
            name: "large_echo".into(),
            input: json!({}),
        },
        StreamEvent::MessageStop { stop_reason: None },
    ]);

    // Turn 2: agent receives externalized summary and produces final answer
    provider.push_script(vec![
        StreamEvent::TextDelta {
            text: "I have retrieved the externalized search results from the artifact store.".into(),
        },
        StreamEvent::MessageStop { stop_reason: None },
    ]);

    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let agent = AgentBuilder::new()
        .working_dir(&project_root)
        .sdk_config(cfg)
        .with_global_agents_md_path(project_root.join("AGENTS.md"))
        .with_provider(provider.clone())
        .model_id("mock-1")
        .build()
        .await
        .expect("build agent");

    // Register the large-output tool
    let large_text = "LINE_".repeat(500); // ~2500 chars, well above 200-char threshold
    agent
        .harness()
        .register_tool(Arc::new(LargeEchoTool {
            text: large_text.clone(),
        }))
        .await;

    let session_id = agent.harness().session_state.read().await.id.clone();
    println!("Session: {session_id}\n");

    // ── 3. Turn 1: agent calls large_echo → observe externalization ──
    println!("═══ Turn 1: agent calls `large_echo` ═══\n");

    let (tx, mut rx) = tokio::sync::mpsc::channel::<AgentEvent>(128);
    let tx_ref = &tx;

    let outcome = agent
        .run_once_streaming(
            "Please call the `large_echo` tool to fetch search data.",
            tx_ref,
        )
        .await
        .expect("run turn 1");

    // Drain all events
    drop(tx); // close sender so rx.recv() returns None
    let mut artifact_id: Option<String> = None;
    while let Some(ev) = rx.recv().await {
        match &ev {
            AgentEvent::ToolCallStart { name, call_id, .. } => {
                println!("  [tool-start] {name} (id={call_id})");
            }
            AgentEvent::ToolCallEnd {
                call_id, output, ..
            } => {
                let preview = if output.text.len() > 120 {
                    format!("{}...", &output.text[..117])
                } else {
                    output.text.clone()
                };
                println!(
                    "  [tool-end]   (id={call_id}) — {} chars\n    → {preview}",
                    output.text.len()
                );
            }
            AgentEvent::RoutingDecision {
                tool_name,
                routing,
                context_pressure,
                output_size,
                reason,
                ..
            } => {
                println!(
                    "  [routing]    {tool_name} → {routing:?} | pressure={context_pressure:.2} | size={output_size} | reason={reason:?}",
                );
            }
            AgentEvent::ArtifactStored {
                tool_name,
                artifact_type,
                retention_class,
                ..
            } => {
                println!("  [artifact]   {tool_name} stored ({artifact_type:?}, {retention_class:?})");
            }
            _ => {}
        }
        // Capture artifact_id from stored event
        if let AgentEvent::ArtifactStored { artifact_id: id, .. } = &ev {
            artifact_id = Some(id.clone());
        }
    }

    match outcome {
        TurnOutcome::Completed { text } => println!("\n  [agent] {text}"),
        _ => println!("\n  [agent] (turn ended without text)"),
    }

    // ── 4. Verify: artifact is stored and readable ──
    let aid = artifact_id
        .as_ref()
        .expect("artifact_id should be set after externalization");
    println!("\n═══ Verification: read artifact {aid} via ArtifactStore ═══\n");

    let stored_text = agent
        .harness()
        .artifact_store
        .get_text(aid)
        .await
        .expect("artifact should be readable")
        .expect("artifact content should not be empty");
    println!(
        "  Stored text: {} chars (first 80: {}...)",
        stored_text.len(),
        &stored_text[..80.min(stored_text.len())]
    );
    assert_eq!(stored_text, large_text, "stored text should match original");

    // ── 5. Exercise artifact_read tool directly ──
    println!("\n═══ Direct artifact_read (offset=100, limit=60) ═══\n");

    let read_result = agent
        .harness()
        .tool_executor
        .execute_tool(
            "artifact_read",
            json!({"artifact_id": aid, "offset_chars": 100, "limit_chars": 60}),
            ToolContext {
                session_id: "demo".into(),
                message_id: "demo_msg".into(),
                tool_call_id: "demo_call".into(),
                working_dir: None,
                execution_mode: ToolExecutionMode::Foreground,
                graceful_shutdown_requested: false,
                progress_tx: None,
            },
        )
        .await
        .expect("artifact_read should succeed");
    println!(
        "  Read back: {} chars\n    → {}",
        read_result.text.len(),
        &read_result.text[..60.min(read_result.text.len())]
    );

    // ── 6. GovernanceMetrics summary ──
    println!("\n══════ Governance Metrics ══════\n");

    let snapshot = agent.harness().governance_metrics.snapshot();
    println!("{}", snapshot.format_summary());
    assert!(
        snapshot.externalize_count > 0,
        "at least one tool should have been externalized"
    );

    println!("═══ Demo complete ═══");
}
