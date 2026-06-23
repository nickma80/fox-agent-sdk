#[cfg(test)]
mod sdk_tests {
    use crate::*;
    use fox_agent_core::{
        AgentEvent, AgentError, CompactionConfig, DefaultModel, DefaultSafetyPolicy,
        FilePlanningStore, FileSessionStore, FoxAgentSdkConfig, MemoryConfig, MemoryStateEvent,
        Message, Model, PermissionDecision, PermissionRequest, PermissionResult, PlanningStore,
        PlanStatus, PlanPriority, SafetyConfig, SessionStore, StreamEvent, TokenUsage, Tool, ToolContext, ToolError,
        ToolOutput, TurnOutcome, ErrorKind,
    };
    use fox_agent_providers::MockProvider;
    use fox_agent_tools::{TodoItem, TodoStatus, TodoPriority, PlanItem, save_todos, save_plan};
    use serde_json::{json, Value};
    use std::sync::Arc;

    struct EchoTool;

    #[async_trait::async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str { "echo" }
        fn description(&self) -> &str { "echo text" }
        fn parameters_schema(&self) -> Value {
            json!({"type":"object","properties":{"text":{"type":"string"}},"required":["text"]})
        }
        async fn execute(&self, input: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
            let text = input.get("text").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            Ok(ToolOutput { text, is_error: false, json: None })
        }
    }

    #[tokio::test]
    async fn tool_call_then_text_completes() {
        let provider = MockProvider::new("mock");
        provider.push_script(vec![
            StreamEvent::ToolUse { id: "c1".into(), name: "echo".into(), input: json!({"text":"hi"}) },
            StreamEvent::MessageStop { stop_reason: None },
        ]);
        provider.push_script(vec![
            StreamEvent::TextDelta { text: "done".into() },
            StreamEvent::MessageStop { stop_reason: None },
        ]);

        let model: Arc<dyn Model> = Arc::new(DefaultModel::new(Arc::new(provider), "mock-1"));
        let harness = Harness::new(FoxAgentSdkConfig::default(), None);
        harness.register_tool(Arc::new(EchoTool)).await;
        let mut agent = Agent::new(model, harness);

        let (tx, mut rx) = tokio::sync::mpsc::channel(32);
        let outcome = agent.run_once_streaming("go", &tx).await.unwrap();

        let mut saw_tool_start = false;
        let mut saw_tool_end = false;
        for _ in 0..16 {
            let ev = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await.ok().flatten();
            let Some(ev) = ev else { break };
            match ev {
                AgentEvent::ToolCallStart { .. } => saw_tool_start = true,
                AgentEvent::ToolCallEnd { .. } => saw_tool_end = true,
                _ => {}
            }
            if saw_tool_start && saw_tool_end { break; }
        }

        assert!(saw_tool_start);
        assert!(saw_tool_end);
        match outcome {
            TurnOutcome::Completed { text } => assert_eq!(text, "done"),
            _ => panic!("expected Completed"),
        }
    }

    #[tokio::test]
    async fn ask_user_then_resume_allows() {
        let provider = MockProvider::new("mock");
        provider.push_script(vec![
            StreamEvent::ToolUse { id: "c1".into(), name: "echo".into(), input: json!({"text":"hi"}) },
            StreamEvent::MessageStop { stop_reason: None },
        ]);
        provider.push_script(vec![
            StreamEvent::TextDelta { text: "ok".into() },
            StreamEvent::MessageStop { stop_reason: None },
        ]);

        let model: Arc<dyn Model> = Arc::new(DefaultModel::new(Arc::new(provider), "mock-1"));
        let harness = Harness::with_permission_hook(
            FoxAgentSdkConfig { safety: SafetyConfig { default_policy: DefaultSafetyPolicy::Allow, ..Default::default() }, ..Default::default() },
            None,
            |tool_name, _input| {
                if tool_name == "echo" {
                    PermissionResult::AskUser { request: PermissionRequest::new("echo", "allow echo?") }
                } else { PermissionResult::Allow }
            },
        );
        harness.register_tool(Arc::new(EchoTool)).await;

        let mut agent = Agent::new(model, harness);
        let (tx, mut rx) = tokio::sync::mpsc::channel(32);
        let outcome = agent.run_once_streaming("go", &tx).await.unwrap();
        let req = match outcome {
            TurnOutcome::RequiresUserDecision { request } => request,
            _ => panic!("expected RequiresUserDecision"),
        };
        assert_eq!(req.tool_name, "echo");

        while let Ok(ev) = tokio::time::timeout(std::time::Duration::from_millis(10), rx.recv()).await {
            if ev.is_none() { break; }
        }

        let outcome = agent.resume_streaming(PermissionDecision::Allow, &tx).await.unwrap();
        match outcome {
            TurnOutcome::Completed { text } => assert_eq!(text, "ok"),
            _ => panic!("expected Completed"),
        }
    }

    #[tokio::test]
    async fn phase3_emits_turn_events_and_compaction() {
        let provider = MockProvider::new("mock");
        provider.push_script(vec![
            StreamEvent::TextDelta { text: "after compact".into() },
            StreamEvent::MessageStop { stop_reason: None },
        ]);

        let model: Arc<dyn Model> = Arc::new(DefaultModel::new(Arc::new(provider), "mock-1"));
        let mut harness = Harness::new(FoxAgentSdkConfig {
            compaction: CompactionConfig { enabled: true, token_budget: 10, preserve_recent_messages: 2, max_turns_before_compaction: 100, ..Default::default() },
            ..Default::default()
        }, None);
        harness.session_state.messages.extend([
            Message::user("this is a very long old message"),
            Message::assistant("this is a very long assistant answer"),
            Message::user("keep me"),
        ]);
        let mut agent = Agent::new(model, harness);

        let (tx, mut rx) = tokio::sync::mpsc::channel(32);
        let outcome = agent.run_once_streaming("go", &tx).await.unwrap();

        let mut saw_turn_start = false;
        let mut saw_turn_end = false;
        let mut saw_compaction = false;
        for _ in 0..16 {
            let ev = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await.ok().flatten();
            let Some(ev) = ev else { break };
            match ev {
                AgentEvent::TurnStart { .. } => saw_turn_start = true,
                AgentEvent::TurnEnd { .. } => saw_turn_end = true,
                AgentEvent::Compaction { .. } => saw_compaction = true,
                _ => {}
            }
            if saw_turn_start && saw_turn_end && saw_compaction { break; }
        }

        assert!(saw_turn_start);
        assert!(saw_turn_end);
        assert!(saw_compaction);
        match outcome {
            TurnOutcome::Completed { text } => assert_eq!(text, "after compact"),
            _ => panic!("expected Completed"),
        }
    }

    #[tokio::test]
    async fn phase3_soft_interrupt_is_emitted() {
        let provider = MockProvider::new("mock");
        provider.push_script(vec![
            StreamEvent::TextDelta { text: "ok".into() },
            StreamEvent::MessageStop { stop_reason: None },
        ]);

        let model: Arc<dyn Model> = Arc::new(DefaultModel::new(Arc::new(provider), "mock-1"));
        let harness = Harness::new(FoxAgentSdkConfig::default(), None);
        let mut agent = Agent::new(model, harness);
        agent.harness().queue_soft_interrupt("please reconsider", true).await;

        let (tx, mut rx) = tokio::sync::mpsc::channel(32);
        let _ = agent.run_once_streaming("go", &tx).await.unwrap();

        let mut saw_interrupt = false;
        for _ in 0..16 {
            let ev = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await.ok().flatten();
            let Some(ev) = ev else { break };
            if matches!(ev, AgentEvent::SoftInterruptInjected { .. }) { saw_interrupt = true; break; }
        }
        assert!(saw_interrupt);
    }

    #[tokio::test]
    async fn phase3_memory_pipeline_emits_state_events() {
        let provider = MockProvider::new("mock");
        provider.push_script(vec![
            StreamEvent::TextDelta { text: "done".into() },
            StreamEvent::MessageStop { stop_reason: None },
        ]);

        let model: Arc<dyn Model> = Arc::new(DefaultModel::new(Arc::new(provider), "mock-1"));
        let harness = Harness::new(FoxAgentSdkConfig {
            memory: MemoryConfig { enabled: true, max_candidates: 10, max_results: 3, max_graph_depth: 1, verify_relevance: false, ..Default::default() },
            ..Default::default()
        }, None);
        harness.memory_manager.add_memory("user likes rust").await;
        let mut agent = Agent::new(model, harness);

        let (tx, mut rx) = tokio::sync::mpsc::channel(32);
        let _ = agent.run_once_streaming("rust", &tx).await.unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        let provider = MockProvider::new("mock");
        provider.push_script(vec![
            StreamEvent::TextDelta { text: "second".into() },
            StreamEvent::MessageStop { stop_reason: None },
        ]);
        agent.model = Arc::new(DefaultModel::new(Arc::new(provider), "mock-1"));
        let _ = agent.run_once_streaming("continue rust", &tx).await.unwrap();

        let mut saw_consumed = false;
        for _ in 0..32 {
            let ev = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await.ok().flatten();
            let Some(ev) = ev else { break };
            if let AgentEvent::MemoryStateChanged { event } = ev {
                if matches!(event, MemoryStateEvent::InjectionConsumed { .. }) { saw_consumed = true; break; }
            }
        }
        assert!(saw_consumed);
    }

    #[tokio::test]
    async fn phase3_auto_extract_emits_ingestion_event_and_persists_memory() {
        let provider = MockProvider::new("mock");
        provider.push_script(vec![
            StreamEvent::TextDelta { text: "Done, I will keep answers concise.".into() },
            StreamEvent::MessageStop { stop_reason: None },
        ]);
        provider.push_script(vec![
            StreamEvent::TextDelta { text: "preference|User prefers concise rust answers|high".into() },
            StreamEvent::MessageStop { stop_reason: None },
        ]);

        let model: Arc<dyn Model> = Arc::new(DefaultModel::new(Arc::new(provider), "mock-1"));
        let harness = Harness::new(FoxAgentSdkConfig {
            memory: MemoryConfig {
                enabled: true,
                auto_extract: true,
                auto_extract_scope: fox_agent_core::AutoExtractScope::Global,
                auto_extract_message_window: 4,
                verify_relevance: false,
                ..Default::default()
            },
            ..Default::default()
        }, None);
        let mut agent = Agent::new(model, harness);

        let (tx, mut rx) = tokio::sync::mpsc::channel(32);
        let _ = agent.run_once_streaming("Please keep rust answers concise", &tx).await.unwrap();

        let mut saw_ingestion = false;
        for _ in 0..24 {
            let ev = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await.ok().flatten();
            let Some(ev) = ev else { break };
            if let AgentEvent::MemoryStateChanged { event } = ev {
                if let MemoryStateEvent::IngestionCompleted { created_ids, .. } = event {
                    saw_ingestion = !created_ids.is_empty();
                    if saw_ingestion { break; }
                }
            }
        }
        assert!(saw_ingestion);

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let stored = agent
            .harness
            .memory_manager
            .core()
            .list(fox_agent_core::MemoryScope::Global)
            .unwrap();
        assert!(stored.iter().any(|entry| entry.content.contains("concise rust answers")));
    }

    #[tokio::test]
    async fn phase4_prompt_builder_includes_planning_context() {
        let session_id = format!("phase4-{}", uuid::Uuid::new_v4());
        let _ = save_todos(&session_id, vec![TodoItem {
            id: "t1".into(), content: "implement phase4".into(), status: TodoStatus::InProgress, priority: TodoPriority::High,
        }], false);
        let _ = save_plan(&session_id, vec![PlanItem {
            id: "p1".into(), content: "spawn worker".into(), status: PlanStatus::Pending, priority: PlanPriority::High,
            assigned_to: None, blocked_by: vec![],
        }], false);

        let builder = crate::prompt_builder::PromptBuilder::new("1.0.0", "abc123");
        let planning_store: Arc<dyn PlanningStore> = Arc::new(FilePlanningStore::new(
            std::env::temp_dir().join(format!("fox-sdk-planning-{}", uuid::Uuid::new_v4())),
        ));
        let (split, _) = builder.build_split(&session_id, &planning_store, None, &[], None, None);
        assert!(split.dynamic_part.contains("implement phase4"));
        // The planning context is routed to dynamic_part, check that
        assert!(split.dynamic_part.contains("implement phase4"), "dynamic part should contain todo items: {}", split.dynamic_part);
        assert!(split.dynamic_part.contains("spawn worker"));
    }

    #[tokio::test]
    async fn m1_auto_snapshot_persists_and_restores_session_state() {
        let provider = MockProvider::new("mock");
        provider.push_script(vec![
            StreamEvent::TextDelta { text: "snapshot restored".into() },
            StreamEvent::MessageStop { stop_reason: None },
        ]);

        let working_dir = std::env::temp_dir().join(format!("fox-sdk-m1-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&working_dir).await.unwrap();
        let session_root = working_dir.join("session-store");
        let planning_root = working_dir.join("planning-store");

        let model: Arc<dyn Model> = Arc::new(DefaultModel::new(Arc::new(provider), "mock-1"));
        let harness = Harness::new(
            FoxAgentSdkConfig {
                session_storage_dir: Some(session_root.clone()),
                planning_storage_dir: Some(planning_root.clone()),
                auto_snapshot: true,
                ..Default::default()
            },
            Some(working_dir.clone()),
        );
        let mut agent = Agent::new(model.clone(), harness);
        let session_id = agent.harness().session_state.id.clone();

        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        let outcome = agent.run_once_streaming("persist this session", &tx).await.unwrap();
        assert!(matches!(outcome, TurnOutcome::Completed { .. }));

        let stored = agent
            .harness()
            .session_store
            .load_session(&session_id)
            .unwrap()
            .expect("session snapshot should exist");
        assert_eq!(stored.session_id, session_id);
        assert!(stored.messages.len() >= 2);

        let restore_harness = Harness::new(
            FoxAgentSdkConfig {
                session_storage_dir: Some(session_root),
                planning_storage_dir: Some(planning_root),
                auto_snapshot: true,
                ..Default::default()
            },
            Some(working_dir.clone()),
        );
        let restored = Agent::load_from_store(model, restore_harness, &session_id)
            .unwrap()
            .expect("agent should restore from snapshot");
        assert_eq!(restored.harness().session_state.id, session_id);
        assert!(restored.harness().session_state.messages.len() >= 2);

        let _ = tokio::fs::remove_dir_all(&working_dir).await;
    }

    #[tokio::test]
    async fn phase3_emits_model_message_lifecycle_and_usage() {
        let provider = MockProvider::new("mock");
        provider.push_script(vec![
            StreamEvent::TextDelta { text: "hello".into() },
            StreamEvent::Usage { usage: TokenUsage { input_tokens: 11, output_tokens: 4, total_tokens: 15, cache_read_input_tokens: None, cache_creation_input_tokens: None } },
            StreamEvent::MessageStop { stop_reason: None },
        ]);

        let model: Arc<dyn Model> = Arc::new(DefaultModel::new(Arc::new(provider), "mock-1"));
        let harness = Harness::new(FoxAgentSdkConfig::default(), None);
        let mut agent = Agent::new(model, harness);

        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let outcome = agent.run_once_streaming("go", &tx).await.unwrap();
        assert!(matches!(outcome, TurnOutcome::Completed { .. }));

        let mut saw_start = false;
        let mut saw_usage = false;
        let mut saw_end = false;
        for _ in 0..12 {
            let ev = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await.ok().flatten();
            let Some(ev) = ev else { break };
            match ev {
                AgentEvent::ModelMessageStart { .. } => saw_start = true,
                AgentEvent::ModelUsage { usage } => { saw_usage = usage.total_tokens == 15; }
                AgentEvent::ModelMessageEnd { .. } => saw_end = true,
                _ => {}
            }
            if saw_start && saw_usage && saw_end { break; }
        }
        assert!(saw_start);
        assert!(saw_usage);
        assert!(saw_end);
    }

    #[tokio::test]
    async fn phase3_graceful_shutdown_cancels_turn() {
        let provider = MockProvider::new("mock");
        let model: Arc<dyn Model> = Arc::new(DefaultModel::new(Arc::new(provider), "mock-1"));
        let harness = Harness::new(FoxAgentSdkConfig::default(), None);
        let mut agent = Agent::new(model, harness);
        agent.harness().request_graceful_shutdown().await;

        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let outcome = agent.run_once_streaming("go", &tx).await.unwrap();
        assert!(matches!(outcome, TurnOutcome::Cancelled));

        let mut saw_cancel_error = false;
        for _ in 0..8 {
            let ev = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await.ok().flatten();
            let Some(ev) = ev else { break };
            if let AgentEvent::Error { error } = ev {
                if error.kind() == ErrorKind::Internal { saw_cancel_error = true; break; }
            }
        }
        assert!(saw_cancel_error);
    }

    #[tokio::test]
    async fn phase3_tool_error_emits_structured_error_event() {
        let provider = MockProvider::new("mock");
        provider.push_script(vec![
            StreamEvent::ToolUse { id: "missing-call".into(), name: "missing_tool".into(), input: json!({}) },
            StreamEvent::MessageStop { stop_reason: None },
        ]);

        let model: Arc<dyn Model> = Arc::new(DefaultModel::new(Arc::new(provider), "mock-1"));
        let harness = Harness::new(FoxAgentSdkConfig::default(), None);
        let mut agent = Agent::new(model, harness);

        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let err = agent.run_once_streaming("go", &tx).await.unwrap_err();
        assert!(matches!(err, AgentError::Tool(_)));

        let mut saw_tool_error = false;
        for _ in 0..8 {
            let ev = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await.ok().flatten();
            let Some(ev) = ev else { break };
            if let AgentEvent::Error { error } = ev {
                if error.kind() == ErrorKind::Tool { saw_tool_error = true; break; }
            }
        }
        assert!(saw_tool_error);
    }

    // ── M2: AgentBuilder tests ──

    /// AgentBuilder::build() with MockProvider and default tools.
    #[tokio::test]
    async fn m2_builder_build_with_mock_provider_produces_agent() {
        let provider = Arc::new(MockProvider::new("mock"));
        provider.push_script(vec![
            StreamEvent::TextDelta { text: "ok".into() },
            StreamEvent::MessageStop { stop_reason: None },
        ]);

        let mut agent = AgentBuilder::new()
            .with_provider(provider)
            .model_id("mock-1")
            .with_default_tools()
            .build()
            .await
            .expect("build should succeed");

        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        let outcome = agent.run_once_streaming("go", &tx).await.unwrap();
        assert!(matches!(outcome, TurnOutcome::Completed { text } if text == "ok"));
    }

    /// AgentBuilder registers default tools without manual Harness wiring.
    #[tokio::test]
    async fn m2_builder_registers_default_tools() {
        let provider = Arc::new(MockProvider::new("mock"));

        let agent = AgentBuilder::new()
            .with_provider(provider)
            .model_id("mock-1")
            .with_default_tools()
            .build()
            .await
            .expect("build should succeed");

        let defs = agent.harness().tool_definitions().await;
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"read"), "expected 'read' tool; got: {names:?}");
        assert!(names.contains(&"todo"), "expected 'todo' tool; got: {names:?}");
        assert!(names.contains(&"plan"), "expected 'plan' tool; got: {names:?}");
        assert!(names.contains(&"goal"), "expected 'goal' tool; got: {names:?}");
    }

    /// AgentBuilder registers custom tool without direct Harness access.
    #[tokio::test]
    async fn m2_builder_registers_custom_tool() {
        let provider = Arc::new(MockProvider::new("mock"));

        let agent = AgentBuilder::new()
            .with_provider(provider)
            .model_id("mock-1")
            .with_tool(Arc::new(EchoTool))
            .build()
            .await
            .expect("build should succeed");

        let defs = agent.harness().tool_definitions().await;
        assert!(defs.iter().any(|d| d.name == "echo"), "missing echo tool");
    }

    /// AgentBuilder with safety denylist causes permission requests.
    #[tokio::test]
    async fn m2_builder_safety_denylist_requires_permission() {
        let provider = Arc::new(MockProvider::new("mock"));
        provider.push_script(vec![
            StreamEvent::ToolUse { id: "c1".into(), name: "echo".into(), input: json!({"text":"hi"}) },
            StreamEvent::MessageStop { stop_reason: None },
        ]);

        let mut agent = AgentBuilder::new()
            .with_provider(provider)
            .model_id("mock-1")
            .with_tool(Arc::new(EchoTool))
            .sdk_config(FoxAgentSdkConfig {
                safety: SafetyConfig {
                    default_policy: DefaultSafetyPolicy::Allow,
                    tool_denylist: Some(vec!["echo".to_string()]),
                    tool_allowlist: None,
                },
                ..Default::default()
            })
            .build()
            .await
            .expect("build should succeed");

        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        let outcome = agent.run_once_streaming("go", &tx).await.unwrap();
        assert!(matches!(outcome, TurnOutcome::RequiresUserDecision { ref request } if request.tool_name == "echo"));
    }

    /// AgentBuilder with working_dir sets the session directory.
    #[tokio::test]
    async fn m2_builder_with_working_dir() {
        let tmp = std::env::temp_dir().join(format!("fox-m2-wd-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&tmp).await.unwrap();

        let provider = Arc::new(MockProvider::new("mock"));
        let agent = AgentBuilder::new()
            .with_provider(provider)
            .model_id("mock-1")
            .working_dir(&tmp)
            .build()
            .await
            .expect("build should succeed");

        assert_eq!(
            agent.harness().session_state.working_dir.as_deref(),
            Some(tmp.as_path())
        );

        let _ = tokio::fs::remove_dir_all(&tmp).await;
    }

    /// AgentBuilder with sandbox blocks reads outside the workspace.
    #[tokio::test]
    async fn m2_builder_sandbox_blocks_outside_access() {
        let tmp = std::env::temp_dir().join(format!("fox-m2-sandbox-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&tmp).await.unwrap();

        let sandbox = fox_agent_core::WorkspaceSandbox::new(&tmp);
        let harness = Harness::new(FoxAgentSdkConfig::default(), Some(tmp.clone()));
        harness.set_sandbox(sandbox).await;

        let ctx = fox_agent_core::ToolContext {
            session_id: "test".into(),
            message_id: uuid::Uuid::new_v4().to_string(),
            tool_call_id: "c1".into(),
            working_dir: Some(tmp.clone()),
            execution_mode: fox_agent_core::ToolExecutionMode::Foreground,
            graceful_shutdown_requested: false,
        };
        // Read outside sandbox → should fail
        let result = harness
            .execute_tool("read", json!({"file_path": r"C:\Windows"}), ctx)
            .await;
        assert!(result.is_err() || result.unwrap().is_error,
            "sandbox should block reads outside the workspace");

        let _ = tokio::fs::remove_dir_all(&tmp).await;
    }

    /// SwarmRuntimeBuilder creates a usable SwarmRuntime.
    #[tokio::test]
    async fn m2_swarm_runtime_builder_creates_runtime() {
        let provider = Arc::new(MockProvider::new("mock"));
        let coordinator = Arc::new(fox_agent_swarm::SwarmCoordinator::new());

        let runtime = crate::builder::SwarmRuntimeBuilder::new()
            .with_provider(provider)
            .model_id("mock-1")
            .coordinator(coordinator)
            .build()
            .await
            .expect("swarm build should succeed");

        assert!(runtime.cfg().auto_snapshot);
    }
}
