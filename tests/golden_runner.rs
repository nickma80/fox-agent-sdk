//! Goldenscript Runner：将 goldenscript 命令翻译为 fox-agent-sdk 操作。
//!
//! 使用 MockProvider 驱动 Agent，支持以下命令：
//! - `run_agent <prompt>` — 运行 Agent 并返回结果摘要
//! - `assert_file_exists <path>` — 断言文件存在
//! - `assert_file_contains <path> <content>` — 断言文件包含指定内容
//! - `assert_no_excessive_tool_calls <max>` — 行为规则：工具调用不超过指定次数

use std::collections::HashMap;
use std::error::Error;
use std::fmt::Write;
use std::sync::Arc;

use goldenscript::{Command, Context, Runner};

use fox_agent_core::{AgentEvent, TokenReport};
use fox_agent_core::{DefaultSafetyPolicy, FoxAgentSdkConfig, SafetyConfig};
use fox_agent_sdk::eval::behavior_rules::BehaviorRuleEngine;
use fox_agent_sdk::{Agent, Harness, MockProvider, StreamEvent};

/// Goldenscript Runner：持有 MockProvider 及 Agent 实例。
pub struct GoldenRunner {
    /// 预录的 Mock 脚本，key = 测试用例名
    mock_scripts: HashMap<String, Vec<Vec<StreamEvent>>>,
    /// 当前测试用例的 Agent 实例（懒初始化）
    agent: Option<Agent>,
    /// 行为规则引擎
    behavior_engine: BehaviorRuleEngine,
    /// 最近一次 Agent run 产生的事件流
    last_events: Vec<AgentEvent>,
    /// 最近一次 Agent run 的 TokenReport
    last_token_report: Option<TokenReport>,
    /// 当前测试用例名
    case_name: String,
}

impl GoldenRunner {
    /// 创建 Runner 并注入预录 Mock 脚本。
    pub fn new(mock_scripts: HashMap<String, Vec<Vec<StreamEvent>>>) -> Self {
        Self {
            mock_scripts,
            agent: None,
            behavior_engine: BehaviorRuleEngine::with_default_rules(),
            last_events: Vec::new(),
            last_token_report: None,
            case_name: String::new(),
        }
    }

    /// 为指定测试用例设置 Agent（懒初始化）。
    pub fn set_case(&mut self, case_name: &str) {
        self.case_name = case_name.to_string();
        self.agent = None;
        self.last_events.clear();
        self.last_token_report = None;
    }

    /// 获取或创建 Agent。每次 set_case 后首次调用会重新初始化。
    fn ensure_agent(&mut self) -> &mut Agent {
        if self.agent.is_none() {
            let provider = MockProvider::new(&self.case_name);
            if let Some(scripts) = self.mock_scripts.get(&self.case_name) {
                for script in scripts {
                    provider.push_script(script.clone());
                }
            }
            let harness = Harness::new(
                FoxAgentSdkConfig {
                    safety: SafetyConfig {
                        default_policy: DefaultSafetyPolicy::Allow,
                        productive_tool_confirm: false,
                        ..Default::default()
                    },
                    ..Default::default()
                },
                None,
            );

            let model = Arc::new(fox_agent_core::DefaultModel::new(
                Arc::new(provider),
                "mock-model",
            ));
            let agent = Agent::new(model, harness, Arc::new(tokio::sync::RwLock::new(None)));
            self.agent = Some(agent);
        }
        self.agent.as_mut().unwrap()
    }

    /// 从事件流收集 TokenReport。
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

