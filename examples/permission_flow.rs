/// Permission Flow — demonstrates fine-grained tool permissions with
/// approval caching and audit trail, built via `AgentBuilder`.
///
/// Covers:
/// - Loading `agent.toml` + `AGENTS.md` via `AgentBuilder`
/// - `SafetyConfig` with allowlist / denylist / default policy
/// - Runtime permission checking through the agent's harness
/// - `ApprovalManager` with 3-tier caching (turn / session / workspace)
/// - Permission audit export to JSONL
///
/// Runs without any LLM dependencies.
use fox_agent_sdk::{
    AgentBuilder, ApprovalManager, ApprovalScope, DefaultSafetyPolicy, FoxAgentSdkConfig,
    MockProvider, PermissionRequest, PermissionResult, SafetyConfig,
};
use std::path::PathBuf;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    println!("=== Fox Agent SDK — Permission Flow Demo ===\n");

    // ── Load project config ──
    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cfg = FoxAgentSdkConfig::load_from_file(project_root.join("agent.toml"))
        .unwrap_or_else(|_| FoxAgentSdkConfig::default());

    // ── Customise safety policy (overrides agent.toml) ──
    let safety = SafetyConfig {
        default_policy: DefaultSafetyPolicy::Allow,
        tool_denylist: Some(vec!["echo".to_string()]),
        tool_allowlist: None,
        mcp_auto_approve_servers: Some(vec!["akshare".to_string()]),
        ..Default::default()
    };
    println!("> Safety config: default=Allow, denylist=[echo], mcp_auto_approve=[akshare]\n");

    // ── Build agent via AgentBuilder ──
    let provider = Arc::new(MockProvider::new("mock-permission"));
    let agent = AgentBuilder::new()
        .working_dir(&project_root)
        .sdk_config(cfg)
        .with_global_agents_md_path(project_root.join("AGENTS.md"))
        .with_safety_policy(safety)
        .with_provider(provider)
        .model_id("mock-1")
        .with_default_tools()
        .build()
        .await
        .expect("build agent");

    println!("> Agent built successfully.  Checking tool permissions...\n");

    // ── Demonstrate permission checking via harness ──
    let tool_input = serde_json::json!({});

    // "read" — not in denylist, default=Allow → allowed
    let result = agent
        .harness()
        .check_tool_permission("read", &tool_input)
        .await;
    println!("  [read]       → {:?}", result);

    // "echo" — in denylist → AskUser
    let result = agent
        .harness()
        .check_tool_permission("echo", &tool_input)
        .await;
    println!("  [echo]       → {:?}", result);

    // "write" — not in denylist, but `productive_tool_confirm=true` → AskUser
    let result = agent
        .harness()
        .check_tool_permission("write", &tool_input)
        .await;
    println!("  [write]      → {:?}", result);

    // MCP tool from auto-approved server → Allowed
    let result = agent
        .harness()
        .check_tool_permission("mcp__akshare__get_news_data", &tool_input)
        .await;
    println!("  [mcp_akshare] → {:?}", result);

    // MCP tool from non-approved server → default=Allow → Allowed
    let result = agent
        .harness()
        .check_tool_permission("mcp__filesystem__read", &tool_input)
        .await;
    println!("  [mcp_filesys] → {:?}\n", result);

    // ── ApprovalManager with caching ──
    let approval = ApprovalManager::new("demo-session", SafetyConfig::default());

    // Cache "read" approvals for this session — skip re-prompting
    approval
        .cache_decision("read", &PermissionResult::Allow, ApprovalScope::ThisSession)
        .await;
    println!("> `read` auto-approved for this session via cache\n");

    // Verify cache hit
    let cached = approval.check_cache("read").await;
    println!("> Cache check for 'read': {:?}", cached);

    // Cache miss for an unknown tool
    let miss = approval.check_cache("write").await;
    println!("> Cache check for 'write': {:?} (expected None)\n", miss);

    // ── Record audit entries ──
    let request = PermissionRequest::new("read", "Read Cargo.toml");
    approval
        .record_audit(&request, &PermissionResult::Allow, 42)
        .await;

    let request2 = PermissionRequest::new("bash", "rm -rf /tmp/test").with_risk(
        fox_agent_sdk::RiskLevel::High,
        "denylist",
        "Execute bash command: rm -rf /tmp/test",
    );
    approval
        .record_audit(
            &request2,
            &PermissionResult::Deny {
                reason: "denylist".into(),
            },
            43,
        )
        .await;

    // ── Export audit trail ──
    let audit_path = std::env::temp_dir().join("permission_audit_demo.jsonl");
    approval.export_audit(&audit_path).await.unwrap();
    println!("> Audit trail exported to {}", audit_path.display());
    println!("\nDone.");
}
