//! Demonstrates exporting agent telemetry to any OTel backend via OTLP/HTTP
//! following the **OTel GenAI** semantic conventions (`gen_ai.*`) plus
//! `fox_agent.*` extensions.
//!
//! Wiring is low-intrusion: the `AgentEvent` stream is fanned out so the UI
//! consumer and the export pipeline each get their own receiver; the agent's
//! main flow is untouched. When `[observability].enabled = false` the exporter
//! is never constructed and the SDK incurs zero overhead.
//!
//! Requires `[observability]` in `agent.toml`:
//! ```toml
//! [observability]
//! enabled = true
//! endpoint = "http://127.0.0.1:4318"   # e.g. Jaeger / OTel Collector
//! ```

use std::path::PathBuf;

use fox_agent_core::{AgentEvent, FoxAgentSdkConfig, ProviderConfig, TurnOutcome};
use fox_agent_sdk::AgentBuilder;
use fox_agent_sdk::otel_genai::{ExportIdentity, OtelGenAiExporter, fanout};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let sdk_config = FoxAgentSdkConfig::load_from_file(project_root.join("agent.toml"))
        .unwrap_or_else(|_| FoxAgentSdkConfig::default());

    // ── Build the agent (same as simple_agent.rs) ──
    let api_key = std::env::var("DEEPSEEK_API_KEY").unwrap_or_else(|_| "demo-key".into());
    let agent = AgentBuilder::new()
        .working_dir(&project_root)
        .sdk_config(sdk_config.clone())
        .provider_config(ProviderConfig::deepseek(api_key))
        .model_id("deepseek-v4-flash")
        .with_default_tools()
        .build()
        .await?;

    let session_id = agent.harness().session_state.read().await.id.clone();
    let model = agent.model();
    let provider_name = model.provider_name().to_string();
    let model_id = model.model_id();

    // ── Event stream: fan out to (UI consumer, OTel exporter) ──
    let (tx, rx) = tokio::sync::mpsc::channel::<AgentEvent>(64);
    let (ui_rx, export_rx) = fanout(rx);

    // ── Export pipeline (no-op when [observability].enabled = false) ──
    let export_handle = match OtelGenAiExporter::from_sdk_config(
        &sdk_config,
        ExportIdentity {
            provider_name,
            model_id,
            session_id,
        },
    ) {
        Ok(exporter) => {
            println!("observability: exporting OTel GenAI spans to OTLP endpoint");
            Some(exporter.spawn(export_rx))
        }
        Err(err) => {
            println!("observability disabled: {err}");
            None
        }
    };

    // ── UI consumer (minimal display of the agent's live stream) ──
    let ui_handle = tokio::spawn(async move {
        let mut rx = ui_rx;
        while let Some(ev) = rx.recv().await {
            match ev {
                AgentEvent::ModelTextDelta { text } => {
                    print!("{text}");
                }
                AgentEvent::ModelThinkingDelta { text } => {
                    print!("\x1b[90m{text}\x1b[0m");
                }
                AgentEvent::ToolCallStart { name, .. } => {
                    println!("\n\n[⚡ Tool: {name}]");
                }
                AgentEvent::ToolCallEnd { output, .. } => {
                    let preview: String = output.text.chars().take(200).collect();
                    println!("  result: {preview} (error: {})", output.is_error);
                }
                AgentEvent::Error { error } => {
                    eprintln!("\n[ERROR] {error}");
                }
                _ => {}
            }
        }
    });

    // ── Run the agent; the exporter task flushes on batch/timer ──
    let prompt = std::env::args().nth(1).unwrap_or_else(|| {
        "What is the weather like in Shanghai? Look it up with a tool call.".to_string()
    });
    println!("[user] {prompt}\n");

    let result = agent.run_once_streaming(&prompt, &tx).await;
    drop(tx); // close the stream → exporter drains events and final-flushes

    if let Some(handle) = export_handle {
        let _ = handle.await;
    }
    let _ = ui_handle.await;

    match result {
        Ok(TurnOutcome::Completed { text }) => println!("\n\n[agent] {text}"),
        Ok(other) => println!("\n\n[agent] turn ended: {other:?}"),
        Err(err) => eprintln!("\n[ERROR] {err}"),
    }
    Ok(())
}