    /// 同步包装：在 tokio runtime 中运行 Agent。
    pub fn run_agent_sync(&mut self, prompt: &str) -> Result<String, Box<dyn Error>> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        rt.block_on(async {
            let agent = self.ensure_agent();
            let (tx, mut rx) = tokio::sync::mpsc::channel(128);
            let result = agent.run_once_streaming(prompt, &tx).await;
            drop(tx);

            let mut events = Vec::new();
            while let Some(ev) = rx.recv().await {
                events.push(ev);
            }

            let token_report = Self::collect_token_report(&events);
            let summary = format!(
                "tokens_in={} tokens_out={} tools={}",
                token_report.total_input, token_report.total_output, token_report.tool_calls,
            );
            self.last_events = events;
            self.last_token_report = Some(token_report);

            match result {
                Ok(outcome) => match outcome {
                    fox_agent_core::TurnOutcome::Completed { .. } => Ok(format!("{summary}")),
                    fox_agent_core::TurnOutcome::Cancelled => Ok(format!("{summary}\n[cancelled]")),
                    fox_agent_core::TurnOutcome::RequiresUserDecision { .. } => {
                        Ok(format!("{summary}\n[requires decision]"))
                    }
                    fox_agent_core::TurnOutcome::Failed { error } => {
                        Err(format!("Agent error: {error}").into())
                    }
                },
                Err(e) => Err(format!("Agent error: {e}").into()),
            }
        })
    }
}

impl Runner for GoldenRunner {
    type Command = Command;

    fn run(
        &mut self,
        command: &Self::Command,
        _context: &Context,
    ) -> Result<String, Box<dyn Error>> {
        let mut output = String::new();
        match command.name.as_str() {
            "run_agent" => {
                let mut args = command.consume_args();
                let prompt = args.next_pos().ok_or("missing prompt argument")?;
                args.reject_next()?;
                let result = self.run_agent_sync(prompt)?;
                writeln!(output, "{result}")?;
            }
            "assert_file_exists" => {
                let mut args = command.consume_args();
                let path = args.next_pos().ok_or("missing path argument")?;
                args.reject_next()?;
                if std::fs::metadata(path).is_ok() {
                    writeln!(output, "✓")?;
                } else {
                    return Err(format!("File {path} does not exist").into());
                }
            }
            "assert_file_contains" => {
                let mut args = command.consume_args();
                let path = args.next_pos().ok_or("missing path argument")?;
                let expected = args.next_pos().ok_or("missing content argument")?;
                args.reject_next()?;
                let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
                if content.contains(expected) {
                    writeln!(output, "✓")?;
                } else {
                    return Err(format!("File {path} does not contain '{expected}'").into());
                }
            }
            "summary" => {
                if let Some(ref report) = self.last_token_report {
                    writeln!(
                        output,
                        "Total: {} tokens, {} tool calls, {} API calls",
                        report.total_tokens(),
                        report.tool_calls,
                        report.api_calls
                    )?;
                    if report.cache_read > 0 || report.cache_write > 0 {
                        writeln!(
                            output,
                            "Cache: {} read, {} write, hit_ratio={:.1}%",
                            report.cache_read,
                            report.cache_write,
                            report.cache_hit_ratio() * 100.0
                        )?;
                    }
                    if report.compactions > 0 {
                        writeln!(output, "Compactions: {}", report.compactions)?;
                    }
                } else {
                    writeln!(output, "(no data)")?;
                }
            }
            "assert_no_excessive_tool_calls" => {
                let mut args = command.consume_args();
                let max_str = args.next_pos().unwrap_or("3");
                let max: usize = max_str
                    .parse()
                    .map_err(|_| format!("invalid max value: {max_str}"))?;
                args.reject_next()?;

                let violations = self.behavior_engine.check(&self.last_events);
                let excessive = violations
                    .iter()
                    .filter(|v| v.rule_name == "no_repeat_tool_storm")
                    .count();
                if excessive <= max {
                    writeln!(output, "✓ ({} violations total)", violations.len())?;
                } else {
                    return Err(format!(
                        "Excessive tool calls: {} violations (max {max})",
                        excessive
                    )
                    .into());
                }
            }
            name => {
                return Err(format!("unknown command: {name}").into());
            }
        }
        Ok(output)
    }

    fn start_script(&mut self) -> Result<(), Box<dyn Error>> {
        self.last_events.clear();
        self.last_token_report = None;
        self.agent = None;
        Ok(())
    }
}
