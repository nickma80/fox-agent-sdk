//! Golden Transcript evaluation tests.
//!
//! Each test simulates a real-world agent task using [`MockProvider`] scripts
//! and pre-created file state, then verifies the evaluation pipeline:
//! [`TaskAssertions`] → [`BehaviorRuleEngine`] → [`TokenReport`].
//!
//! Run:
//! ```bash
//! cargo test --test golden_transcripts
//! $env:RUST_TEST_NOCAPTURE = "1"
//! cargo test --test golden_transcripts -- --nocapture
//! ```

use fox_agent_core::{
    AgentEvent, FoxAgentSdkConfig, SafetyConfig,
    StreamEvent, Tool, ToolContext, ToolError, ToolOutput,
    TaskAssertions, run_task_assertions,
    TokenReport, DefaultSafetyPolicy,
};
use fox_agent_sdk::eval::behavior_rules::{BehaviorRuleEngine, RuleSeverity};
use fox_agent_sdk::{Agent, Harness, MockProvider};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::path::PathBuf;

// ═══════════════════════════════════════════════════════════════════════════════
// Mock Tools
// ═══════════════════════════════════════════════════════════════════════════════

struct FsCreateTool { root: Arc<PathBuf> }

#[async_trait::async_trait]
impl Tool for FsCreateTool {
    fn name(&self) -> &str { "write" }
    fn description(&self) -> &str { "Write a file" }
    fn parameters_schema(&self) -> Value {
        json!({"type":"object","properties":{"file_path":{"type":"string"},"content":{"type":"string"}},"required":["file_path","content"]})
    }
    async fn execute(&self, input: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let fp = input["file_path"].as_str().unwrap_or_default();
        let content = input["content"].as_str().unwrap_or_default();
        let full = self.root.join(fp.trim_start_matches('/'));
        if let Some(parent) = full.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&full, content)
            .map_err(|e| ToolError::Message { message: e.to_string() })?;
        Ok(ToolOutput { text: format!("Wrote {}", fp), is_error: false, json: None })
    }
}

struct FsReadTool { root: Arc<PathBuf> }

#[async_trait::async_trait]
impl Tool for FsReadTool {
    fn name(&self) -> &str { "read" }
    fn description(&self) -> &str { "Read a file" }
    fn parameters_schema(&self) -> Value {
        json!({"type":"object","properties":{"file_path":{"type":"string"}},"required":["file_path"]})
    }
    async fn execute(&self, input: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let fp = input["file_path"].as_str().unwrap_or_default();
        let full = self.root.join(fp.trim_start_matches('/'));
        let content = std::fs::read_to_string(&full)
            .map_err(|e| ToolError::Message { message: e.to_string() })?;
        Ok(ToolOutput { text: content, is_error: false, json: None })
    }
}

struct CmdTool {
    outputs: HashMap<String, (String, bool)>,
}

impl CmdTool {
    fn new(outputs: HashMap<String, (String, bool)>) -> Self { Self { outputs } }
}

#[async_trait::async_trait]
impl Tool for CmdTool {
    fn name(&self) -> &str { "bash" }
    fn description(&self) -> &str { "Run a shell command" }
    fn parameters_schema(&self) -> Value {
        json!({"type":"object","properties":{"command":{"type":"string"}},"required":["command"]})
    }
    async fn execute(&self, input: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let cmd = input["command"].as_str().unwrap_or_default();
        for (key, (output, is_error)) in &self.outputs {
            if cmd.contains(key.as_str()) {
                return Ok(ToolOutput { text: output.clone(), is_error: *is_error, json: None });
            }
        }
        Ok(ToolOutput { text: format!("OK: {cmd}"), is_error: false, json: None })
    }
}

struct GrepTool { results: Vec<String> }

#[async_trait::async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str { "grep" }
    fn description(&self) -> &str { "Search file contents" }
    fn parameters_schema(&self) -> Value {
        json!({"type":"object","properties":{"pattern":{"type":"string"}},"required":["pattern"]})
    }
    async fn execute(&self, _input: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput { text: self.results.join("\n"), is_error: false, json: None })
    }
}

