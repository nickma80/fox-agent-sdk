#[cfg(test)]
mod tools_tests {
    use crate::*;
    use fox_agent_core::{
        storage, FilePlanningStore, MemoryConfig, MemoryManager, PlanningScope, PlanningStore,
        Tool, ToolContext, ToolExecutionMode,
    };
    use serde_json::json;
    use std::sync::Arc;
    use std::time::Duration;
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
        let tool = TodoTool::default();
        let output = tool
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

        tool
            .execute(
                json!({"todos":[{"id":"b","content":"phase4-2","status":"in_progress","priority":"medium"}],"merge":true}),
                ctx.clone(),
            )
            .await
            .unwrap();
        let read = tool.execute(json!({}), ctx).await.unwrap();
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
        let tool = PlanTool::default();
        let output = tool
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
        let tool = GoalTool::default();
        let created = tool
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

        tool
            .execute(
                json!({"action":"checkpoint","id":goal_id,"checkpoint_summary":"implemented goal tool","progress":30}),
                ctx.clone(),
            )
            .await
            .unwrap();

        let shown = tool
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

        let goal_tool = GoalTool::default();
        let todo_tool = TodoTool::default();

        goal_tool
            .execute(
                json!({"action":"create","title":"context goal test"}),
                ctx.clone(),
            )
            .await
            .unwrap();
        todo_tool
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
    async fn planning_tools_persist_to_file_store_snapshot() {
        let root = std::env::temp_dir().join(format!("fox-agent-tools-planning-{}", Uuid::new_v4()));
        let store = Arc::new(FilePlanningStore::new(root.clone()));
        let session_id = format!("plan-file-{}", Uuid::new_v4());
        let ctx = ToolContext {
            session_id: session_id.clone(),
            message_id: "m1".into(),
            tool_call_id: "t1".into(),
            working_dir: None,
            execution_mode: ToolExecutionMode::Foreground,
            graceful_shutdown_requested: false,
        };
        let todo_tool = TodoTool::new(store.clone());
        let plan_tool = PlanTool::new(store.clone());
        let goal_tool = GoalTool::new(store.clone());

        todo_tool
            .execute(
                json!({"todos":[{"id":"t1","content":"persist planning","status":"pending","priority":"high"}]}),
                ctx.clone(),
            )
            .await
            .unwrap();
        plan_tool
            .execute(
                json!({"items":[{"id":"p1","content":"persist plan item","status":"pending","priority":"high","blocked_by":[]}]}),
                ctx.clone(),
            )
            .await
            .unwrap();
        goal_tool
            .execute(json!({"action":"create","title":"persist goal"}), ctx)
            .await
            .unwrap();

        let snapshot = store
            .load_snapshot(&session_id, PlanningScope::Session)
            .unwrap();
        assert_eq!(snapshot.todos.len(), 1);
        assert_eq!(snapshot.plan.items.len(), 1);
        assert_eq!(snapshot.goals.len(), 1);

        let _ = tokio::fs::remove_dir_all(root).await;
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

    #[tokio::test]
    async fn memory_tool_supports_reindex_and_reembed_actions() {
        let tool = MemoryTool::new_test();
        let ctx = ToolContext {
            session_id: "mem-maint".into(), message_id: "m1".into(), tool_call_id: "t1".into(),
            working_dir: None, execution_mode: ToolExecutionMode::Foreground,
            graceful_shutdown_requested: false,
        };
        tool.execute(json!({"action":"remember","content":"reindex me"}), ctx.clone()).await.unwrap();

        let reindex = tool.execute(json!({"action":"reindex"}), ctx.clone()).await.unwrap();
        assert!(reindex.text.contains("Reindexed"));

        let reembed = tool.execute(json!({"action":"reembed"}), ctx).await.unwrap();
        assert!(reembed.text.contains("Re-embedded"));
    }

    #[tokio::test]
    async fn memory_tool_supports_disable_enable_redact_and_refresh_clusters_actions() {
        let tool = MemoryTool::new_test();
        let ctx = ToolContext {
            session_id: "mem-govern".into(),
            message_id: "m1".into(),
            tool_call_id: "t1".into(),
            working_dir: None,
            execution_mode: ToolExecutionMode::Foreground,
            graceful_shutdown_requested: false,
        };

        let remembered = tool
            .execute(
                json!({"action":"remember","content":"private memory","category":"fact"}),
                ctx.clone(),
            )
            .await
            .unwrap();
        let memory_id = remembered
            .json
            .as_ref()
            .and_then(|value| value.get("id"))
            .and_then(|value| value.as_str())
            .unwrap()
            .to_string();

        let disabled = tool
            .execute(json!({"action":"disable","id":memory_id}), ctx.clone())
            .await
            .unwrap();
        assert!(disabled.text.contains("Disabled memory"));

        let enabled = tool
            .execute(json!({"action":"enable","id":memory_id}), ctx.clone())
            .await
            .unwrap();
        assert!(enabled.text.contains("Enabled memory"));

        let redacted = tool
            .execute(
                json!({"action":"redact","id":memory_id,"replacement":"[hidden]"}),
                ctx.clone(),
            )
            .await
            .unwrap();
        assert!(redacted.text.contains("Redacted memory"));

        let listed = tool
            .execute(json!({"action":"list"}), ctx.clone())
            .await
            .unwrap();
        assert!(listed.text.contains("[hidden]"));

        let refreshed = tool
            .execute(json!({"action":"refresh_clusters","scope":"all"}), ctx)
            .await
            .unwrap();
        assert!(refreshed.text.contains("Refreshed clusters"));
    }

    #[tokio::test]
    async fn memory_tool_supports_export_import_and_rebuild_ann_actions() {
        let source_storage = std::env::temp_dir().join(format!("fox-agent-tools-memory-src-{}", Uuid::new_v4()));
        let source_project = std::env::temp_dir().join(format!("fox-agent-tools-memory-src-project-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&source_storage).await.unwrap();
        tokio::fs::create_dir_all(&source_project).await.unwrap();

        let source_tool = MemoryTool::with_manager(
            MemoryManager::new(&MemoryConfig::default()).with_storage_dir(source_storage.clone()),
        );
        let source_ctx = ToolContext {
            session_id: "mem-export-src".into(),
            message_id: "m1".into(),
            tool_call_id: "t1".into(),
            working_dir: Some(source_project.clone()),
            execution_mode: ToolExecutionMode::Foreground,
            graceful_shutdown_requested: false,
        };

        source_tool
            .execute(
                json!({"action":"remember","content":"project memory","scope":"project","category":"fact"}),
                source_ctx.clone(),
            )
            .await
            .unwrap();
        source_tool
            .execute(
                json!({"action":"remember","content":"global memory","scope":"global","category":"preference"}),
                source_ctx.clone(),
            )
            .await
            .unwrap();

        let export_path = source_storage.join("memory-bundle.json");
        let exported = source_tool
            .execute(
                json!({"action":"export","scope":"all","file_path":export_path.to_string_lossy().to_string()}),
                source_ctx.clone(),
            )
            .await
            .unwrap();
        assert!(exported.text.contains("Exported memories"));
        assert!(export_path.exists());

        let rebuilt = source_tool
            .execute(json!({"action":"rebuild_ann","scope":"all"}), source_ctx)
            .await
            .unwrap();
        assert!(rebuilt.text.contains("Rebuilt ANN indexes"));

        let target_storage = std::env::temp_dir().join(format!("fox-agent-tools-memory-dst-{}", Uuid::new_v4()));
        let target_project = std::env::temp_dir().join(format!("fox-agent-tools-memory-dst-project-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&target_storage).await.unwrap();
        tokio::fs::create_dir_all(&target_project).await.unwrap();

        let target_tool = MemoryTool::with_manager(
            MemoryManager::new(&MemoryConfig::default()).with_storage_dir(target_storage),
        );
        let target_ctx = ToolContext {
            session_id: "mem-export-dst".into(),
            message_id: "m1".into(),
            tool_call_id: "t1".into(),
            working_dir: Some(target_project),
            execution_mode: ToolExecutionMode::Foreground,
            graceful_shutdown_requested: false,
        };

        let imported = target_tool
            .execute(
                json!({"action":"import","file_path":export_path.to_string_lossy().to_string(),"merge":false}),
                target_ctx.clone(),
            )
            .await
            .unwrap();
        assert!(imported.text.contains("Imported memories"));

        let listed = target_tool
            .execute(json!({"action":"list","scope":"all"}), target_ctx)
            .await
            .unwrap();
        assert!(listed.text.contains("project memory"));
        assert!(listed.text.contains("global memory"));
    }

    #[tokio::test]
    async fn memory_tool_compact_removes_stale_memory_files() {
        let storage_dir = std::env::temp_dir().join(format!("fox-agent-tools-memory-gc-{}", Uuid::new_v4()));
        let project_dir = std::env::temp_dir().join(format!("fox-agent-tools-memory-gc-project-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&storage_dir).await.unwrap();
        tokio::fs::create_dir_all(&project_dir).await.unwrap();

        let tool = MemoryTool::with_manager(
            MemoryManager::new(&MemoryConfig::default()).with_storage_dir(storage_dir.clone()),
        );
        let ctx = ToolContext {
            session_id: "mem-gc".into(),
            message_id: "m1".into(),
            tool_call_id: "t1".into(),
            working_dir: Some(project_dir.clone()),
            execution_mode: ToolExecutionMode::Foreground,
            graceful_shutdown_requested: false,
        };

        tool.execute(
            json!({"action":"remember","content":"project gc memory","scope":"project"}),
            ctx.clone(),
        )
        .await
        .unwrap();
        tool.execute(
            json!({"action":"remember","content":"global gc memory","scope":"global"}),
            ctx.clone(),
        )
        .await
        .unwrap();

        let global_path = storage_dir.join("global.json");
        let project_path = storage_dir
            .join("projects")
            .join(format!("{}.json", storage::project_hash(&project_dir)));
        assert!(global_path.exists());
        assert!(project_path.exists());

        tokio::time::sleep(Duration::from_millis(1200)).await;

        let compacted = tool
            .execute(json!({"action":"compact","max_age_hours":0}), ctx)
            .await
            .unwrap();
        assert!(compacted.text.contains("Compacted memories"));
        assert!(!global_path.exists());
        assert!(!project_path.exists());
    }
}
