#[cfg(test)]
mod sdk_tests {
    use crate::routing::{GovernanceMetrics, RoutingInput, RoutingPolicyEngine};
    use crate::*;
    use fox_agent_core::{
        AgentError, AgentEvent, ArtifactProducer, ArtifactRetentionClass, ArtifactStoreConfig,
        ArtifactType, CompactionConfig, DefaultModel, DefaultSafetyPolicy, ErrorKind, EvidenceRef,
        FilePlanningStore, FoxAgentSdkConfig, McpServerKind, McpServerProfile,
        McpToolDescriptorSnapshot, McpTransportKind, MemoryConfig, MemoryStateEvent, Message,
        Model, PermissionDecision, PermissionRequest, PermissionResult, PlanPriority, PlanStatus,
        PlanningStore, RoutingPolicyConfig, SafetyConfig, StreamEvent, SubagentOutcome,
        SubagentSummary, SubagentTask, TokenUsage, Tool, ToolContext, ToolError, ToolOutput,
        ToolResultRouting, TurnOutcome,
    };
    use fox_agent_providers::MockProvider;
    use fox_agent_tools::{PlanItem, TodoItem, TodoPriority, TodoStatus};
    use serde_json::{Value, json};
    use std::sync::Arc;

    struct EchoTool;

    struct StaticTool {
        name: &'static str,
        description: &'static str,
        text: String,
    }