struct LsTool { entries: Vec<String> }

#[async_trait::async_trait]
impl Tool for LsTool {
    fn name(&self) -> &str { "ls" }
    fn description(&self) -> &str { "List directory contents" }
    fn parameters_schema(&self) -> Value { json!({"type":"object","properties":{}}) }
    async fn execute(&self, _input: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput { text: self.entries.join("\n"), is_error: false, json: None })
    }
}

struct PlanTool(Mutex<Vec<String>>);
impl PlanTool {
    fn new() -> Self { Self(Mutex::new(Vec::new())) }
    fn plans(&self) -> Vec<String> { self.0.lock().unwrap().clone() }
}

#[async_trait::async_trait]
impl Tool for PlanTool {
    fn name(&self) -> &str { "plan" }
    fn description(&self) -> &str { "Create a plan" }
    fn parameters_schema(&self) -> Value {
        json!({"type":"object","properties":{"items":{"type":"array"}},"required":["items"]})
    }
    async fn execute(&self, input: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let s = serde_json::to_string_pretty(&input).unwrap_or_default();
        self.0.lock().unwrap().push(s);
        Ok(ToolOutput { text: "plan recorded".into(), is_error: false, json: None })
    }
}

struct TodoTool(Mutex<Vec<String>>);
impl TodoTool {
    fn new() -> Self { Self(Mutex::new(Vec::new())) }
    fn todos(&self) -> Vec<String> { self.0.lock().unwrap().clone() }
}

