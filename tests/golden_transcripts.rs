//! Goldenscript 集成测试入口。
//!
//! 使用 `test_each_file::test_each_path!` 为 `tests/fixtures/transcripts/`
//! 下的每个 `.gs` 文件自动生成独立测试用例。
//!
//! 运行方式：
//!   cargo test --test golden_transcripts                   # 验证模式
//!   UPDATE_GOLDENFILES=1 cargo test --test golden_transcripts  # 录制模式

use std::collections::HashMap;

use fox_agent_core::{StreamEvent, TokenUsage};

mod golden_runner;
use golden_runner::GoldenRunner;

// ═══════════════════════════════════════════════════════════════════════════════
// Mock Script 注册表
// ═══════════════════════════════════════════════════════════════════════════════
//
// 每个 `.gs` 测试用例按文件名（去扩展名）作为 key，这里注册对应的预录 Mock
// 脚本。`run_agent` 命令执行时，GoldenRunner 按 case_name 查找脚本并通过
// MockProvider 回放。

fn mock_scripts() -> HashMap<String, Vec<Vec<StreamEvent>>> {
    let mut registry: HashMap<String, Vec<Vec<StreamEvent>>> = HashMap::new();

    // ── 001_smoke ────────────────────────────────────────────────────────
    // 冒烟测试：无预录脚本，使用 !run_agent 验证错误处理。
    // 不注册任何脚本。

    // ── 002_create_file ──────────────────────────────────────────────────
    // Agent 创建文件：模拟 write 工具调用 + 最终响应。
    registry.insert(
        "002_create_file".into(),
        vec![
            // Turn 1: assistant proposes write tool call
            vec![
                StreamEvent::ToolUse {
                    id: "t1".into(),
                    name: "write".into(),
                    input: serde_json::json!({
                        "file_path": "/tmp/hello.txt",
                        "content": "Hello World"
                    }),
                },
                StreamEvent::TextDelta {
                    text: "I'll create the file.".into(),
                },
                StreamEvent::MessageStop {
                    stop_reason: Some("tool_use".into()),
                },
            ],
            // Turn 2: after tool execution, assistant confirms
            vec![
                StreamEvent::TextDelta {
                    text: "I have created the file hello.txt with the requested content.".into(),
                },
                StreamEvent::MessageStop {
                    stop_reason: Some("end_turn".into()),
                },
                StreamEvent::Usage {
                    usage: TokenUsage {
                        input_tokens: 45,
                        output_tokens: 12,
                        total_tokens: 57,
                        cache_read_input_tokens: None,
                        cache_creation_input_tokens: None,
                    },
                },
            ],
        ],
    );

    // ── 003_multi_step ───────────────────────────────────────────────────
    // 多步骤工作流：read → write（2 次 tool use + 最终响应 = 3 个脚本）。
    registry.insert(
        "003_multi_step".into(),
        vec![
            // Turn 1: assistant proposes read tool
            vec![
                StreamEvent::ToolUse {
                    id: "t1".into(),
                    name: "read".into(),
                    input: serde_json::json!({"file_path": "/tmp/config.toml"}),
                },
                StreamEvent::MessageStop {
                    stop_reason: Some("tool_use".into()),
                },
            ],
            // Turn 2: assistant proposes write tool (after seeing read result)
            vec![
                StreamEvent::ToolUse {
                    id: "t2".into(),
                    name: "write".into(),
                    input: serde_json::json!({
                        "file_path": "/tmp/config.toml",
                        "content": "[server]\nport = 8080\n"
                    }),
                },
                StreamEvent::MessageStop {
                    stop_reason: Some("tool_use".into()),
                },
            ],
            // Turn 3: final response after both tools
            vec![
                StreamEvent::TextDelta {
                    text: "Updated the config file successfully.".into(),
                },
                StreamEvent::MessageStop {
                    stop_reason: Some("end_turn".into()),
                },
                StreamEvent::Usage {
                    usage: TokenUsage {
                        input_tokens: 150,
                        output_tokens: 35,
                        total_tokens: 185,
                        cache_read_input_tokens: Some(80),
                        cache_creation_input_tokens: None,
                    },
                },
            ],
        ],
    );

    registry
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test Harness
// ═══════════════════════════════════════════════════════════════════════════════

test_each_file::test_each_path! {
    in "tests/fixtures/transcripts" as transcript => test_golden_transcript
}

fn test_golden_transcript(path: &std::path::Path) {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if ext != "gs" {
        return;
    }

    let case_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");

    let registry = mock_scripts();
    let mut runner = GoldenRunner::new(registry);
    runner.set_case(case_name);
    goldenscript::run(&mut runner, path).unwrap();
}

// ═══════════════════════════════════════════════════════════════════════════════
// Self-test: verify mock script registry integrity
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_registry_has_002_create_file() {
        let registry = mock_scripts();
        let scripts = registry.get("002_create_file");
        assert!(
            scripts.is_some(),
            "002_create_file should have mock scripts"
        );
        assert!(!scripts.unwrap().is_empty(), "scripts should not be empty");
    }

    #[test]
    fn mock_registry_has_003_multi_step() {
        let registry = mock_scripts();
        let scripts = registry.get("003_multi_step");
        assert!(scripts.is_some(), "003_multi_step should have mock scripts");
    }

    #[test]
    fn runner_loads_mock_scripts_for_002() {
        let registry = mock_scripts();
        let mut runner = GoldenRunner::new(registry);
        runner.set_case("002_create_file");

        let result = runner.run_agent_sync("create a file");
        // Should succeed with registered mock script
        assert!(
            result.is_ok(),
            "run_agent should work with mock scripts: {:?}",
            result.err()
        );
    }
}
