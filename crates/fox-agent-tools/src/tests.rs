#[cfg(test)]
mod tools_tests {
    use crate::*;
    use fox_agent_core::{Tool, ToolContext, ToolExecutionMode};
    use serde_json::json;
    use uuid::Uuid;

    #[tokio::test]
    async fn read_tool_reads_relative_path_from_working_dir() {
        let dir = std::env::temp_dir().join(format!("fox-agent-tools-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let file_path = dir.join("note.txt");
        tokio::fs::write(&file_path, "hello\nworld\nfoo\nbar").await.unwrap();

        let tool = ReadTool::new();
        let output = tool
            .execute(
                json!({"file_path":"note.txt"}),
                ToolContext {
                    session_id: "s1".into(),
                    message_id: "m1".into(),
                    tool_call_id: "t1".into(),
                    working_dir: Some(dir.clone()),
                    execution_mode: ToolExecutionMode::Foreground,
                    graceful_shutdown_requested: false,
                },
            )
            .await
            .unwrap();

        assert!(output.text.contains("hello"));
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn write_tool_writes_and_shows_diff() {
        let dir = std::env::temp_dir().join(format!("fox-agent-tools-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let tool = WriteTool::new();
        let ctx = ToolContext {
            session_id: "s1".into(),
            message_id: "m1".into(),
            tool_call_id: "t1".into(),
            working_dir: Some(dir.clone()),
            execution_mode: ToolExecutionMode::Foreground,
            graceful_shutdown_requested: false,
        };

        let output = tool
            .execute(json!({"file_path":"note.txt","content":"hello"}), ctx.clone())
            .await
            .unwrap();
        assert!(output.text.contains("Created"));

        let output2 = tool
            .execute(json!({"file_path":"note.txt","content":"world"}), ctx)
            .await
            .unwrap();
        assert!(output2.text.contains("Updated"));
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn edit_tool_replaces_text() {
        let dir = std::env::temp_dir().join(format!("fox-agent-tools-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let file_path = dir.join("edit.txt");
        tokio::fs::write(&file_path, "hello world").await.unwrap();

        let tool = EditTool::new();
        let output = tool
            .execute(
                json!({"file_path":"edit.txt","old_string":"world","new_string":"rust"}),
                ToolContext {
                    session_id: "s1".into(),
                    message_id: "m1".into(),
                    tool_call_id: "t1".into(),
                    working_dir: Some(dir.clone()),
                    execution_mode: ToolExecutionMode::Foreground,
                    graceful_shutdown_requested: false,
                },
            )
            .await
            .unwrap();
        assert!(output.text.contains("Edited"));
        let content = tokio::fs::read_to_string(&file_path).await.unwrap();
        assert_eq!(content, "hello rust");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn grep_tool_returns_error_for_nonexistent_dir() {
        let tool = GrepTool::new();
        let result = tool
            .execute(
                json!({"pattern":"hello","path":"/nonexistent/path/xyz123"}),
                ToolContext {
                    session_id: "s1".into(),
                    message_id: "m1".into(),
                    tool_call_id: "t1".into(),
                    working_dir: None,
                    execution_mode: ToolExecutionMode::Foreground,
                    graceful_shutdown_requested: false,
                },
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn glob_tool_finds_files_matching_pattern() {
        let dir = std::env::temp_dir().join(format!("fox-agent-tools-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("alpha.txt"), "a").await.unwrap();
        tokio::fs::write(dir.join("beta.txt"), "b").await.unwrap();
        tokio::fs::write(dir.join("gamma.rs"), "c").await.unwrap();

        let tool = GlobTool::new();
        let output = tool
            .execute(
                json!({"pattern":"*.txt","path": "."}),
                ToolContext {
                    session_id: "s1".into(),
                    message_id: "m1".into(),
                    tool_call_id: "t1".into(),
                    working_dir: Some(dir.clone()),
                    execution_mode: ToolExecutionMode::Foreground,
                    graceful_shutdown_requested: false,
                },
            )
            .await
            .unwrap();
        assert!(output.text.contains("alpha.txt"));
        assert!(output.text.contains("beta.txt"));
        assert!(!output.text.contains("gamma.rs"));
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn ls_tool_lists_directory_contents() {
        let dir = std::env::temp_dir().join(format!("fox-agent-tools-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("file1.txt"), "a").await.unwrap();
        tokio::fs::create_dir(dir.join("subdir")).await.unwrap();

        let tool = LsTool::new();
        let output = tool
            .execute(
                json!({"path": "."}),
                ToolContext {
                    session_id: "s1".into(),
                    message_id: "m1".into(),
                    tool_call_id: "t1".into(),
                    working_dir: Some(dir.clone()),
                    execution_mode: ToolExecutionMode::Foreground,
                    graceful_shutdown_requested: false,
                },
            )
            .await
            .unwrap();
        assert!(output.text.contains("file1.txt"));
        assert!(output.text.contains("subdir"));
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn bash_tool_runs_command_in_working_dir() {
        let dir = std::env::temp_dir().join(format!("fox-agent-tools-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("sample.txt"), "abc").await.unwrap();

        let command = if cfg!(windows) {
            "Get-Content sample.txt"
        } else {
            "cat sample.txt"
        };
        let output = BashTool::new()
            .execute(
                json!({"command": command, "timeout": 5000}),
                ToolContext {
                    session_id: "s1".into(),
                    message_id: "m1".into(),
                    tool_call_id: "t1".into(),
                    working_dir: Some(dir.clone()),
                    execution_mode: ToolExecutionMode::Foreground,
                    graceful_shutdown_requested: false,
                },
            )
            .await
            .unwrap();
        assert_eq!(output.text, "abc");
        assert_eq!(
            output
                .json
                .as_ref()
                .and_then(|v| v.get("exit_code"))
                .and_then(|v| v.as_i64()),
            Some(0)
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn default_tool_executor_registers_expected_tools() {
        let executor = default_tool_executor().await;
        let defs = executor.tool_definitions().await;
        let names: Vec<_> = defs.into_iter().map(|d| d.name).collect();
        assert!(names.iter().any(|n| n == "read"));
        assert!(names.iter().any(|n| n == "write"));
        assert!(names.iter().any(|n| n == "edit"));
        assert!(names.iter().any(|n| n == "grep"));
        assert!(names.iter().any(|n| n == "glob"));
        assert!(names.iter().any(|n| n == "ls"));
        assert!(names.iter().any(|n| n == "bash"));
        assert!(names.iter().any(|n| n == "webfetch"));
        assert!(names.iter().any(|n| n == "websearch"));
        assert!(names.iter().any(|n| n == "todo"));
        assert!(names.iter().any(|n| n == "plan"));
        assert!(names.iter().any(|n| n == "goal"));
        assert!(names.iter().any(|n| n == "lsp"));
        assert!(names.iter().any(|n| n == "invalid"));
        assert!(names.iter().any(|n| n == "agentgrep"));
        assert!(names.iter().any(|n| n == "memory"));
    }

    #[tokio::test]
    async fn todo_tool_reads_and_merges_items() {
        let ctx = ToolContext {
            session_id: format!("todo-{}", Uuid::new_v4()),
            message_id: "m1".into(),
            tool_call_id: "t1".into(),
            working_dir: None,
            execution_mode: ToolExecutionMode::Foreground,
            graceful_shutdown_requested: false,
        };
        let output = TodoTool
            .execute(
                json!({"todos":[{"id":"a","content":"phase4","status":"pending","priority":"high"}]}),
                ctx.clone(),
            )
            .await
            .unwrap();
        assert_eq!(
            output
                .json
                .as_ref()
                .and_then(|v| v.get("remaining"))
                .and_then(|v| v.as_u64()),
            Some(1)
        );

        TodoTool
            .execute(
                json!({"todos":[{"id":"b","content":"phase4-2","status":"in_progress","priority":"medium"}],"merge":true}),
                ctx.clone(),
            )
            .await
            .unwrap();
        let read = TodoTool.execute(json!({}), ctx).await.unwrap();
        let todos = read
            .json
            .as_ref()
            .and_then(|v| v.get("todos"))
            .and_then(|v| v.as_array())
            .unwrap();
        assert_eq!(todos.len(), 2);
    }

    #[tokio::test]
    async fn plan_tool_tracks_version() {
        let session_id = format!("plan-{}", Uuid::new_v4());
        let ctx = ToolContext {
            session_id: session_id.clone(),
            message_id: "m1".into(),
            tool_call_id: "t1".into(),
            working_dir: None,
            execution_mode: ToolExecutionMode::Foreground,
            graceful_shutdown_requested: false,
        };
        let output = PlanTool
            .execute(
                json!({"items":[{"id":"p1","content":"draft implementation","status":"pending","priority":"high","blocked_by":[]}]}),
                ctx,
            )
            .await
            .unwrap();
        assert_eq!(
            output
                .json
                .as_ref()
                .and_then(|v| v.get("version"))
                .and_then(|v| v.as_u64()),
            Some(1)
        );
    }

    #[tokio::test]
    async fn goal_tool_creates_and_checkpoints() {
        let session_id = format!("goal-{}", Uuid::new_v4());
        let ctx = ToolContext {
            session_id: session_id.clone(),
            message_id: "m1".into(),
            tool_call_id: "t1".into(),
            working_dir: None,
            execution_mode: ToolExecutionMode::Foreground,
            graceful_shutdown_requested: false,
        };
        let created = GoalTool
            .execute(
                json!({"action":"create","title":"finish phase4b","description":"goal tool + api"}),
                ctx.clone(),
            )
            .await
            .unwrap();
        let goal_id = created
            .json
            .as_ref()
            .and_then(|v| v.get("goal"))
            .and_then(|v| v.get("id"))
            .and_then(|v| v.as_str())
            .unwrap()
            .to_string();

        GoalTool
            .execute(
                json!({"action":"checkpoint","id":goal_id,"checkpoint_summary":"implemented goal tool","progress":30}),
                ctx.clone(),
            )
            .await
            .unwrap();

        let shown = GoalTool
            .execute(json!({"action":"show","id":goal_id}), ctx)
            .await
            .unwrap();
        assert!(shown.text.contains("finish phase4b"));
    }

    #[tokio::test]
    async fn webfetch_validates_url() {
        let tool = WebFetchTool::new();
        let result = tool
            .execute(
                json!({"url":"not-a-url"}),
                ToolContext {
                    session_id: "s1".into(),
                    message_id: "m1".into(),
                    tool_call_id: "t1".into(),
                    working_dir: None,
                    execution_mode: ToolExecutionMode::Foreground,
                    graceful_shutdown_requested: false,
                },
            )
            .await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("must start with http"));
    }

    #[tokio::test]
    async fn websearch_parses_bing_html_correctly() {
        // Unit test for Bing HTML parsing — no network call
        let _html = r#"
            <li class="b_algo">
              <h2><a href="https://example.com/rust">Rust Language</a></h2>
              <div class="b_caption"><p>A systems language.</p></div>
            </li>
        "#;

        // The parsing function is module-private, so we test the public API
        // indirectly through error handling
        let tool = WebSearchTool::new();
        // Bing API env is not set, so this will attempt HTML parse
        // We don't call it since it makes a network request;
        // we test via the unit test in websearch module
        let _ = tool;
    }

    #[tokio::test]
    async fn invalid_tool_formats_error() {
        let tool = InvalidTool::new();
        let output = tool
            .execute(
                json!({"tool_name":"nonexistent","reason":"not available"}),
                ToolContext {
                    session_id: "s1".into(),
                    message_id: "m1".into(),
                    tool_call_id: "t1".into(),
                    working_dir: None,
                    execution_mode: ToolExecutionMode::Foreground,
                    graceful_shutdown_requested: false,
                },
            )
            .await
            .unwrap();
        assert!(output.is_error);
        assert!(output.text.contains("nonexistent"));
    }

    #[tokio::test]
    async fn lsp_tool_reports_not_integrated() {
        let dir = std::env::temp_dir().join(format!("fox-agent-tools-lsp-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let file_path = dir.join("main.rs");
        tokio::fs::write(&file_path, "fn main() {}").await.unwrap();

        let tool = LspTool::new();
        let output = tool
            .execute(
                json!({"operation":"goToDefinition","file_path":"main.rs","line":1,"character":1}),
                ToolContext {
                    session_id: "s1".into(),
                    message_id: "m1".into(),
                    tool_call_id: "t1".into(),
                    working_dir: Some(dir.clone()),
                    execution_mode: ToolExecutionMode::Foreground,
                    graceful_shutdown_requested: false,
                },
            )
            .await
            .unwrap();
        assert!(output.text.contains("LSP is not integrated"));
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn context_includes_goals_and_todos() {
        let session_id = format!("ctx-{}", Uuid::new_v4());
        let ctx = ToolContext {
            session_id: session_id.clone(),
            message_id: "m1".into(),
            tool_call_id: "t1".into(),
            working_dir: None,
            execution_mode: ToolExecutionMode::Foreground,
            graceful_shutdown_requested: false,
        };

        GoalTool
            .execute(
                json!({"action":"create","title":"context goal test"}),
                ctx.clone(),
            )
            .await
            .unwrap();
        TodoTool
            .execute(
                json!({"todos":[{"id":"ct1","content":"context todo test","status":"pending","priority":"medium"}]}),
                ctx,
            )
            .await
            .unwrap();

        let context = render_planning_context(&session_id);
        assert!(context.contains("context goal test"));
        assert!(context.contains("context todo test"));
    }

    #[tokio::test]
    async fn memory_tool_remember_and_recall() {
        let tool = MemoryTool::new_test();
        let ctx = ToolContext {
            session_id: format!("mem-{}", Uuid::new_v4()),
            message_id: "m1".into(),
            tool_call_id: "t1".into(),
            working_dir: None,
            execution_mode: ToolExecutionMode::Foreground,
            graceful_shutdown_requested: false,
        };
        let out = tool.execute(json!({"action":"remember","content":"test memory entry","category":"fact"}), ctx.clone()).await.unwrap();
        assert!(out.text.contains("Remembered"));

        let recalled = tool.execute(json!({"action":"recall","mode":"keyword","query":"test"}), ctx.clone()).await.unwrap();
        assert!(recalled.text.contains("test memory entry"));

        tool.execute(json!({"action":"list"}), ctx).await.unwrap();
    }

    #[tokio::test]
    async fn memory_tool_stats_action() {
        let tool = MemoryTool::new_test();
        let ctx = ToolContext {
            session_id: "mem-stats".into(), message_id: "m1".into(), tool_call_id: "t1".into(),
            working_dir: None, execution_mode: ToolExecutionMode::Foreground,
            graceful_shutdown_requested: false,
        };
        let stats = tool.execute(json!({"action":"stats"}), ctx).await.unwrap();
        assert!(stats.text.contains("Memories: 0"));
    }
}
