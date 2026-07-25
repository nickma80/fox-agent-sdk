/// Safety Demo — 完整的权限与安全端到端示例。
///
/// 演示：
/// - SafetyConfig：allowlist、default_policy、productive_tool_confirm
/// - `with_audit_handler()` — 自动审计回调，消除手动 record_audit 样板代码
/// - 实际 LLM 交互触发工具调用 → 权限中断 → 用户决策 → 恢复执行
/// - 审计日志导出 JSONL
///
/// 运行：
///   cargo run --example safety_demo
use fox_agent_sdk::{
    AgentBuilder, AgentEvent, ApprovalManager, DefaultSafetyPolicy,
    FoxAgentSdkConfig, PermissionDecision, PermissionResult,
    ProviderConfig, SafetyConfig, TurnOutcome,
};
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Fox Agent SDK — 权限与安全完整示例 ===\n");

    // ── 1. 加载配置 ──
    let project_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cfg = FoxAgentSdkConfig::load_from_file(project_root.join("agent.toml"))
        .unwrap_or_else(|_| FoxAgentSdkConfig::default());

    // ── 2. 自定义安全策略 ──
    //
    // 设计思路：
    //   - allowlist 开放常用只读工具（read/grep/glob/ls）
    //   - write/edit/bash 也在 allowlist 中（允许使用），
    //     但由 productive_tool_confirm 兜底 —— 每次调用都弹确认框
    let safety = SafetyConfig {
        default_policy: DefaultSafetyPolicy::Allow,
        tool_allowlist: Some(vec![
            "read".to_string(),
            "grep".to_string(),
            "glob".to_string(),
            "ls".to_string(),
            "write".to_string(),
            "edit".to_string(),
            "bash".to_string(),
        ]),
        productive_tool_confirm: true,
        ..Default::default()
    };

    println!("安全策略:");
    println!("  allowlist:  [read, grep, glob, ls, write, edit, bash]");
    println!("  default:    Allow");
    println!("  productive_tool_confirm: 开启\n");

    // ── 3. 审计管理器（独立实例，由 with_audit_handler 自动驱动） ──
    let approval = std::sync::Arc::new(tokio::sync::Mutex::new(
        ApprovalManager::new("safety-demo", SafetyConfig::default()),
    ));

    // ── 4. 构建 Agent（注入审计回调） ──
    let api_key = std::env::var("DEEPSEEK_API_KEY")
        .unwrap_or_else(|_| "sk-placeholder".to_string());

    {
        let approval = approval.clone();
        let agent = AgentBuilder::new()
            .working_dir(&project_root)
            .sdk_config(cfg)
            .provider_config(ProviderConfig::deepseek(api_key))
            .model_id("deepseek-v4-flash")
            .with_safety_policy(safety)
            .with_default_tools()
            // ── 审计回调：自动在每次权限决策时触发 ──
            .with_audit_handler(move |req, dec, turn| {
                let result = match dec {
                    PermissionDecision::Allow => PermissionResult::Allow,
                    PermissionDecision::Deny { reason } => {
                        PermissionResult::Deny { reason: reason.clone() }
                    }
                };
                // 克隆所需数据以便在 spawn 中使用（回调参数是引用，不满足 'static）
                let req = req.clone();
                let approval = approval.clone();
                tokio::spawn(async move {
                    approval.lock().await.record_audit(&req, &result, turn).await;
                });
            })
            .build()
            .await?;

        // ── 5. 运行 Agent，处理权限中断 ──
        let (tx, mut rx) = mpsc::channel::<AgentEvent>(64);

        // 后台消费事件
        let event_handle = tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                match &event {
                    AgentEvent::PermissionRequest {
                        tool_name,
                        policy_source,
                        risk_level,
                        ..
                    } => {
                        println!(
                            "  [事件] PermissionRequest: tool={}, source={}, risk={}",
                            tool_name, policy_source, risk_level
                        );
                    }
                    AgentEvent::ToolCallStart { name, .. } => {
                        println!("  [事件] ToolCallStart: {}", name);
                    }
                    AgentEvent::ToolCallEnd { call_id, .. } => {
                        println!(
                            "  [事件] ToolCallEnd: {}",
                            &call_id[..16.min(call_id.len())]
                        );
                    }
                    _ => {}
                }
            }
        });

        println!(
            "发起任务: \"在当前目录创建一个 hello.txt，写入 'Hello from Fox Agent SDK'\"\n"
        );

        let mut is_first = true;
        let mut decision: Option<PermissionDecision> = None;

        loop {
            let outcome = if is_first {
                is_first = false;
                agent
                    .run_once_streaming(
                        "在当前目录创建一个 hello.txt，写入 'Hello from Fox Agent SDK'",
                        &tx,
                    )
                    .await?
            } else {
                agent
                    .resume_streaming(decision.take().unwrap(), &tx)
                    .await?
            };

            match outcome {
                TurnOutcome::Completed { text } => {
                    println!("\n执行完成:");
                    println!("{}", &text[..text.len().min(500)]);
                    break;
                }
                TurnOutcome::RequiresUserDecision { request } => {
                    println!("\n>>> 权限请求 <<<");
                    println!("  工具:     {}", request.tool_name);
                    println!("  策略来源: {}", request.policy_source);
                    println!("  风险等级: {:?}", request.risk_level);
                    println!("  提示:     {}", request.prompt);

                    // 用户决策（此处模拟为 Allow）
                    // 注意：审计已由 with_audit_handler 回调自动处理，无需手动 record_audit
                    let user_choice = PermissionDecision::Allow;
                    println!("  用户决策: Allow\n");
                    decision = Some(user_choice);
                }
                TurnOutcome::Cancelled => {
                    println!("\n执行已取消");
                    break;
                }
                TurnOutcome::Failed { error } => {
                    eprintln!("\n执行失败: {}", error);
                    break;
                }
            }
        }

        drop(tx);
        let _ = event_handle.await;
    } // agent 在此处 drop，确保审计回调的生命周期结束

    // ── 6. 导出审计日志 ──
    let approval = approval.lock().await;
    let audit_path = std::env::temp_dir().join("fox_agent_safety_audit.jsonl");
    approval.export_audit(&audit_path).await?;
    println!("\n审计日志已导出: {}", audit_path.display());

    let audit_log = approval.audit_log().await;
    println!("审计记录数: {}", audit_log.len());
    for (i, entry) in audit_log.iter().enumerate() {
        println!(
            "  [{}] tool={}, turn={}, decision={:?}, latency={}ms",
            i + 1,
            entry.tool_name,
            entry.turn_id,
            entry.decision,
            entry.latency_ms,
        );
    }

    println!("\nDone.");
    Ok(())
}
