//! Property-based (fuzz) tests for the fox-agent-sdk framework boundary.
//!
//! Run via:
//! ```bash
//! cargo test --test proptest
//! ```
//!
//! Each test verifies that the framework does not panic on adversarial inputs
//! (malformed JSON, arbitrarily large text, etc.).

use fox_agent_core::{
    DefaultSafetyPolicy, FoxAgentSdkConfig, Message, SafetyConfig, Tool, ToolContext, ToolError,
    ToolExecutionMode, ToolOutput,
};
use fox_agent_sdk::Harness;
use proptest::prelude::*;
use serde_json::Value;
use std::sync::Arc;

// ── Helper: null tool ──

struct NullTool;

#[async_trait::async_trait]
impl Tool for NullTool {
    fn name(&self) -> &str {
        "null"
    }
    fn description(&self) -> &str {
        "null tool"
    }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({"type":"object","properties":{}})
    }
    async fn execute(&self, _input: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput {
            text: "ok".into(),
            is_error: false,
            json: None,
        })
    }
}

// ── Tests ──

proptest! {
    /// Verify that arbitrary tool result text does not cause panic when pushed
    /// into the harness message stream.
    #[test]
    fn arbitrary_tool_result_text_no_panic(text in "\\PC*") {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let harness = Harness::new(FoxAgentSdkConfig::default(), None);
            harness.push_message(Message::tool_result("c1", &text, false)).await;
            let messages = harness.session_messages().await;
            assert!(!messages.is_empty());
        });
    }

    /// Verify that SafetySystem does not panic on arbitrary tool names.
    #[test]
    fn arbitrary_tool_permission_no_panic(tool_name in any::<String>()) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let cfg = FoxAgentSdkConfig {
                safety: SafetyConfig {
                    default_policy: DefaultSafetyPolicy::Allow,
                    ..Default::default()
                },
                ..Default::default()
            };
            let harness = Harness::new(cfg, None);
            let result = harness.check_tool_permission(&tool_name, &serde_json::json!({})).await;
            let _ = result; // must not panic
        });
    }

    /// Verify that SafetySystem with denylist does not panic.
    #[test]
    fn denylist_arbitrary_no_panic(
        entries in prop::collection::vec(any::<String>(), 0..5),
        tool_name in any::<String>(),
    ) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let cfg = FoxAgentSdkConfig {
                safety: SafetyConfig {
                    tool_denylist: Some(entries),
                    default_policy: DefaultSafetyPolicy::Allow,
                    ..Default::default()
                },
                ..Default::default()
            };
            let harness = Harness::new(cfg, None);
            let result = harness.check_tool_permission(&tool_name, &serde_json::json!({})).await;
            let _ = result;
        });
    }

    /// Verify that arbitrary byte arrays as tool input don't panic tool execution.
    #[test]
    fn arbitrary_tool_input_no_panic(input in prop::collection::vec(any::<u8>(), 0..1024)) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let tool = Arc::new(NullTool);
            let val = serde_json::to_value(input).unwrap_or(Value::Null);
            let ctx = ToolContext {
                session_id: "fuzz".into(),
                message_id: "f1".into(),
                tool_call_id: "f1".into(),
                working_dir: None,
                execution_mode: ToolExecutionMode::Foreground,
                graceful_shutdown_requested: false,
                progress_tx: None,
            };
            let _ = tool.execute(val, ctx).await;
        });
    }
}