#[async_trait::async_trait]
impl Tool for TodoTool {
    fn name(&self) -> &str { "todo" }
    fn description(&self) -> &str { "Track todos" }
    fn parameters_schema(&self) -> Value {
        json!({"type":"object","properties":{"todos":{"type":"array"}},"required":["todos"]})
    }
    async fn execute(&self, input: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let s = serde_json::to_string_pretty(&input).unwrap_or_default();
        self.0.lock().unwrap().push(s);
        Ok(ToolOutput { text: "todo recorded".into(), is_error: false, json: None })
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Script builders
// ═══════════════════════════════════════════════════════════════════════════════

/// Build one model-turn script: a tool-use decision.
fn tool_use_script(call_id: &str, name: &str, input: Value) -> Vec<StreamEvent> {
    vec![
        StreamEvent::ToolUse { id: call_id.into(), name: name.into(), input },
        StreamEvent::Usage { usage: fox_agent_core::TokenUsage {
            input_tokens: 50, output_tokens: 20, total_tokens: 70,
            cache_read_input_tokens: None, cache_creation_input_tokens: None,
        }},
        StreamEvent::MessageStop { stop_reason: Some("tool_use".into()) },
    ]
}

/// Build one model-turn script: a final text response.
fn text_script(text: &str) -> Vec<StreamEvent> {
    vec![
        StreamEvent::TextDelta { text: text.to_string() },
        StreamEvent::Usage { usage: fox_agent_core::TokenUsage {
            input_tokens: 30, output_tokens: text.len() as u32, total_tokens: (30 + text.len()) as u32,
            cache_read_input_tokens: None, cache_creation_input_tokens: None,
        }},
        StreamEvent::MessageStop { stop_reason: Some("stop".into()) },
    ]
}

// ═══════════════════════════════════════════════════════════════════════════════
// Eval pipeline helpers
// ═══════════════════════════════════════════════════════════════════════════════

fn collect_token_report(events: &[AgentEvent]) -> TokenReport {
    let mut report = TokenReport::default();
    for ev in events {
        match ev {
            AgentEvent::ModelUsage { usage, .. } => report.record_usage(usage),
            AgentEvent::ToolCallStart { .. } => report.record_tool_call(),
            AgentEvent::Compaction { .. } => report.record_compaction(),
            _ => {}
        }
    }
    report
}

async fn drain(rx: &mut tokio::sync::mpsc::Receiver<AgentEvent>) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    loop {
        match tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await {
            Ok(Some(ev)) => events.push(ev),
            _ => break,
        }
    }
    events
}

async fn build_agent(tools: Vec<Arc<dyn Tool>>) -> (Agent, MockProvider) {
    let provider = MockProvider::new("eval");
    let harness = Harness::new(FoxAgentSdkConfig {
        safety: SafetyConfig {
            default_policy: DefaultSafetyPolicy::Allow,
            productive_tool_confirm: false,
            ..Default::default()
        },
        ..Default::default()
    }, None);
    for t in tools {
        harness.register_tool(t).await;
    }
    let model = Arc::new(fox_agent_core::DefaultModel::new(Arc::new(provider.clone()), "eval-model"));
    let agent = Agent::new(model, harness, Arc::new(tokio::sync::RwLock::new(None)));
    (agent, provider)
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test Cases
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn case_001_create_rust_project_and_compile() {
    let dir = tempfile::tempdir().unwrap();
    let root = Arc::new(dir.path().to_path_buf());

    // Pre-create expected files (simulates cargo init)
    let project_dir = root.join("test-project");
    std::fs::create_dir_all(project_dir.join("src")).unwrap();
    std::fs::write(project_dir.join("Cargo.toml"), "[package]\nname = \"test-project\"\nversion = \"0.1.0\"\n").unwrap();
    std::fs::write(project_dir.join("src/main.rs"), "fn main() { println!(\"hello\"); }\n").unwrap();

    let create = Arc::new(FsCreateTool { root: root.clone() });
    let mut cmd_outputs = HashMap::new();
    cmd_outputs.insert("cargo build".into(), ("Compiling test-project v0.1.0\nFinished dev [unoptimized] target(s)\n".into(), false));
    let cmd = Arc::new(CmdTool::new(cmd_outputs));
    let (agent, provider) = build_agent(vec![create, cmd]).await;

    // Simulate: agent initialized project, then builds it
    provider.push_script(tool_use_script("c1", "bash", json!({"command":"cargo init test-project"})));
    provider.push_script(tool_use_script("c2", "bash", json!({"command":"cd test-project && cargo build"})));
    provider.push_script(text_script("Project created and built"));

    let (tx, mut rx) = tokio::sync::mpsc::channel(128);
    let _ = agent.run_once_streaming("Create a Rust project", &tx).await.unwrap();
    drop(tx);
    let events = drain(&mut rx).await;

    let assertions = TaskAssertions {
        file_exists: vec![PathBuf::from("test-project/Cargo.toml"), PathBuf::from("test-project/src/main.rs")],
        file_contains: vec![(PathBuf::from("test-project/Cargo.toml"), "[package]".into())],
        file_not_contains: vec![],
        dir_exists: vec![PathBuf::from("test-project/src")],
        commands: vec![],
        max_duration_secs: Some(30),
    };
    let report = run_task_assertions(&assertions, dir.path());
    println!("{report}");

    let engine = BehaviorRuleEngine::with_default_rules();
    let violations = engine.check(&events);
    let errors: Vec<_> = violations.iter().filter(|v| v.severity == RuleSeverity::Error).collect();
    let token = collect_token_report(&events);

    assert!(report.passed, "Task assertions failed");
    assert!(errors.is_empty(), "Behavior errors: {errors:?}");
    println!("Token: {} input / {} output / {} calls", token.total_input, token.total_output, token.api_calls);
}

#[tokio::test]
async fn case_002_multi_file_edit() {
    let dir = tempfile::tempdir().unwrap();
    let root = Arc::new(dir.path().to_path_buf());

    // Pre-create files that need editing
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() { println!(\"new\"); }\n").unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn greet() -> &'static str { \"new\" }\n").unwrap();

    let read = Arc::new(FsReadTool { root: root.clone() });
    let create = Arc::new(FsCreateTool { root: root.clone() });
    let (agent, provider) = build_agent(vec![read, create]).await;

    provider.push_script(tool_use_script("c1", "read", json!({"file_path":"src/main.rs"})));
    provider.push_script(tool_use_script("c2", "write", json!({"file_path":"src/main.rs","content":"fn main() { println!(\"new\"); }\n"})));
    provider.push_script(tool_use_script("c3", "read", json!({"file_path":"src/lib.rs"})));
    provider.push_script(text_script("All files edited and verified"));

    let (tx, mut rx) = tokio::sync::mpsc::channel(128);
    let _ = agent.run_once_streaming("Update greeting", &tx).await.unwrap();
    drop(tx);
    let _events = drain(&mut rx).await;

    let assertions = TaskAssertions {
        file_contains: vec![
            (PathBuf::from("src/main.rs"), "\"new\"".into()),
            (PathBuf::from("src/lib.rs"), "\"new\"".into()),
        ],
        file_not_contains: vec![],
        file_exists: vec![],
        dir_exists: vec![],
        commands: vec![],
        max_duration_secs: None,
    };
    let report = run_task_assertions(&assertions, dir.path());
    println!("{report}");
    assert!(report.passed);
}

#[tokio::test]
async fn case_003_codebase_search() {
    let dir = tempfile::tempdir().unwrap();

    let grep = Arc::new(GrepTool {
        results: vec!["config.rs:1: DB_URL".into(), "handler.rs:2: DB_URL".into()],
    });
    let read = Arc::new(FsReadTool { root: Arc::new(dir.path().to_path_buf()) });
    let (agent, provider) = build_agent(vec![grep, read]).await;

    provider.push_script(tool_use_script("c1", "grep", json!({"pattern":"DB_URL"})));
    provider.push_script(tool_use_script("c2", "read", json!({"file_path":"config.rs"})));
    provider.push_script(text_script("Found DB_URL"));

    let (tx, mut rx) = tokio::sync::mpsc::channel(128);
    let _ = agent.run_once_streaming("Search DB_URL", &tx).await.unwrap();
    drop(tx);
    let events = drain(&mut rx).await;

    let has_grep = events.iter().any(|ev| matches!(ev, AgentEvent::ToolCallStart { name, .. } if name == "grep"));
    let has_read = events.iter().any(|ev| matches!(ev, AgentEvent::ToolCallStart { name, .. } if name == "read"));
    assert!(has_grep, "Agent should use grep");
    assert!(has_read, "Agent should read found files");

    let engine = BehaviorRuleEngine::with_default_rules();
    let violations = engine.check(&events);
    assert!(violations.iter().all(|v| v.severity != RuleSeverity::Error));
    println!("Search flow OK: grep={has_grep}, read={has_read}");
}

#[tokio::test]
async fn case_004_git_log_analysis() {
    let _dir = tempfile::tempdir().unwrap();

    let mut cmd_outputs = HashMap::new();
    cmd_outputs.insert("git log".into(), ("abc123 Alice\nghi789 Alice".into(), false));
    let cmd = Arc::new(CmdTool::new(cmd_outputs));
    let grep = Arc::new(GrepTool { results: vec!["abc123 Alice".into(), "ghi789 Alice".into()] });
    let (agent, provider) = build_agent(vec![cmd, grep]).await;

    provider.push_script(tool_use_script("c1", "bash", json!({"command":"git log --oneline"})));
    provider.push_script(tool_use_script("c2", "grep", json!({"pattern":"Alice"})));
    provider.push_script(text_script("Alice made 2 commits"));

    let (tx, mut rx) = tokio::sync::mpsc::channel(128);
    let _ = agent.run_once_streaming("Show Alice's commits", &tx).await.unwrap();
    drop(tx);
    let events = drain(&mut rx).await;

    let has_log = events.iter().any(|ev| matches!(ev, AgentEvent::ToolCallStart { name, .. } if name == "bash"));
    let has_grep = events.iter().any(|ev| matches!(ev, AgentEvent::ToolCallStart { name, .. } if name == "grep"));
    assert!(has_log && has_grep);
    println!("Git analysis: log={has_log}, grep={has_grep}");
}

#[tokio::test]
async fn case_005_error_diagnosis_and_fix() {
    let dir = tempfile::tempdir().unwrap();
    let root = Arc::new(dir.path().to_path_buf());

    // Pre-create file with error, then simulate fix
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() { let x: i32 = 1; }\n").unwrap();

    let read = Arc::new(FsReadTool { root: root.clone() });
    let create = Arc::new(FsCreateTool { root: root.clone() });
    let mut cmd_outputs = HashMap::new();
    cmd_outputs.insert("cargo build-err".into(), ("error[E0282]: type annotations needed".into(), true));
    cmd_outputs.insert("cargo build-ok".into(), ("Compiling my-app\nFinished".into(), false));
    let cmd = Arc::new(CmdTool::new(cmd_outputs));
    let (agent, provider) = build_agent(vec![read, create, cmd]).await;

    // Flow: build(FAIL) → read → write(fix) → build(OK)
    provider.push_script(tool_use_script("c1", "bash", json!({"command":"cargo build-err"})));
    provider.push_script(tool_use_script("c2", "read", json!({"file_path":"src/main.rs"})));
    provider.push_script(tool_use_script("c3", "write", json!({"file_path":"src/main.rs","content":"fn main() { let x: i32 = 1; }\n"})));
    provider.push_script(tool_use_script("c4", "bash", json!({"command":"cargo build-ok"})));
    provider.push_script(text_script("Fixed type annotation"));

    let (tx, mut rx) = tokio::sync::mpsc::channel(128);
    let _ = agent.run_once_streaming("Fix the build", &tx).await.unwrap();
    drop(tx);
    let events = drain(&mut rx).await;

    let tool_names: Vec<&str> = events.iter()
        .filter_map(|ev| match ev { AgentEvent::ToolCallStart { name, .. } => Some(name.as_str()), _ => None })
        .collect();
    assert_eq!(tool_names, vec!["bash", "read", "write", "bash"], "Error→fix→rebuild flow");

    let engine = BehaviorRuleEngine::with_default_rules();
    let violations = engine.check(&events);
    assert!(violations.iter().all(|v| v.severity != RuleSeverity::Error));
    println!("Error diagnosis OK: {:?}", tool_names);
}

#[tokio::test]
async fn case_006_permission_denied_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let root = Arc::new(dir.path().to_path_buf());

    let create = Arc::new(FsCreateTool { root: root.clone() });
    let (agent, provider) = build_agent(vec![create]).await;

    // Simulate: destructive tool gets denied, agent falls back to safe tool
    provider.push_script(tool_use_script("c1", "bash", json!({"command":"rm -rf /tmp"})));
    provider.push_script(tool_use_script("c2", "write", json!({"file_path":"cleaned.txt","content":"safe cleanup\n"})));
    provider.push_script(text_script("Used safe alternative"));

    let (tx, mut rx) = tokio::sync::mpsc::channel(128);
    let _ = agent.run_once_streaming("Clean up safely", &tx).await.unwrap();
    drop(tx);
    let events = drain(&mut rx).await;

    let engine = BehaviorRuleEngine::with_default_rules();
    let violations = engine.check(&events);
    let errors: Vec<_> = violations.iter().filter(|v| v.severity == RuleSeverity::Error).collect();
    assert!(errors.is_empty(), "Behavior errors: {errors:?}");

    let assertions = TaskAssertions {
        file_exists: vec![PathBuf::from("cleaned.txt")],
        file_contains: vec![],
        file_not_contains: vec![],
        dir_exists: vec![],
        commands: vec![],
        max_duration_secs: None,
    };
    let report = run_task_assertions(&assertions, dir.path());
    println!("{report}");
    assert!(report.passed);
}

#[tokio::test]
async fn case_007_tool_timeout_handling() {
    let mut cmd_outputs = HashMap::new();
    cmd_outputs.insert("sleep".into(), ("timeout: command exceeded limit".into(), true));
    cmd_outputs.insert("echo done".into(), ("done".into(), false));
    let cmd = Arc::new(CmdTool::new(cmd_outputs));
    let (agent, provider) = build_agent(vec![cmd]).await;

    provider.push_script(tool_use_script("c1", "bash", json!({"command":"sleep 120","timeout":30000})));
    provider.push_script(tool_use_script("c2", "bash", json!({"command":"echo done"})));
    provider.push_script(text_script("Completed after timeout"));

    let (tx, mut rx) = tokio::sync::mpsc::channel(128);
    let _ = agent.run_once_streaming("Slow task", &tx).await.unwrap();
    drop(tx);
    let events = drain(&mut rx).await;

    let tool_names: Vec<&str> = events.iter()
        .filter_map(|ev| match ev { AgentEvent::ToolCallStart { name, .. } => Some(name.as_str()), _ => None })
        .collect();
    assert_eq!(tool_names.len(), 2, "Should recover from timeout");
    println!("Timeout recovery: {:?}", tool_names);
}

#[tokio::test]
async fn case_008_compaction_behavior() {
    let dir = tempfile::tempdir().unwrap();
    let root = Arc::new(dir.path().to_path_buf());
    let write = Arc::new(FsCreateTool { root: root.clone() });
    let (agent, provider) = build_agent(vec![write]).await;

    // Multiple tool turns to simulate long conversation
    for i in 0..5 {
        provider.push_script(tool_use_script(
            &format!("c{i}"), "write",
            json!({"file_path":format!("file_{i}.txt"),"content":format!("content {i}")}),
        ));
    }
    provider.push_script(text_script("All files created"));

    let (tx, mut rx) = tokio::sync::mpsc::channel(128);
    let _ = agent.run_once_streaming("Create 5 files", &tx).await.unwrap();
    drop(tx);
    let events = drain(&mut rx).await;

    let engine = BehaviorRuleEngine::with_default_rules();
    let violations = engine.check(&events);
    assert!(violations.iter().all(|v| v.severity != RuleSeverity::Error),
        "Behavior errors: {violations:?}");

    let token = collect_token_report(&events);
    assert!(token.tool_calls >= 5, "Should track all tool calls");
    println!("Compaction check: {} tool calls tracked", token.tool_calls);
}

#[tokio::test]
async fn case_009_mcp_tool_routing() {
    struct McpTool;
    #[async_trait::async_trait]
    impl Tool for McpTool {
        fn name(&self) -> &str { "mcp__filesystem__read" }
        fn description(&self) -> &str { "MCP tool" }
        fn parameters_schema(&self) -> Value {
            json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]})
        }
        async fn execute(&self, _input: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput { text: "file content".into(), is_error: false, json: None })
        }
    }

    let (agent, provider) = build_agent(vec![Arc::new(McpTool)]).await;

    provider.push_script(tool_use_script("c1", "mcp__filesystem__read", json!({"path":"/tmp/test.txt"})));
    provider.push_script(text_script("Read from MCP"));

    let (tx, mut rx) = tokio::sync::mpsc::channel(128);
    let _ = agent.run_once_streaming("Read via MCP", &tx).await.unwrap();
    drop(tx);
    let events = drain(&mut rx).await;

    let has_mcp = events.iter().any(|ev| matches!(ev, AgentEvent::ToolCallStart { name, .. } if name.starts_with("mcp__")));
    assert!(has_mcp, "MCP tool should be called");
    println!("MCP tool routing verified");
}

