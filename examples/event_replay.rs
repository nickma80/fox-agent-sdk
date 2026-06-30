/// event_replay — demonstrates Event Recording, JSONL export with
/// automatic secret scrubbing, and replay-based assertion verification.
///
/// Covers:
/// - `EventRecorder` — capture agent events per turn
/// - JSONL export with `mask_event_payload` auto-scrubbing
/// - Replay assertions (text contains check, no-error check)
/// - `EventRecorder::export_to_file()` — persist for offline analysis
///
/// Uses MockProvider — no real LLM credentials needed.
use fox_agent_sdk::{
    AgentBuilder, AgentEvent, EventRecorder, MockProvider, StreamEvent, TurnOutcome,
};
use std::sync::Arc;

#[tokio::main]
async fn main() {
    println!("=== Event Replay Demo ===\n");

    // ── 1. Build agent with MockProvider ──
    let provider = Arc::new(MockProvider::new("mock"));

    provider.push_script(vec![
        StreamEvent::TextDelta {
            text: "42 is the answer. (answer from model A)".into(),
        },
        StreamEvent::MessageStop { stop_reason: None },
    ]);

    let mut agent = AgentBuilder::new()
        .with_provider(provider.clone())
        .model_id("mock-1")
        .build()
        .await
        .expect("build agent");

    // ── 2. Set up EventRecorder ──
    // Use spawn_blocking because record() uses std::sync::RwLock internally
    let recorder = Arc::new(EventRecorder::new("replay-demo-session", 0));

    // ── 3. Run agent and collect events ──
    let (tx, mut rx) = tokio::sync::mpsc::channel::<AgentEvent>(32);

    // Spawn agent runner; collect events in main task
    let handle = tokio::spawn(async move {
        agent.run_once_streaming("什么是生命的答案？", &tx).await.unwrap()
    });

    let mut events: Vec<AgentEvent> = Vec::new();
    while let Some(ev) = rx.recv().await {
        events.push(ev);
    }

    let outcome = handle.await.unwrap();

    // record() uses blocking RwLock — must go through spawn_blocking
    let rec = recorder.clone();
    tokio::task::spawn_blocking(move || {
        for ev in &events {
            rec.record("agent", ev.into());
        }
    })
    .await
    .unwrap();

    match outcome {
        TurnOutcome::Completed { text } => {
            println!("[agent] {text}");
            assert!(text.contains("42"));
        }
        _ => panic!("expected Completed"),
    }

    // ── 4. Export events to JSONL (with auto scrubbing) ──
    let output_path = std::env::temp_dir().join("event_replay_demo.jsonl");
    recorder.export_to_file(&output_path).await.unwrap();

    let recorded = recorder.buffer().await;
    println!(
        "\n> Exported {} events to {}",
        recorded.len(),
        output_path.display()
    );

    // ── 5. Replay: inspect replayed events with assertions ──
    for env in &recorded {
        print!("  [seq={} source={}] ", env.seq, env.source);
        match &env.event {
            fox_agent_sdk::EnvelopePayload::ModelTextDelta { text } => {
                println!("text: {text}");
            }
            fox_agent_sdk::EnvelopePayload::ModelUsage { usage } => {
                println!("usage: {} tokens", usage.total_tokens);
            }
            fox_agent_sdk::EnvelopePayload::ToolCallStart { name, .. } => {
                println!("tool start: {name}");
            }
            fox_agent_sdk::EnvelopePayload::ToolCallEnd { output, .. } => {
                println!("tool end: {}", output.text);
            }
            fox_agent_sdk::EnvelopePayload::Error { message, .. } => {
                println!("error: {message}");
            }
            other => {
                println!("{:?}", other);
            }
        }
    }

    // ── 6. Run replay assertions ──
    // Check: at least one event contains "42"
    let has_answer = recorded.iter().any(|e| match &e.event {
        fox_agent_sdk::EnvelopePayload::ModelTextDelta { text } => text.contains("42"),
        _ => false,
    });
    assert!(has_answer, "Replay assertion failed: no event contains '42'");
    println!("\n> Replay assertion passed: found '42' in event stream");

    // Check: no error events
    let no_errors = !recorded.iter().any(|e| matches!(&e.event, fox_agent_sdk::EnvelopePayload::Error { .. }));
    assert!(no_errors, "Replay assertion failed: found error events");
    println!("> Replay assertion passed: no errors in event stream");

    // ── 7. Secret scrubbing demo ──
    // Simulate an event that contains a secret, verify it's scrubbed on export
    let scrubbed_line = fox_agent_sdk::mask_event_payload(
        r#"{"text":"API key is sk-abc123def456 and JWT is eyJhbGciOiJIUzI1NiJ9.abc.def"}"#
    );
    println!("\n> Secret scrubbing demo:");
    println!("  input:  API key is sk-abc123def456 and JWT is eyJhbGciOiJIUzI1NiJ9.abc.def");
    println!("  output: {scrubbed_line}");
    assert!(scrubbed_line.contains("[API_KEY]"));
    assert!(scrubbed_line.contains("[JWT]"));

    println!("\n=== PASSED ===");
}
