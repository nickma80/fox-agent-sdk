/// Permission Flow — demonstrates fine-grained tool permissions with
/// approval caching and audit trail via `ApprovalManager`.
///
/// Covers:
/// - `SafetyConfig` with allowlist / denylist / default policy
/// - `ApprovalManager` with 3-tier caching (turn / session / workspace)
/// - Permission audit export to JSONL
///
/// Runs without any LLM dependencies.
use fox_agent_sdk::{
    ApprovalManager, ApprovalScope, DefaultSafetyPolicy, PermissionRequest,
    PermissionResult, SafetyConfig,
};

#[tokio::main]
async fn main() {
    println!("=== Fox Agent SDK — Permission Flow Demo ===\n");

    // ── Configure safety policy ──
    let safety = SafetyConfig {
        default_policy: DefaultSafetyPolicy::Allow,
        tool_denylist: Some(vec!["echo".to_string()]),
        tool_allowlist: None,
        ..Default::default()
    };
    println!("> Safety config: default=Allow, denylist=[echo]\n");

    // ── Set up approval manager ──
    let approval = ApprovalManager::new("demo-session", safety);

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

    let request2 = PermissionRequest::new("bash", "rm -rf /tmp/test")
        .with_risk(
            fox_agent_sdk::RiskLevel::High,
            "denylist",
            "Execute bash command: rm -rf /tmp/test",
        );
    approval
        .record_audit(&request2, &PermissionResult::Deny { reason: "denylist".into() }, 43)
        .await;

    // ── Export audit trail ──
    let audit_path = std::env::temp_dir().join("permission_audit_demo.jsonl");
    approval.export_audit(&audit_path).await.unwrap();
    println!(
        "> Audit trail exported to {}",
        audit_path.display()
    );
    println!("\nDone.");
}
