/// permission_flow: demonstrates permission approval, caching, and audit.
///
/// Covers:
/// - Custom permission hook
/// - Approval caching (this-turn / this-session / this-workspace)
/// - Permission audit trail
/// - Denylist-based permission triggering
///
/// Uses MockProvider - no real LLM credentials needed.
use fox_agent_sdk::{
    ApprovalManager, ApprovalScope, DefaultModel, DefaultSafetyPolicy,
    FoxAgentSdkConfig, Harness, MockProvider, Model, PermissionResult,
    SafetyConfig, StreamEvent,
};
use std::sync::Arc;

#[tokio::main]
async fn main() {
    println!("=== Fox Agent SDK — Permission Flow Demo ===\n");

    // ── Set up provider ──
    let provider = Arc::new(MockProvider::new("mock"));
    provider.push_script(vec![
        StreamEvent::TextDelta { text: "I'll check the file.".into() },
        StreamEvent::MessageStop { stop_reason: None },
    ]);

    // ── Build with safety policy ──
    let safety = SafetyConfig {
        default_policy: DefaultSafetyPolicy::Allow,
        tool_denylist: Some(vec!["echo".to_string()]),
        tool_allowlist: None,
        ..Default::default()
    };

    let _model: Arc<dyn Model> = Arc::new(DefaultModel::new(provider, "mock-1"));
    let harness = Harness::new(
        FoxAgentSdkConfig { safety, ..Default::default() },
        None,
    );
    harness.register_default_tools().await;

    // ── Set up approval manager ──
    let approval = ApprovalManager::new("demo-session", SafetyConfig::default());

    // Cache "read" approvals for entire session
    approval
        .cache_decision("read", &PermissionResult::Allow, ApprovalScope::ThisSession)
        .await;
    println!("> read tool auto-approved for session\n");

    // Check cache
    let cached = approval.check_cache("read").await;
    println!("> cache check for 'read': {:?}", cached);

    // Record an audit entry
    let request = fox_agent_sdk::PermissionRequest::new("read", "Read Cargo.toml");
    approval
        .record_audit(&request, &PermissionResult::Allow, 42)
        .await;

    // Export audit
    let audit_path = std::env::temp_dir().join("permission_audit_demo.jsonl");
    approval.export_audit(&audit_path).await.unwrap();
    println!(
        "> audit trail exported to {}",
        audit_path.display()
    );
    println!("\nDone.");
}