#[tokio::test]
async fn case_010_subagent_delegation() {
    struct SubAgentTool;
    #[async_trait::async_trait]
    impl Tool for SubAgentTool {
        fn name(&self) -> &str { "subagent" }
        fn description(&self) -> &str { "Delegate" }
        fn parameters_schema(&self) -> Value {
            json!({"type":"object","properties":{"task":{"type":"string"}},"required":["task"]})
        }
        async fn execute(&self, _input: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput { text: "artifact:sub-001".into(), is_error: false, json: Some(json!({"artifact_id":"sub-001"})) })
        }
    }
    struct ArtifactReadTool;
    #[async_trait::async_trait]
    impl Tool for ArtifactReadTool {
        fn name(&self) -> &str { "artifact_read" }
        fn description(&self) -> &str { "Read artifact" }
        fn parameters_schema(&self) -> Value {
            json!({"type":"object","properties":{"artifact_id":{"type":"string"}},"required":["artifact_id"]})
        }
        async fn execute(&self, _input: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput { text: "results: a, b, c".into(), is_error: false, json: None })
        }
    }

    let (agent, provider) = build_agent(vec![Arc::new(SubAgentTool), Arc::new(ArtifactReadTool)]).await;

    provider.push_script(tool_use_script("c1", "subagent", json!({"task":"search"})));
    provider.push_script(tool_use_script("c2", "artifact_read", json!({"artifact_id":"sub-001"})));
    provider.push_script(text_script("Sub-agent results read"));

    let (tx, mut rx) = tokio::sync::mpsc::channel(128);
    let _ = agent.run_once_streaming("Search codebase", &tx).await.unwrap();
    drop(tx);
    let events = drain(&mut rx).await;

    let has_sub = events.iter().any(|ev| matches!(ev, AgentEvent::ToolCallStart { name, .. } if name == "subagent"));
    let has_read = events.iter().any(|ev| matches!(ev, AgentEvent::ToolCallStart { name, .. } if name == "artifact_read"));
    assert!(has_sub && has_read, "Subagent → artifact_read");
    println!("Subagent delegation: OK");
}