    #[async_trait::async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "echo text"
        }
        fn parameters_schema(&self) -> Value {
            json!({"type":"object","properties":{"text":{"type":"string"}},"required":["text"]})
        }
        async fn execute(&self, input: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
            let text = input
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            Ok(ToolOutput {
                text,
                is_error: false,
                json: None,
            })
        }
    }

    #[async_trait::async_trait]
    impl Tool for StaticTool {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            self.description
        }
        fn parameters_schema(&self) -> Value {
            json!({"type":"object","properties":{}})
        }
        async fn execute(&self, _input: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput {
                text: self.text.clone(),
                is_error: false,
                json: None,
            })
        }
    }

    #[tokio::test]
    async fn tool_call_then_text_completes() {
        let provider = MockProvider::new("mock");
        provider.push_script(vec![
            StreamEvent::ToolUse {
                id: "c1".into(),
                name: "echo".into(),
                input: json!({"text":"hi"}),
            },
            StreamEvent::MessageStop { stop_reason: None },
        ]);
        provider.push_script(vec![
            StreamEvent::TextDelta {
                text: "done".into(),
            },
            StreamEvent::MessageStop { stop_reason: None },
        ]);

        let model: Arc<dyn Model> = Arc::new(DefaultModel::new(Arc::new(provider), "mock-1"));
        let harness = Harness::new(FoxAgentSdkConfig::default(), None);
        harness.register_tool(Arc::new(EchoTool)).await;
        let agent = Agent::new(model, harness, Arc::new(tokio::sync::RwLock::new(None)));

        let (tx, mut rx) = tokio::sync::mpsc::channel(32);
        let outcome = agent.run_once_streaming("go", &tx).await.unwrap();

        let mut saw_tool_start = false;
        let mut saw_tool_end = false;
        for _ in 0..16 {
            let ev = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
                .await
                .ok()
                .flatten();
            let Some(ev) = ev else { break };
            match ev {
                AgentEvent::ToolCallStart { .. } => saw_tool_start = true,
                AgentEvent::ToolCallEnd { .. } => saw_tool_end = true,
                _ => {}
            }
            if saw_tool_start && saw_tool_end {
                break;
            }
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
            StreamEvent::ToolUse {
                id: "c1".into(),
                name: "echo".into(),
                input: json!({"text":"hi"}),
            },
            StreamEvent::MessageStop { stop_reason: None },
        ]);
        provider.push_script(vec![
            StreamEvent::TextDelta { text: "ok".into() },
            StreamEvent::MessageStop { stop_reason: None },
        ]);

        let model: Arc<dyn Model> = Arc::new(DefaultModel::new(Arc::new(provider), "mock-1"));
        let harness = Harness::with_permission_hook(
            FoxAgentSdkConfig {
                safety: SafetyConfig {
                    default_policy: DefaultSafetyPolicy::Allow,
                    ..Default::default()
                },
                ..Default::default()
            },
            None,
            |tool_name, _input| {
                if tool_name == "echo" {
                    PermissionResult::AskUser {
                        request: PermissionRequest::new("echo", "allow echo?"),
                    }
                } else {
                    PermissionResult::Allow
                }
            },
        );
        harness.register_tool(Arc::new(EchoTool)).await;

        let agent = Agent::new(model, harness, Arc::new(tokio::sync::RwLock::new(None)));
        let (tx, mut rx) = tokio::sync::mpsc::channel(32);
        let outcome = agent.run_once_streaming("go", &tx).await.unwrap();
        let req = match outcome {
            TurnOutcome::RequiresUserDecision { request } => request,
            _ => panic!("expected RequiresUserDecision"),
        };
        assert_eq!(req.tool_name, "echo");

        while let Ok(ev) =
            tokio::time::timeout(std::time::Duration::from_millis(10), rx.recv()).await
        {
            if ev.is_none() {
                break;
            }
        }

        let outcome = agent
            .resume_streaming(PermissionDecision::Allow, &tx)
            .await
            .unwrap();
        match outcome {
            TurnOutcome::Completed { text } => assert_eq!(text, "ok"),
            _ => panic!("expected Completed"),
        }
    }

    #[tokio::test]
    async fn phase3_emits_turn_events_and_compaction() {
        let provider = MockProvider::new("mock");
        provider.push_script(vec![
            StreamEvent::TextDelta {
                text: "after compact".into(),
            },
            StreamEvent::MessageStop { stop_reason: None },
        ]);

        let model: Arc<dyn Model> = Arc::new(DefaultModel::new(Arc::new(provider), "mock-1"));
        let harness = Harness::new(
            FoxAgentSdkConfig {
                compaction: CompactionConfig {
                    enabled: true,
                    token_budget: 10,
                    preserve_recent_messages: 2,
                    max_turns_before_compaction: 100,
                    llm_summary_enabled: false,
                    ..Default::default()
                },
                ..Default::default()
            },
            None,
        );
        harness.session_state.write().await.messages.extend([
            Message::user("this is a very long old message"),
            Message::assistant("this is a very long assistant answer"),
            Message::user("keep me"),
        ]);
        let agent = Agent::new(model, harness, Arc::new(tokio::sync::RwLock::new(None)));

        let (tx, mut rx) = tokio::sync::mpsc::channel(32);
        let outcome = agent.run_once_streaming("go", &tx).await.unwrap();

        let mut saw_turn_start = false;
        let mut saw_turn_end = false;
        let mut saw_compaction = false;
        for _ in 0..16 {
            let ev = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
                .await
                .ok()
                .flatten();
            let Some(ev) = ev else { break };
            match ev {
                AgentEvent::TurnStart { .. } => saw_turn_start = true,
                AgentEvent::TurnEnd { .. } => saw_turn_end = true,
                AgentEvent::Compaction { .. } => saw_compaction = true,
                _ => {}
            }
            if saw_turn_start && saw_turn_end && saw_compaction {
                break;
            }
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
        let agent = Agent::new(model, harness, Arc::new(tokio::sync::RwLock::new(None)));
        agent
            .harness()
            .queue_soft_interrupt("please reconsider", true)
            .await;

        let (tx, mut rx) = tokio::sync::mpsc::channel(32);
        let _ = agent.run_once_streaming("go", &tx).await.unwrap();

        let mut saw_interrupt = false;
        for _ in 0..16 {
            let ev = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
                .await
                .ok()
                .flatten();
            let Some(ev) = ev else { break };
            if matches!(ev, AgentEvent::SoftInterruptInjected { .. }) {
                saw_interrupt = true;
                break;
            }
        }
        assert!(saw_interrupt);
    }

    #[tokio::test]
    async fn phase3_memory_pipeline_emits_state_events() {
        let provider = MockProvider::new("mock");
        provider.push_script(vec![
            StreamEvent::TextDelta {
                text: "done".into(),
            },
            StreamEvent::MessageStop { stop_reason: None },
        ]);

        let model: Arc<dyn Model> = Arc::new(DefaultModel::new(Arc::new(provider), "mock-1"));
        let harness = Harness::new(
            FoxAgentSdkConfig {
                memory: MemoryConfig {
                    enabled: true,
                    max_candidates: 10,
                    max_results: 3,
                    max_graph_depth: 1,
                    verify_relevance: false,
                    ..Default::default()
                },
                ..Default::default()
            },
            None,
        );
        harness.memory_manager.add_memory("user likes rust").await;
        let mut agent = Agent::new(model, harness, Arc::new(tokio::sync::RwLock::new(None)));

        let (tx, mut rx) = tokio::sync::mpsc::channel(32);
        let _ = agent.run_once_streaming("rust", &tx).await.unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        let provider = MockProvider::new("mock");
        provider.push_script(vec![
            StreamEvent::TextDelta {
                text: "second".into(),
            },
            StreamEvent::MessageStop { stop_reason: None },
        ]);
        agent.model = Arc::new(DefaultModel::new(Arc::new(provider), "mock-1"));
        let _ = agent
            .run_once_streaming("continue rust", &tx)
            .await
            .unwrap();

        let mut saw_consumed = false;
        for _ in 0..32 {
            let ev = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
                .await
                .ok()
                .flatten();
            let Some(ev) = ev else { break };
            if let AgentEvent::MemoryStateChanged { event } = ev {
                if matches!(event, MemoryStateEvent::InjectionConsumed { .. }) {
                    saw_consumed = true;
                    break;
                }
            }
        }
        assert!(saw_consumed);
    }

    #[tokio::test]
    async fn phase1_artifact_events_are_emitted() {
        let provider = MockProvider::new("mock");
        provider.push_script(vec![
            StreamEvent::ToolUse {
                id: "c1".into(),
                name: "long_echo".into(),
                input: json!({}),
            },
            StreamEvent::MessageStop { stop_reason: None },
        ]);
        provider.push_script(vec![
            StreamEvent::TextDelta {
                text: "done".into(),
            },
            StreamEvent::MessageStop { stop_reason: None },
        ]);

        let model: Arc<dyn Model> = Arc::new(DefaultModel::new(Arc::new(provider), "mock-1"));
        let harness = Harness::new(
            FoxAgentSdkConfig {
                compaction: CompactionConfig {
                    enabled: true,
                    token_budget: 1000,
                    ..Default::default()
                },
                safety: SafetyConfig {
                    default_policy: DefaultSafetyPolicy::Allow,
                    ..Default::default()
                },
                ..Default::default()
            },
            None,
        );
        harness
            .register_tool(Arc::new(StaticTool {
                name: "long_echo",
                description: "returns a large payload",
                text: "x".repeat(4000),
            }))
            .await;
        let agent = Agent::new(model, harness, Arc::new(tokio::sync::RwLock::new(None)));

        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let outcome = agent.run_once_streaming("go", &tx).await.unwrap();

        let mut saw_stored = false;
        for _ in 0..32 {
            let ev = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
                .await
                .ok()
                .flatten();
            let Some(ev) = ev else { break };
            if let AgentEvent::ArtifactStored { tool_name, .. } = ev {
                assert_eq!(tool_name, "long_echo");
                saw_stored = true;
                break;
            }
        }

        assert!(saw_stored);
        assert!(matches!(outcome, TurnOutcome::Completed { .. }));
    }

    #[tokio::test]
    async fn phase1_artifact_read_emits_event() {
        let provider = MockProvider::new("mock");
        let harness = Harness::new(
            FoxAgentSdkConfig {
                safety: SafetyConfig {
                    default_policy: DefaultSafetyPolicy::Allow,
                    ..Default::default()
                },
                ..Default::default()
            },
            None,
        );

        let record = harness
            .artifact_store
            .put_text(
                harness.session_id(),
                ArtifactProducer::Tool {
                    tool_name: "read".to_string(),
                },
                ArtifactType::FileChunk,
                ArtifactRetentionClass::Ephemeral,
                "abcdefghij".to_string(),
                json!({}),
            )
            .await
            .unwrap()
            .record;

        harness
            .register_tool(Arc::new(ArtifactReadTool::new(
                harness.artifact_store.clone(),
            )))
            .await;

        provider.push_script(vec![
            StreamEvent::ToolUse {
                id: "c1".into(),
                name: "artifact_read".into(),
                input: json!({"artifact_id": record.artifact_id, "offset_chars": 1, "limit_chars": 3}),
            },
            StreamEvent::MessageStop { stop_reason: None },
        ]);
        provider.push_script(vec![
            StreamEvent::TextDelta {
                text: "done".into(),
            },
            StreamEvent::MessageStop { stop_reason: None },
        ]);

        let model: Arc<dyn Model> = Arc::new(DefaultModel::new(Arc::new(provider), "mock-1"));
        let agent = Agent::new(model, harness, Arc::new(tokio::sync::RwLock::new(None)));
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let _ = agent.run_once_streaming("go", &tx).await.unwrap();

        let mut saw_read = false;
        for _ in 0..32 {
            let ev = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
                .await
                .ok()
                .flatten();
            let Some(ev) = ev else { break };
            if let AgentEvent::ArtifactRead {
                artifact_id,
                returned_chars,
                offset_chars,
                limit_chars,
                ..
            } = ev
            {
                assert_eq!(artifact_id, record.artifact_id);
                assert_eq!(returned_chars, 3);
                assert_eq!(offset_chars, 1);
                assert_eq!(limit_chars, 3);
                saw_read = true;
                break;
            }
        }
        assert!(saw_read);
    }

    #[tokio::test]
    async fn phase2_mcp_profile_externalizes_large_result() {
        let provider = MockProvider::new("mock");
        provider.push_script(vec![
            StreamEvent::ToolUse {
                id: "c1".into(),
                name: "mcp__filesystem__read_file".into(),
                input: json!({}),
            },
            StreamEvent::MessageStop { stop_reason: None },
        ]);
        provider.push_script(vec![
            StreamEvent::TextDelta {
                text: "done".into(),
            },
            StreamEvent::MessageStop { stop_reason: None },
        ]);

        let model: Arc<dyn Model> = Arc::new(DefaultModel::new(Arc::new(provider), "mock-1"));
        let harness = Harness::new(
            FoxAgentSdkConfig {
                compaction: CompactionConfig {
                    enabled: true,
                    token_budget: 100_000,
                    ..Default::default()
                },
                safety: SafetyConfig {
                    default_policy: DefaultSafetyPolicy::Allow,
                    ..Default::default()
                },
                ..Default::default()
            },
            None,
        );
        harness
            .register_tool(Arc::new(StaticTool {
                name: "mcp__filesystem__read_file",
                description: "mock filesystem MCP read",
                text: "x".repeat(2500),
            }))
            .await;

        let mut agent = Agent::new(model, harness, Arc::new(tokio::sync::RwLock::new(None)));
        agent.set_mcp_runtime_metadata(
            std::iter::once((
                "filesystem".to_string(),
                McpServerProfile {
                    server_name: "filesystem".to_string(),
                    kind: McpServerKind::Filesystem,
                    transport: McpTransportKind::Stdio,
                    auto_approve: false,
                    allowed_tools: Vec::new(),
                    capability_tags: Vec::new(),
                },
            ))
            .collect(),
            vec![McpToolDescriptorSnapshot {
                server_name: "filesystem".to_string(),
                tool_name: "mcp__filesystem__read_file".to_string(),
                original_name: "mcp://filesystem/read_file".to_string(),
                description: "Read a file from filesystem".to_string(),
                input_schema: json!({}),
                output_hint: None,
            }],
        );

        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let _ = agent.run_once_streaming("go", &tx).await.unwrap();

        let mut saw_externalized_output = false;
        let mut saw_artifact_metadata = false;
        for _ in 0..32 {
            let ev = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
                .await
                .ok()
                .flatten();
            let Some(ev) = ev else { break };
            match ev {
                AgentEvent::ToolCallEnd { output, .. } => {
                    if output.text.contains("[OUTPUT EXTERNALIZED: artifact_id=") {
                        saw_externalized_output = true;
                    }
                }
                AgentEvent::ArtifactStored {
                    artifact_type,
                    server_name,
                    server_kind,
                    transport,
                    original_tool_name,
                    externalized_reason,
                    ..
                } => {
                    assert_eq!(artifact_type, "McpFilesystemSnapshot");
                    assert_eq!(server_name.as_deref(), Some("filesystem"));
                    assert_eq!(server_kind.as_deref(), Some("filesystem"));
                    assert_eq!(transport.as_deref(), Some("stdio"));
                    assert_eq!(
                        original_tool_name.as_deref(),
                        Some("mcp://filesystem/read_file")
                    );
                    assert_eq!(externalized_reason.as_deref(), Some("mcp:filesystem-large"));
                    saw_artifact_metadata = true;
                }
                _ => {}
            }
            if saw_externalized_output && saw_artifact_metadata {
                break;
            }
        }
        assert!(saw_externalized_output);
        assert!(saw_artifact_metadata);
    }

    #[tokio::test]
    async fn phase2_artifact_read_emits_mcp_audit_fields() {
        let provider = MockProvider::new("mock");
        let harness = Harness::new(
            FoxAgentSdkConfig {
                safety: SafetyConfig {
                    default_policy: DefaultSafetyPolicy::Allow,
                    ..Default::default()
                },
                ..Default::default()
            },
            None,
        );

        let record = harness
            .artifact_store
            .put_text(
                harness.session_id(),
                ArtifactProducer::Mcp {
                    server_name: "filesystem".to_string(),
                    tool_name: "read_file".to_string(),
                },
                ArtifactType::McpFilesystemSnapshot,
                ArtifactRetentionClass::Referenced,
                "hello from filesystem artifact".to_string(),
                json!({
                    "tool_name": "mcp__filesystem__read_file",
                    "server_name": "filesystem",
                    "server_kind": "filesystem",
                    "transport": "stdio",
                    "original_tool_name": "mcp://filesystem/read_file",
                }),
            )
            .await
            .unwrap()
            .record;

        harness
            .register_tool(Arc::new(ArtifactReadTool::new(
                harness.artifact_store.clone(),
            )))
            .await;

        provider.push_script(vec![
            StreamEvent::ToolUse {
                id: "c1".into(),
                name: "artifact_read".into(),
                input: json!({"artifact_id": record.artifact_id, "offset_chars": 0, "limit_chars": 5}),
            },
            StreamEvent::MessageStop { stop_reason: None },
        ]);
        provider.push_script(vec![
            StreamEvent::TextDelta {
                text: "done".into(),
            },
            StreamEvent::MessageStop { stop_reason: None },
        ]);

        let model: Arc<dyn Model> = Arc::new(DefaultModel::new(Arc::new(provider), "mock-1"));
        let agent = Agent::new(model, harness, Arc::new(tokio::sync::RwLock::new(None)));
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let _ = agent.run_once_streaming("go", &tx).await.unwrap();

        let mut saw_read = false;
        for _ in 0..32 {
            let ev = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
                .await
                .ok()
                .flatten();
            let Some(ev) = ev else { break };
            if let AgentEvent::ArtifactRead {
                artifact_id,
                source_tool_name,
                artifact_type,
                server_name,
                server_kind,
                transport,
                original_tool_name,
                ..
            } = ev
            {
                assert_eq!(artifact_id, record.artifact_id);
                assert_eq!(
                    source_tool_name.as_deref(),
                    Some("mcp__filesystem__read_file")
                );
                assert_eq!(artifact_type.as_deref(), Some("McpFilesystemSnapshot"));
                assert_eq!(server_name.as_deref(), Some("filesystem"));
                assert_eq!(server_kind.as_deref(), Some("filesystem"));
                assert_eq!(transport.as_deref(), Some("stdio"));
                assert_eq!(
                    original_tool_name.as_deref(),
                    Some("mcp://filesystem/read_file")
                );
                saw_read = true;
                break;
            }
        }
        assert!(saw_read);
    }

    #[tokio::test]
    async fn phase3_auto_extract_emits_ingestion_event_and_persists_memory() {
        let provider = MockProvider::new("mock");
        provider.push_script(vec![
            StreamEvent::TextDelta {
                text: "Done, I will keep answers concise.".into(),
            },
            StreamEvent::MessageStop { stop_reason: None },
        ]);
        provider.push_script(vec![
            StreamEvent::TextDelta {
                text: "preference|User prefers concise rust answers|high".into(),
            },
            StreamEvent::MessageStop { stop_reason: None },
        ]);

        let model: Arc<dyn Model> = Arc::new(DefaultModel::new(Arc::new(provider), "mock-1"));
        let mem_dir = std::env::temp_dir().join(format!("fox-sdk-mem-{}", uuid::Uuid::new_v4()));
        let harness = Harness::new(
            FoxAgentSdkConfig {
                memory: MemoryConfig {
                    enabled: true,
                    auto_extract: true,
                    auto_extract_scope: fox_agent_core::AutoExtractScope::Global,
                    auto_extract_message_window: 4,
                    verify_relevance: false,
                    embedding_enabled: false,
                    ..Default::default()
                },
                storage_dir: mem_dir,
                ..Default::default()
            },
            None,
        );
        let agent = Agent::new(model, harness, Arc::new(tokio::sync::RwLock::new(None)));

        let (tx, mut rx) = tokio::sync::mpsc::channel(32);
        let _ = agent
            .run_once_streaming("Please keep rust answers concise", &tx)
            .await
            .unwrap();

        // auto_extract is spawned async; wait for it to complete
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let mut saw_ingestion = false;
        for _ in 0..60 {
            let ev = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
                .await
                .ok()
                .flatten();
            let Some(ev) = ev else { break };
            if let AgentEvent::MemoryStateChanged { event } = ev {
                if let MemoryStateEvent::IngestionCompleted { created_ids, .. } = event {
                    saw_ingestion = !created_ids.is_empty();
                    if saw_ingestion {
                        break;
                    }
                }
            }
        }
        assert!(
            saw_ingestion,
            "expected IngestionCompleted event from auto_extract"
        );

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let stored = agent
            .harness
            .memory_manager
            .core()
            .list(fox_agent_core::MemoryScope::Global)
            .unwrap();
        assert!(
            stored
                .iter()
                .any(|entry| entry.content.contains("concise rust answers"))
        );
    }

    #[tokio::test]
    async fn phase4_prompt_builder_includes_planning_context() {
        let session_id = format!("phase4-{}", uuid::Uuid::new_v4());
        let root = std::env::temp_dir().join(format!("fox-sdk-planning-{}", uuid::Uuid::new_v4()));
        let planning_store: Arc<dyn PlanningStore> = Arc::new(FilePlanningStore::new(root));
        let _ = fox_agent_core::save_todos_with_store(
            planning_store.as_ref(),
            &session_id,
            vec![TodoItem {
                id: "t1".into(),
                content: "implement phase4".into(),
                status: TodoStatus::InProgress,
                priority: TodoPriority::High,
            }],
            false,
        );
        let _ = fox_agent_core::save_plan_with_store(
            planning_store.as_ref(),
            &session_id,
            vec![PlanItem {
                id: "p1".into(),
                content: "spawn worker".into(),
                status: PlanStatus::Pending,
                priority: PlanPriority::High,
                assigned_to: None,
                blocked_by: vec![],
            }],
            false,
        );

        let builder = crate::prompt_builder::PromptBuilder::new("1.0.0", "abc123");
        let (split, _) = builder.build_split(
            &session_id,
            &planning_store,
            None,
            &[],
            None,
            None,
            None,
            None,
            None,
        );
        assert!(split.dynamic_part.contains("implement phase4"));
        assert!(
            split.dynamic_part.contains("implement phase4"),
            "dynamic part should contain todo items: {}",
            split.dynamic_part
        );
        assert!(split.dynamic_part.contains("spawn worker"));
    }

    #[test]
    fn phase5_status_bar_injected_into_prompt() {
        // Verify that status_text is rendered into the dynamic prompt section.
        let session_id = format!("phase5-status-{}", uuid::Uuid::new_v4());
        let builder = crate::prompt_builder::PromptBuilder::new("1.0.0", "abc123");
        let root = std::env::temp_dir().join(format!("fox-sdk-status-{}", uuid::Uuid::new_v4()));
        let planning_store: Arc<dyn PlanningStore> = Arc::new(FilePlanningStore::new(root));

        // No status text 鈫?should NOT appear
        let (split_no_status, _) = builder.build_split(
            &session_id,
            &planning_store,
            None,
            &[],
            None,
            None,
            None,
            None,
            None,
        );
        assert!(!split_no_status.dynamic_part.contains("Task Status"));

        // With status text 鈫?should appear
        let status_text = "<!-- AGENT_STATUS_BAR -->\n# Task Status\n\n## Runtime\n\
| Turn | 5 |\n<!-- /AGENT_STATUS_BAR -->";
        let (split_with_status, _) = builder.build_split(
            &session_id,
            &planning_store,
            None,
            &[],
            None,
            None,
            None,
            None,
            Some(status_text),
        );
        assert!(split_with_status.dynamic_part.contains("Task Status"));
        assert!(split_with_status.dynamic_part.contains("AGENT_STATUS_BAR"));
    }

    #[tokio::test]
    async fn m1_auto_snapshot_persists_and_restores_session_state() {
        let provider = MockProvider::new("mock");
        provider.push_script(vec![
            StreamEvent::TextDelta {
                text: "snapshot restored".into(),
            },
            StreamEvent::MessageStop { stop_reason: None },
        ]);

        let working_dir = std::env::temp_dir().join(format!("fox-sdk-m1-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&working_dir).await.unwrap();
        let storage_root = working_dir.join("data");

        let model: Arc<dyn Model> = Arc::new(DefaultModel::new(Arc::new(provider), "mock-1"));
        let harness = Harness::new(
            FoxAgentSdkConfig {
                storage_dir: storage_root.clone(),
                auto_snapshot: true,
                ..Default::default()
            },
            Some(working_dir.clone()),
        );
        let agent = Agent::new(
            model.clone(),
            harness,
            Arc::new(tokio::sync::RwLock::new(None)),
        );
        let session_id = agent.harness().session_id().to_string();

        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        let outcome = agent
            .run_once_streaming("persist this session", &tx)
            .await
            .unwrap();
        assert!(matches!(outcome, TurnOutcome::Completed { .. }));

        // Wait for the background tokio::spawn in persist_snapshot to finish.
        // The snapshot is saved asynchronously to avoid blocking the agent loop.
        for _ in 0..10 {
            if agent
                .harness()
                .session_store
                .load_session(&session_id)
                .unwrap()
                .is_some()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

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
                storage_dir: storage_root,
                auto_snapshot: true,
                ..Default::default()
            },
            Some(working_dir.clone()),
        );
        let restored = Agent::load_from_store(model, restore_harness, &session_id)
            .unwrap()
            .expect("agent should restore from snapshot");
        assert_eq!(restored.harness().session_id(), session_id);
        assert!(restored.harness().session_messages().await.len() >= 2);

        let _ = tokio::fs::remove_dir_all(&working_dir).await;
    }

    #[tokio::test]
    async fn phase3_emits_model_message_lifecycle_and_usage() {
        let provider = MockProvider::new("mock");
        provider.push_script(vec![
            StreamEvent::TextDelta {
                text: "hello".into(),
            },
            StreamEvent::Usage {
                usage: TokenUsage {
                    input_tokens: 11,
                    output_tokens: 4,
                    total_tokens: 15,
                    cache_read_input_tokens: None,
                    cache_creation_input_tokens: None,
                },
            },
            StreamEvent::MessageStop { stop_reason: None },
        ]);

        let model: Arc<dyn Model> = Arc::new(DefaultModel::new(Arc::new(provider), "mock-1"));
        let harness = Harness::new(FoxAgentSdkConfig::default(), None);
        let agent = Agent::new(model, harness, Arc::new(tokio::sync::RwLock::new(None)));

        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let outcome = agent.run_once_streaming("go", &tx).await.unwrap();
        assert!(matches!(outcome, TurnOutcome::Completed { .. }));

        let mut saw_start = false;
        let mut saw_usage = false;
        let mut saw_end = false;
        for _ in 0..12 {
            let ev = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
                .await
                .ok()
                .flatten();
            let Some(ev) = ev else { break };
            match ev {
                AgentEvent::ModelMessageStart { .. } => saw_start = true,
                AgentEvent::ModelUsage { usage } => {
                    saw_usage = usage.total_tokens == 15;
                }
                AgentEvent::ModelMessageEnd { .. } => saw_end = true,
                _ => {}
            }
            if saw_start && saw_usage && saw_end {
                break;
            }
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
        let agent = Agent::new(model, harness, Arc::new(tokio::sync::RwLock::new(None)));

        // Simulate an in-progress turn: request graceful shutdown, then run
        // a turn directly via the test helper (which bypasses run_once_streaming's
        // flag-clearing). This mirrors the real scenario where the user cancels
        // a turn that is already running.
        agent.harness().request_graceful_shutdown().await;

        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let outcome = agent.run_turn_for_test("go", &tx).await.unwrap();
        assert!(matches!(outcome, TurnOutcome::Cancelled));

        let mut saw_cancel_error = false;
        for _ in 0..8 {
            let ev = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
                .await
                .ok()
                .flatten();
            let Some(ev) = ev else { break };
            if let AgentEvent::Error { error } = ev {
                if error.kind() == ErrorKind::Internal {
                    saw_cancel_error = true;
                    break;
                }
            }
        }
        assert!(saw_cancel_error);
    }

    #[tokio::test]
    async fn new_user_message_clears_stale_graceful_shutdown() {
        // Regression test: a graceful shutdown requested to cancel a previous
        // turn must NOT carry over and immediately cancel the next turn started
        // by a fresh user message.
        let provider = MockProvider::new("mock");
        provider.push_script(vec![
            StreamEvent::TextDelta {
                text: "hello from a fresh turn".into(),
            },
            StreamEvent::MessageStop {
                stop_reason: Some("stop".into()),
            },
        ]);
        let model: Arc<dyn Model> = Arc::new(DefaultModel::new(Arc::new(provider), "mock-1"));
        let harness = Harness::new(FoxAgentSdkConfig::default(), None);
        let agent = Agent::new(model, harness, Arc::new(tokio::sync::RwLock::new(None)));

        // Stale shutdown flag left over from a cancelled previous turn.
        agent.harness().request_graceful_shutdown().await;

        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        let outcome = agent.run_once_streaming("go", &tx).await.unwrap();
        // Should complete, NOT be cancelled.
        assert!(matches!(outcome, TurnOutcome::Completed { .. }));
    }

    #[tokio::test]
    async fn phase3_tool_error_emits_structured_error_event() {
        let provider = MockProvider::new("mock");
        provider.push_script(vec![
            StreamEvent::ToolUse {
                id: "missing-call".into(),
                name: "missing_tool".into(),
                input: json!({}),
            },
            StreamEvent::MessageStop { stop_reason: None },
        ]);

        let model: Arc<dyn Model> = Arc::new(DefaultModel::new(Arc::new(provider), "mock-1"));
        let harness = Harness::new(FoxAgentSdkConfig::default(), None);
        let agent = Agent::new(model, harness, Arc::new(tokio::sync::RwLock::new(None)));

        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let err = agent.run_once_streaming("go", &tx).await.unwrap_err();
        // After tool error, the agent loop continues and the MockProvider
        // returns no more responses, producing a Provider error.
        assert!(matches!(err, AgentError::Provider(_)));
        eprintln!("ERROR TYPE (expected after tool failure + provider retry): {err:?}");

        let mut saw_tool_error = false;
        for _ in 0..8 {
            let ev = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
                .await
                .ok()
                .flatten();
            let Some(ev) = ev else { break };
            if matches!(&ev, AgentEvent::Error { error } if error.kind() == ErrorKind::Tool) {
                saw_tool_error = true;
                break;
            }
            if let AgentEvent::ToolCallEnd { output, .. } = &ev {
                if output.is_error {
                    saw_tool_error = true;
                    break;
                }
            }
        }
        assert!(saw_tool_error);
    }

    // 鈹€鈹€ M2: AgentBuilder tests 鈹€鈹€

    /// AgentBuilder::build() with MockProvider and default tools.
    #[tokio::test]
    async fn m2_builder_build_with_mock_provider_produces_agent() {
        let provider = Arc::new(MockProvider::new("mock"));
        provider.push_script(vec![
            StreamEvent::TextDelta { text: "ok".into() },
            StreamEvent::MessageStop { stop_reason: None },
        ]);

        let agent = AgentBuilder::new()
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
        assert!(
            names.contains(&"read"),
            "expected 'read' tool; got: {names:?}"
        );
        assert!(
            names.contains(&"todo"),
            "expected 'todo' tool; got: {names:?}"
        );
        assert!(
            names.contains(&"plan"),
            "expected 'plan' tool; got: {names:?}"
        );
        assert!(
            names.contains(&"goal"),
            "expected 'goal' tool; got: {names:?}"
        );
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
            StreamEvent::ToolUse {
                id: "c1".into(),
                name: "echo".into(),
                input: json!({"text":"hi"}),
            },
            StreamEvent::MessageStop { stop_reason: None },
        ]);

        let agent = AgentBuilder::new()
            .with_provider(provider)
            .model_id("mock-1")
            .with_tool(Arc::new(EchoTool))
            .sdk_config(FoxAgentSdkConfig {
                safety: SafetyConfig {
                    default_policy: DefaultSafetyPolicy::Allow,
                    tool_denylist: Some(vec!["echo".to_string()]),
                    tool_allowlist: None,
                    ..Default::default()
                },
                ..Default::default()
            })
            .build()
            .await
            .expect("build should succeed");

        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        let outcome = agent.run_once_streaming("go", &tx).await.unwrap();
        assert!(
            matches!(outcome, TurnOutcome::RequiresUserDecision { ref request } if request.tool_name == "echo")
        );
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
            agent.harness().session_working_dir().map(|p| p.as_path()),
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
            progress_tx: None,
        };
        // Read outside sandbox 鈫?should fail
        let result = harness
            .execute_tool("read", json!({"file_path": r"C:\Windows"}), ctx)
            .await;
        assert!(
            result.is_err() || result.unwrap().is_error,
            "sandbox should block reads outside the workspace"
        );

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

    // 鈹€鈹€ M4: Swarm + Replay tests 鈹€鈹€

    use crate::replay_runner::ReplayRunner;
    use fox_agent_core::{EnvelopePayload, EventEnvelope};
    use fox_agent_swarm::WorkerStatus;

    /// Swarm coordinator with supervisor: complete lifecycle test.
    #[tokio::test]
    async fn m4_supervisor_worker_lifecycle() {
        let coordinator = Arc::new(SwarmCoordinator::new());
        let supervisor = fox_agent_swarm::SwarmSupervisor::with_defaults(coordinator.clone());

        // Spawn workers
        coordinator.spawn("w1", "analyst").await;
        coordinator.spawn("w2", "reviewer").await;

        // Upsert plan
        coordinator
            .upsert_plan(vec![PlanItem {
                id: "task-a".into(),
                content: "analyse".into(),
                status: PlanStatus::Pending,
                priority: PlanPriority::High,
                assigned_to: None,
                blocked_by: vec![],
            }])
            .await;

        // Assign and complete
        let task = coordinator.assign_next_runnable_task("w1").await.unwrap();
        assert_eq!(task.id, "task-a");
        let w1 = coordinator
            .list_workers()
            .await
            .iter()
            .find(|w| w.worker_id == "w1")
            .cloned()
            .unwrap();
        assert_eq!(w1.status, WorkerStatus::Running);

        coordinator
            .report_completion("w1", "task-a", "done")
            .await
            .unwrap();
        let w1 = coordinator
            .list_workers()
            .await
            .iter()
            .find(|w| w.worker_id == "w1")
            .cloned()
            .unwrap();
        assert_eq!(w1.status, WorkerStatus::Completed);

        // Summary
        let summary = supervisor.generate_summary().await;
        assert_eq!(summary.completed, 1);
        assert!(summary.all_terminal());
    }

    /// Supervisor failure retry: task resets to Pending and worker goes Ready.
    #[tokio::test]
    async fn m4_supervisor_retry_resets_task_and_worker() {
        let coordinator = Arc::new(SwarmCoordinator::new());
        let supervisor = fox_agent_swarm::SwarmSupervisor::with_defaults(coordinator.clone());

        coordinator.spawn("w1", "worker").await;
        coordinator
            .upsert_plan(vec![PlanItem {
                id: "t1".into(),
                content: "task".into(),
                status: PlanStatus::Pending,
                priority: PlanPriority::High,
                assigned_to: None,
                blocked_by: vec![],
            }])
            .await;
        coordinator.assign_next_runnable_task("w1").await.unwrap();

        // Simulate failure
        let handled = supervisor.handle_failure("w1", "t1").await;
        assert!(handled, "failure should be handled");

        // Worker should be Ready again
        let w1 = coordinator
            .list_workers()
            .await
            .iter()
            .find(|w| w.worker_id == "w1")
            .cloned()
            .unwrap();
        assert_eq!(w1.status, WorkerStatus::Ready);

        // Plan item should be Pending
        let plan = coordinator.shared_plan.read().await;
        let item = plan.items.iter().find(|i| i.id == "t1").unwrap();
        assert_eq!(item.status, PlanStatus::Pending);
    }

    /// SwarmSummaryReport aggregates correctly from reports.
    #[test]
    fn m4_summary_report_aggregation() {
        let reports = vec![
            AgentReport {
                worker_id: "a".into(),
                task_id: Some("t1".into()),
                status: WorkerStatus::Completed,
                summary: "ok".into(),
            },
            AgentReport {
                worker_id: "b".into(),
                task_id: Some("t2".into()),
                status: WorkerStatus::Failed,
                summary: "err".into(),
            },
            AgentReport {
                worker_id: "c".into(),
                task_id: Some("t3".into()),
                status: WorkerStatus::TimedOut,
                summary: "timeout".into(),
            },
        ];
        let summary = SwarmSummaryReport::from_reports(&reports);
        assert_eq!(summary.completed, 1);
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.timed_out, 1);
        assert!(summary.format().contains("3 workers total"));
    }

    /// ReplayRunner can load and verify a transcript from JSONL.
    #[tokio::test]
    async fn m4_replay_runner_load_and_verify() {
        use fox_agent_swarm::{GoldenTranscript, TranscriptCheck};
        use std::io::Write;

        // Build envelopes directly (avoids EventRecorder blocking inside async runtime)
        let envelopes = vec![
            EventEnvelope::new(
                "rp-session",
                1,
                0,
                "agent",
                EnvelopePayload::TurnStart { turn_id: 1 },
            ),
            EventEnvelope::new(
                "rp-session",
                1,
                1,
                "agent",
                EnvelopePayload::ModelTextDelta {
                    text: "hello world".into(),
                },
            ),
        ];

        // Export to temp file
        let tmp_path =
            std::env::temp_dir().join(format!("fox-m4-replay-{}.jsonl", uuid::Uuid::new_v4()));
        {
            let mut f = std::fs::File::create(&tmp_path).unwrap();
            for env in &envelopes {
                writeln!(&mut f, "{}", env.to_json_line().unwrap()).unwrap();
            }
        }

        // Load via ReplayRunner
        let runner = ReplayRunner::from_file(&tmp_path).unwrap();
        assert_eq!(runner.events().len(), 2);
        assert_eq!(runner.total_tokens(), 0);

        // Verify text
        let transcript = GoldenTranscript {
            session_id: "rp-session".into(),
            events: runner
                .events()
                .iter()
                .map(|e| serde_json::to_string(e).unwrap())
                .collect(),
            verification_checks: vec![
                TranscriptCheck {
                    description: "contains hello".into(),
                    event_id: None,
                    must_contain_text: Some("hello world".into()),
                    must_have_tool_call: None,
                    must_have_usage: false,
                },
                TranscriptCheck {
                    description: "no tool call expected".into(),
                    event_id: None,
                    must_contain_text: None,
                    must_have_tool_call: Some("read".into()),
                    must_have_usage: false,
                },
            ],
        };
        let runner2 = ReplayRunner::from_transcript(transcript);
        let failures = runner2.verify();
        assert!(
            !failures.is_empty(),
            "one check should fail (no tool call present)"
        );

        let _ = tokio::fs::remove_file(&tmp_path).await;
    }

    /// ReplayRunner filters events by source.
    #[test]
    fn m4_replay_runner_events_by_source() {
        let envelopes = vec![
            EventEnvelope::new(
                "s1",
                1,
                0,
                "agent",
                EnvelopePayload::TurnStart { turn_id: 1 },
            ),
            EventEnvelope::new(
                "s1",
                1,
                1,
                "tool",
                EnvelopePayload::ToolCallStart {
                    call_id: "c1".into(),
                    name: "read".into(),
                    input: serde_json::json!({"file":"x"}),
                },
            ),
            EventEnvelope::new(
                "s1",
                1,
                2,
                "agent",
                EnvelopePayload::TurnEnd {
                    turn_id: 1,
                    outcome: "Completed".into(),
                },
            ),
        ];

        // Build a transcript manually
        let transcript = fox_agent_swarm::GoldenTranscript {
            session_id: "s1".into(),
            events: envelopes
                .iter()
                .map(|e| serde_json::to_string(e).unwrap())
                .collect(),
            verification_checks: vec![],
        };
        let runner = ReplayRunner::from_transcript(transcript);

        let agent_events = runner.events_by_source("agent");
        assert_eq!(agent_events.len(), 2);

        let tool_events = runner.events_by_source("tool");
        assert_eq!(tool_events.len(), 1);
    }

    // 鈹€鈹€ Phase 2 integration tests 鈹€鈹€

    #[test]
    fn phase2_should_externalize_browser_html() {
        let profiles = std::iter::once((
            "browser".to_string(),
            McpServerProfile {
                server_name: "browser".to_string(),
                kind: McpServerKind::Browser,
                transport: McpTransportKind::Stdio,
                auto_approve: false,
                allowed_tools: Vec::new(),
                capability_tags: Vec::new(),
            },
        ))
        .collect::<std::collections::HashMap<_, _>>();
        let descriptors = std::iter::once((
            "mcp__browser__navigate".to_string(),
            McpToolDescriptorSnapshot {
                server_name: "browser".to_string(),
                tool_name: "mcp__browser__navigate".to_string(),
                original_name: "mcp://browser/navigate".to_string(),
                description: "Navigate to a web page and return HTML content".to_string(),
                input_schema: json!({}),
                output_hint: None,
            },
        ))
        .collect::<std::collections::HashMap<_, _>>();

        let artifact_cfg = ArtifactStoreConfig::default();
        let result = crate::agent::should_externalize_tool_result(
            &artifact_cfg,
            &profiles,
            &descriptors,
            "mcp__browser__navigate",
            "<html><body><p>Hello World</p></body></html>",
            false,
        );
        assert!(
            result.should_externalize,
            "browser HTML should be externalized"
        );
        assert_eq!(result.reason.as_deref(), Some("mcp:browser-html"));
    }

    #[tokio::test]
    async fn phase2_artifact_stored_has_externalized_reason() {
        let provider = MockProvider::new("mock");
        provider.push_script(vec![
            StreamEvent::ToolUse {
                id: "c1".into(),
                name: "mcp__filesystem__read_file".into(),
                input: json!({}),
            },
            StreamEvent::MessageStop { stop_reason: None },
        ]);
        provider.push_script(vec![
            StreamEvent::TextDelta {
                text: "done".into(),
            },
            StreamEvent::MessageStop { stop_reason: None },
        ]);

        let model: Arc<dyn Model> = Arc::new(DefaultModel::new(Arc::new(provider), "mock-1"));
        let harness = Harness::new(
            FoxAgentSdkConfig {
                compaction: CompactionConfig {
                    enabled: true,
                    token_budget: 100_000,
                    ..Default::default()
                },
                safety: SafetyConfig {
                    default_policy: DefaultSafetyPolicy::Allow,
                    ..Default::default()
                },
                ..Default::default()
            },
            None,
        );
        harness
            .register_tool(Arc::new(StaticTool {
                name: "mcp__filesystem__read_file",
                description: "mock filesystem MCP read",
                text: "x".repeat(2000),
            }))
            .await;

        let mut agent = Agent::new(model, harness, Arc::new(tokio::sync::RwLock::new(None)));
        agent.set_mcp_runtime_metadata(
            std::iter::once((
                "filesystem".to_string(),
                McpServerProfile {
                    server_name: "filesystem".to_string(),
                    kind: McpServerKind::Filesystem,
                    transport: McpTransportKind::Stdio,
                    auto_approve: false,
                    allowed_tools: Vec::new(),
                    capability_tags: Vec::new(),
                },
            ))
            .collect(),
            vec![McpToolDescriptorSnapshot {
                server_name: "filesystem".to_string(),
                tool_name: "mcp__filesystem__read_file".to_string(),
                original_name: "mcp://filesystem/read_file".to_string(),
                description: "Read a file from filesystem".to_string(),
                input_schema: json!({}),
                output_hint: None,
            }],
        );

        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let _ = agent.run_once_streaming("go", &tx).await.unwrap();

        let mut saw_reason = false;
        for _ in 0..32 {
            let ev = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
                .await
                .ok()
                .flatten();
            let Some(ev) = ev else { break };
            if let AgentEvent::ArtifactStored {
                artifact_type,
                retention_class,
                server_kind,
                externalized_reason,
                ..
            } = ev
            {
                assert!(!artifact_type.is_empty(), "artifact_type must be set");
                assert!(!retention_class.is_empty(), "retention_class must be set");
                assert_eq!(server_kind.as_deref(), Some("filesystem"));
                assert_eq!(externalized_reason.as_deref(), Some("mcp:filesystem-large"));
                saw_reason = true;
                break;
            }
        }
        assert!(
            saw_reason,
            "ArtifactStored must carry externalized_reason and classification fields"
        );
    }

    #[tokio::test]
    async fn phase2_unprofiled_mcp_asks_user() {
        let provider = MockProvider::new("mock");
        provider.push_script(vec![
            StreamEvent::ToolUse {
                id: "c1".into(),
                name: "mcp__unknown__tool".into(),
                input: json!({}),
            },
            StreamEvent::MessageStop { stop_reason: None },
        ]);

        let model: Arc<dyn Model> = Arc::new(DefaultModel::new(Arc::new(provider), "mock-1"));
        let harness = Harness::new(
            FoxAgentSdkConfig {
                safety: SafetyConfig {
                    default_policy: DefaultSafetyPolicy::Allow,
                    ..Default::default()
                },
                ..Default::default()
            },
            None,
        );
        harness
            .register_tool(Arc::new(StaticTool {
                name: "mcp__unknown__tool",
                description: "unknown MCP tool",
                text: "output".to_string(),
            }))
            .await;

        let mut agent = Agent::new(model, harness, Arc::new(tokio::sync::RwLock::new(None)));
        // Provide a profile with Unknown kind 鈥?no metadata = conservative
        agent.set_mcp_runtime_metadata(
            std::iter::once((
                "unknown".to_string(),
                McpServerProfile {
                    server_name: "unknown".to_string(),
                    kind: McpServerKind::Unknown,
                    transport: McpTransportKind::Stdio,
                    auto_approve: false,
                    allowed_tools: Vec::new(),
                    capability_tags: Vec::new(),
                },
            ))
            .collect(),
            vec![McpToolDescriptorSnapshot {
                server_name: "unknown".to_string(),
                tool_name: "mcp__unknown__tool".to_string(),
                original_name: "mcp://unknown/tool".to_string(),
                description: "An unrecognised tool".to_string(),
                input_schema: json!({}),
                output_hint: None,
            }],
        );

        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        let outcome = agent.run_once_streaming("go", &tx).await.unwrap();
        assert!(
            matches!(outcome, TurnOutcome::RequiresUserDecision { ref request }
                if request.risk_level == RiskLevel::Medium),
            "unprofiled MCP tool should require user confirmation; got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn phase2_artifact_store_stats_by_type() {
        let harness = Harness::new(FoxAgentSdkConfig::default(), None);

        let _ = harness
            .artifact_store
            .put_text(
                harness.session_id(),
                ArtifactProducer::Mcp {
                    server_name: "fs".to_string(),
                    tool_name: "read".to_string(),
                },
                ArtifactType::McpFilesystemSnapshot,
                ArtifactRetentionClass::Referenced,
                "abc".to_string(),
                json!({}),
            )
            .await
            .unwrap();

        let _ = harness
            .artifact_store
            .put_text(
                harness.session_id(),
                ArtifactProducer::Mcp {
                    server_name: "api".to_string(),
                    tool_name: "search".to_string(),
                },
                ArtifactType::McpExternalApiPayload,
                ArtifactRetentionClass::Ephemeral,
                "defg".to_string(),
                json!({}),
            )
            .await
            .unwrap();

        let _ = harness
            .artifact_store
            .put_text(
                harness.session_id(),
                ArtifactProducer::Tool {
                    tool_name: "read".to_string(),
                },
                ArtifactType::FileChunk,
                ArtifactRetentionClass::Ephemeral,
                "hi".to_string(),
                json!({}),
            )
            .await
            .unwrap();

        let stats = harness
            .artifact_store
            .stats_by_type(harness.session_id())
            .await
            .unwrap();

        assert_eq!(stats.total_count, 3);
        assert!(stats.total_bytes > 0);
        assert!(stats.by_type.contains_key("McpFilesystemSnapshot"));
        assert!(stats.by_type.contains_key("McpExternalApiPayload"));
        assert!(stats.by_type.contains_key("FileChunk"));

        let summary = stats.format_summary();
        assert!(summary.contains("3 artifacts"));
        assert!(summary.contains("McpFilesystemSnapshot"));
    }

    // 鈹€鈹€ Phase 3: sub-agent isolation tests 鈹€鈹€

    #[test]
    fn phase3_subagent_summary_format_is_compact() {
        let summary = SubagentSummary {
            task_id: "task_1".into(),
            objective: "test objective".into(),
            outcome: SubagentOutcome::Completed,
            findings: vec!["Found 3 files".into(), "All files use UTF-8".into()],
            evidence_refs: vec![EvidenceRef {
                artifact_id: "art_abc".into(),
                label: "file list".into(),
                snippet: "src/main.rs\nsrc/lib.rs".into(),
            }],
            recommendations: vec!["Refactor to use async".into()],
            uncertainties: vec!["Not sure about Windows paths".into()],
            next_queries: vec!["Check Windows compat".into()],
            token_usage: None,
            turns_used: 3,
            elapsed_secs: 5,
        };

        let formatted = summary.format_for_main_context();
        assert!(formatted.contains("[sub-agent task_1] completed"));
        assert!(formatted.contains("Found 3 files"));
        assert!(formatted.contains("Evidence: 1 artifact"));
        assert!(formatted.contains("Refactor to use async"));
        assert!(formatted.contains("Not sure about Windows paths"));
        // Verify it's compact (well under 1000 chars for a real summary)
        assert!(formatted.len() < 500);
    }

    #[test]
    fn phase3_subagent_summary_error_outcome() {
        let summary = SubagentSummary {
            task_id: "err_1".into(),
            objective: "test objective".into(),
            outcome: SubagentOutcome::Error("connection reset".into()),
            findings: vec![],
            evidence_refs: vec![],
            recommendations: vec![],
            uncertainties: vec!["Failed to connect".into()],
            next_queries: vec![],
            token_usage: None,
            turns_used: 1,
            elapsed_secs: 2,
        };
        let formatted = summary.format_for_main_context();
        assert!(formatted.contains("error: connection reset"));
    }

    #[test]
    fn phase3_subagent_task_serde_roundtrip() {
        let task = SubagentTask {
            task_id: "t1".into(),
            objective: "Find all TODOs".into(),
            context: "Project uses Rust".into(),
            tools: vec!["read".into(), "grep".into()],
            max_turns: 10,
            timeout_secs: 60,
        };
        let json = serde_json::to_string(&task).unwrap();
        let restored: SubagentTask = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.task_id, "t1");
        assert_eq!(restored.objective, "Find all TODOs");
        assert_eq!(restored.tools.len(), 2);
        assert_eq!(restored.max_turns, 10);
    }

    #[test]
    fn phase3_evidence_ref_serde_roundtrip() {
        let eref = EvidenceRef {
            artifact_id: "a1".into(),
            label: "search results".into(),
            snippet: "TODO: fix this".into(),
        };
        let json = serde_json::to_string(&eref).unwrap();
        let restored: EvidenceRef = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.artifact_id, "a1");
        assert_eq!(restored.snippet, "TODO: fix this");
    }
    // 鈹€鈹€ Phase 4: routing policy and governance metrics tests 鈹€鈹€

    #[test]
    fn phase4_routing_engine_inline_for_small_output() {
        let cfg = RoutingPolicyConfig::default();
        let engine = RoutingPolicyEngine::new(cfg);
        let artifact_cfg = ArtifactStoreConfig::default();
        let input = RoutingInput::simple("bash", "short output");
        let result = engine.decide(&input, &artifact_cfg);
        assert!(matches!(result, ToolResultRouting::Inline));
    }

    #[test]
    fn phase4_routing_engine_externalize_for_large_output() {
        let cfg = RoutingPolicyConfig::default();
        let engine = RoutingPolicyEngine::new(cfg);
        let artifact_cfg = ArtifactStoreConfig::default();
        let large = "x".repeat(10_000);
        let input = RoutingInput::simple("read", &large);
        let result = engine.decide(&input, &artifact_cfg);
        assert!(
            matches!(
                result,
                ToolResultRouting::Externalize | ToolResultRouting::DelegateToSubagent
            ),
            "large output should externalize or delegate, got {result:?}"
        );
    }

    #[test]
    fn phase4_routing_engine_truncated_forces_externalize() {
        let cfg = RoutingPolicyConfig::default();
        let engine = RoutingPolicyEngine::new(cfg);
        let artifact_cfg = ArtifactStoreConfig::default();
        let mut input = RoutingInput::simple("read", "small");
        input.truncated_by_context_guard = true;
        let result = engine.decide(&input, &artifact_cfg);
        assert_eq!(
            result,
            ToolResultRouting::Externalize,
            "truncated by context guard must externalize"
        );
    }

    #[test]
    fn phase4_routing_engine_delegate_candidate() {
        let cfg = RoutingPolicyConfig::default();
        let engine = RoutingPolicyEngine::new(cfg);
        let artifact_cfg = ArtifactStoreConfig::default();
        let large = "x".repeat(30_000);
        let input = RoutingInput::simple("grep", &large);
        let result = engine.decide(&input, &artifact_cfg);
        assert_eq!(
            result,
            ToolResultRouting::DelegateToSubagent,
            "grep with 30k chars should delegate to sub-agent"
        );
    }

    #[test]
    fn phase4_routing_engine_high_pressure_escalates() {
        let cfg = RoutingPolicyConfig::default();
        let engine = RoutingPolicyEngine::new(cfg);
        let artifact_cfg = ArtifactStoreConfig::default();
        let mut input = RoutingInput::simple("read", "moderate output");
        input.context_pressure = 0.85; // above 0.70 threshold
        let result = engine.decide(&input, &artifact_cfg);
        assert!(
            matches!(
                result,
                ToolResultRouting::Externalize | ToolResultRouting::DelegateToSubagent
            ),
            "high pressure should escalate to externalize or delegate, got {result:?}"
        );
    }

    #[test]
    fn phase4_governance_metrics_atomic_counters() {
        let m = GovernanceMetrics::new();
        m.record_routing(ToolResultRouting::Inline);
        m.record_routing(ToolResultRouting::Externalize);
        m.record_routing(ToolResultRouting::Externalize);
        m.record_artifact_write(1000);
        m.record_artifact_write(2000);
        m.record_artifact_read();
        m.record_subagent_success();
        m.record_compaction();

        let snap = m.snapshot();
        assert_eq!(snap.inline_count, 1);
        assert_eq!(snap.externalize_count, 2);
        assert_eq!(snap.artifact_write_count, 2);
        assert_eq!(snap.artifact_write_bytes, 3000);
        assert_eq!(snap.artifact_read_count, 1);
        assert_eq!(snap.subagent_task_count, 1);
        assert_eq!(snap.subagent_success_count, 1);
        assert_eq!(snap.compaction_trigger_count, 1);
    }

    #[test]
    fn phase4_metrics_snapshot_format() {
        let m = GovernanceMetrics::new();
        m.record_routing(ToolResultRouting::Inline);
        m.record_routing(ToolResultRouting::Externalize);
        m.record_routing(ToolResultRouting::DelegateToSubagent);
        m.record_artifact_write(500);
        m.record_subagent_success();
        let snap = m.snapshot();
        let formatted = snap.format_summary();
        assert!(formatted.contains("Governance Metrics"));
        assert!(formatted.contains("inline"));
        assert!(formatted.contains("externalize"));
        assert!(formatted.contains("delegate"));
        assert!(formatted.contains("Artifacts"));
        assert!(formatted.contains("Sub-agents"));
        assert!(formatted.contains("Compaction"));
    }

    #[test]
    fn phase4_routing_config_default_delegate_tools() {
        let cfg = RoutingPolicyConfig::default();
        assert!(cfg.delegate_candidate_tools.contains(&"grep".to_string()));
        assert!(cfg.delegate_candidate_tools.contains(&"read".to_string()));
        assert!(
            cfg.delegate_candidate_tools
                .contains(&"web_fetch".to_string())
        );
        assert!(cfg.local_externalize_threshold_chars > 0);
        assert!(cfg.local_delegate_threshold_chars > cfg.local_externalize_threshold_chars);
    }

    #[test]
    fn phase4_message_total_chars() {
        let msg = Message::user("hello world");
        assert_eq!(msg.total_chars(), 11);
        let tool = Message::tool_result("c1", "output text", false);
        assert!(tool.total_chars() > 10);
    }
}
