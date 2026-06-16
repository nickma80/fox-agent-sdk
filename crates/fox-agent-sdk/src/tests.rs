#[cfg(test)]
mod sdk_tests {
    use crate::*;
    use fox_agent_core::{
        AgentEvent, AgentError, CompactionConfig, DefaultModel, DefaultSafetyPolicy,
        FoxAgentSdkConfig, MemoryConfig, MemoryStateEvent, Message, Model, PermissionDecision,
        PermissionRequest, PermissionResult, SafetyConfig, StreamEvent, TokenUsage, Tool,
        ToolContext, ToolError, ToolOutput, TurnOutcome, ErrorKind,
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
    async fn phase4_prompt_builder_includes_planning_context() {
        let session_id = format!("phase4-{}", uuid::Uuid::new_v4());
        let _ = save_todos(&session_id, vec![TodoItem {
            id: "t1".into(), content: "implement phase4".into(), status: TodoStatus::InProgress, priority: TodoPriority::High,
        }], false);
        let _ = save_plan(&session_id, vec![PlanItem {
            id: "p1".into(), content: "spawn worker".into(), status: "pending".into(), priority: "high".into(),
            assigned_to: None, blocked_by: vec![],
        }], false);

        let builder = crate::prompt_builder::PromptBuilder::new("1.0.0", "abc123");
        let (split, _) = builder.build_split(&session_id, None, &[], None, None);
        assert!(split.dynamic_part.contains("implement phase4"));
        // The planning context is routed to dynamic_part, check that
        assert!(split.dynamic_part.contains("implement phase4"), "dynamic part should contain todo items: {}", split.dynamic_part);
        assert!(split.dynamic_part.contains("spawn worker"));
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
}