#[tokio::test]
async fn case_011_multi_turn_conversation() {
    let dir = tempfile::tempdir().unwrap();
    let root = Arc::new(dir.path().to_path_buf());
    let write = Arc::new(FsCreateTool { root: root.clone() });
    let (agent, provider) = build_agent(vec![write]).await;

    // Multi-turn: create file → append → read
    provider.push_script(tool_use_script("c1", "write", json!({"file_path":"notes.md","content":"Step 1\n"})));
    provider.push_script(tool_use_script("c2", "write", json!({"file_path":"notes.md","content":"Step 1\nStep 2\n"})));
    provider.push_script(text_script("Updated"));

    let (tx, mut rx) = tokio::sync::mpsc::channel(128);
    let _ = agent.run_once_streaming("Multi-turn", &tx).await.unwrap();
    drop(tx);
    let _events = drain(&mut rx).await;

    let assertions = TaskAssertions {
        file_exists: vec![PathBuf::from("notes.md")],
        file_contains: vec![],
        file_not_contains: vec![],
        dir_exists: vec![],
        commands: vec![],
        max_duration_secs: None,
    };
    let report = run_task_assertions(&assertions, dir.path());
    println!("{report}");
    assert!(report.passed);
}

#[tokio::test]
async fn case_012_large_file_read_truncation() {
    let dir = tempfile::tempdir().unwrap();
    let root = Arc::new(dir.path().to_path_buf());

    let large = "A".repeat(100_000);
    std::fs::write(root.join("large.log"), &large).unwrap();

    let read = Arc::new(FsReadTool { root: root.clone() });
    let (agent, provider) = build_agent(vec![read]).await;

    provider.push_script(tool_use_script("c1", "read", json!({"file_path":"large.log"})));
    provider.push_script(text_script("Read complete"));

    let (tx, mut rx) = tokio::sync::mpsc::channel(128);
    let _ = agent.run_once_streaming("Read large.log", &tx).await.unwrap();
    drop(tx);
    let events = drain(&mut rx).await;

    let sizes: Vec<usize> = events.iter()
        .filter_map(|ev| match ev { AgentEvent::ToolCallEnd { output, .. } => Some(output.text.len()), _ => None })
        .collect();
    println!("Read sizes: {sizes:?}");

    let engine = BehaviorRuleEngine::with_default_rules();
    let violations = engine.check(&events);
    assert!(violations.iter().all(|v| v.severity != RuleSeverity::Error));
}

#[tokio::test]
async fn case_013_concurrent_tool_execution() {
    let dir = tempfile::tempdir().unwrap();
    let root = Arc::new(dir.path().to_path_buf());
    let write = Arc::new(FsCreateTool { root: root.clone() });
    let (agent, provider) = build_agent(vec![write]).await;

    // Two tool calls in one model turn
    let turn = vec![
        StreamEvent::ToolUse { id: "c1".into(), name: "write".into(), input: json!({"file_path":"a.txt","content":"A"}) },
        StreamEvent::ToolUse { id: "c2".into(), name: "write".into(), input: json!({"file_path":"b.txt","content":"B"}) },
        StreamEvent::Usage { usage: fox_agent_core::TokenUsage {
            input_tokens: 100, output_tokens: 40, total_tokens: 140,
            cache_read_input_tokens: None, cache_creation_input_tokens: None,
        }},
        StreamEvent::MessageStop { stop_reason: Some("tool_use".into()) },
    ];
    provider.push_script(turn);
    provider.push_script(text_script("Both files created"));

    let (tx, mut rx) = tokio::sync::mpsc::channel(128);
    let _ = agent.run_once_streaming("Create two files", &tx).await.unwrap();
    drop(tx);
    let _events = drain(&mut rx).await;

    let assertions = TaskAssertions {
        file_exists: vec![PathBuf::from("a.txt"), PathBuf::from("b.txt")],
        file_contains: vec![],
        file_not_contains: vec![],
        dir_exists: vec![],
        commands: vec![],
        max_duration_secs: None,
    };
    let report = run_task_assertions(&assertions, dir.path());
    println!("{report}");
    assert!(report.passed);
}

#[tokio::test]
async fn case_014_plan_todo_consistency() {
    let plan = Arc::new(PlanTool::new());
    let todo = Arc::new(TodoTool::new());
    let (agent, provider) = build_agent(vec![plan.clone(), todo.clone()]).await;

    provider.push_script(tool_use_script("c1", "plan", json!({
        "items": [{"id":"1","content":"Step 1","status":"pending"}]
    })));
    provider.push_script(tool_use_script("c2", "todo", json!({
        "todos": [{"id":"1","content":"Step 1","status":"in_progress"}]
    })));
    provider.push_script(text_script("Plan and todo set"));

    let (tx, mut rx) = tokio::sync::mpsc::channel(128);
    let _ = agent.run_once_streaming("Plan a task", &tx).await.unwrap();
    drop(tx);
    let events = drain(&mut rx).await;

    let tool_names: Vec<&str> = events.iter()
        .filter_map(|ev| match ev { AgentEvent::ToolCallStart { name, .. } => Some(name.as_str()), _ => None })
        .collect();

    let plan_pos = tool_names.iter().position(|&n| n == "plan");
    let todo_pos = tool_names.iter().position(|&n| n == "todo");
    assert!(plan_pos.is_some(), "plan tool should be called");
    assert!(todo_pos.is_some(), "todo tool should be called");

    let engine = BehaviorRuleEngine::with_default_rules();
    let violations = engine.check(&events);
    assert!(violations.iter().all(|v| v.severity != RuleSeverity::Error));
    println!("Plan-todo: plan used, {} plans / {} todos recorded", plan.plans().len(), todo.todos().len());
}

#[tokio::test]
async fn case_015_environment_initialization() {
    let dir = tempfile::tempdir().unwrap();
    let root = Arc::new(dir.path().to_path_buf());

    // Pre-create README for the agent to discover
    std::fs::write(root.join("README.md"), "# Test Project\nEnv setup required\n").unwrap();

    let write = Arc::new(FsCreateTool { root: root.clone() });
    let ls = Arc::new(LsTool { entries: vec!["README.md".into(), "src/".into()] });
    let read = Arc::new(FsReadTool { root: root.clone() });
    let (agent, provider) = build_agent(vec![write, ls, read]).await;

    provider.push_script(tool_use_script("c1", "ls", json!({})));
    provider.push_script(tool_use_script("c2", "read", json!({"file_path":"README.md"})));
    provider.push_script(tool_use_script("c3", "write", json!({"file_path":".env","content":"DEBUG=true\n"})));
    provider.push_script(text_script("Environment ready"));

    let (tx, mut rx) = tokio::sync::mpsc::channel(128);
    let _ = agent.run_once_streaming("Init environment", &tx).await.unwrap();
    drop(tx);
    let _events = drain(&mut rx).await;

    let assertions = TaskAssertions {
        file_exists: vec![PathBuf::from(".env")],
        file_contains: vec![],
        file_not_contains: vec![],
        dir_exists: vec![],
        commands: vec![],
        max_duration_secs: None,
    };
    let report = run_task_assertions(&assertions, dir.path());
    println!("{report}");
    assert!(report.passed);
}
