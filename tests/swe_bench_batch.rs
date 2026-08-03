//! SWE-bench 批量评估入口。
//!
//! 加载 SWE-bench Lite 数据集，按难度筛选（Easy + Medium），逐实例构建
//! coding agent 执行，汇总 Token / 工具调用 / 通过率，输出 JSON 报告。
//!
//! 真实执行需要 `swe_bench` feature + 真实 LLM provider（从 `agent.toml`
//! 读取）+ 数据集文件；汇总统计部分有独立的单元测试，无需真实 LLM。
//!
//! ```bash
//! cargo test --test swe_bench_batch --features swe_bench -- --ignored --nocapture
//! ```

use fox_agent_core::{
    AgentEvent, DefaultSafetyPolicy, FoxAgentSdkConfig, SafetyConfig, TokenReport, TurnOutcome,
};
use fox_agent_sdk::AgentBuilder;
use serde::{Deserialize, Serialize};
use std::path::Path;

// ═══════════════════════════════════════════════════════════════════════════════
// 数据模型（独立内联，测试文件各自为 crate）
// ═══════════════════════════════════════════════════════════════════════════════

/// SWE-bench Lite 单个实例（批量评估所需的最小字段）。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Instance {
    pub instance_id: String,
    pub repo: String,
    pub difficulty: Option<String>,
    pub problem_statement: String,
    #[serde(default)]
    pub hints_text: Option<String>,
}

impl Instance {
    /// 生成发送给 coding agent 的 prompt。
    pub fn generate_prompt(&self) -> String {
        let mut prompt = format!(
            "You are a software engineer. Fix the following issue in the {} repository.\n\n",
            self.repo
        );
        prompt.push_str(&self.problem_statement);
        if let Some(ref hints) = self.hints_text {
            prompt.push_str("\n\nHints:\n");
            prompt.push_str(hints);
        }
        prompt.push_str("\n\nGenerate a patch that fixes this issue.");
        prompt
    }
}

/// 单个实例的评估结果（含 Token 与工具统计）。
#[derive(Debug, Clone, Serialize)]
pub struct InstanceEval {
    pub instance_id: String,
    pub repo: String,
    pub difficulty: String,
    pub passed: bool,
    pub error: Option<String>,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub tool_calls: u64,
    pub duration_ms: u64,
}

/// 批量评估报告。
#[derive(Debug, Clone, Serialize, Default)]
pub struct BatchReport {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub errors: usize,
    pub pass_rate: f64,
    pub results: Vec<InstanceEval>,
    pub by_difficulty: Vec<(String, usize, usize)>, // (difficulty, total, passed)
}

impl BatchReport {
    pub fn add(&mut self, result: InstanceEval) {
        self.total += 1;
        if result.passed {
            self.passed += 1;
        } else if result.error.is_some() {
            self.errors += 1;
        } else {
            self.failed += 1;
        }
        self.pass_rate = if self.total > 0 {
            self.passed as f64 / self.total as f64
        } else {
            0.0
        };
        self.results.push(result);
    }

    /// 按难度汇总 (difficulty, total, passed)。
    pub fn compute_by_difficulty(&mut self, instances: &[Instance]) {
        let mut map: std::collections::HashMap<String, (usize, usize)> =
            std::collections::HashMap::new();
        for inst in instances {
            let diff = inst.difficulty.clone().unwrap_or_else(|| "unknown".to_string());
            map.entry(diff).or_insert((0, 0)).0 += 1;
        }
        for result in &self.results {
            if let Some(inst) = instances.iter().find(|i| i.instance_id == result.instance_id) {
                let diff = inst.difficulty.clone().unwrap_or_else(|| "unknown".to_string());
                if let Some(entry) = map.get_mut(&diff) {
                    if result.passed {
                        entry.1 += 1;
                    }
                }
            }
        }
        self.by_difficulty = map
            .into_iter()
            .map(|(diff, (total, passed))| (diff, total, passed))
            .collect();
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// 数据加载
// ═══════════════════════════════════════════════════════════════════════════════

/// 从 JSONL 文件加载实例，按难度筛选（Easy/Medium），最多取 `max` 个。
fn load_and_filter(dataset_path: &Path, max: usize) -> anyhow::Result<Vec<Instance>> {
    let content = std::fs::read_to_string(dataset_path)?;
    let all: Vec<Instance> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).map_err(anyhow::Error::from))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(all
        .into_iter()
        .filter(|i| {
            matches!(
                i.difficulty.as_deref(),
                Some("Easy") | Some("Medium") | Some("easy") | Some("medium")
            )
        })
        .take(max)
        .collect())
}

// ═══════════════════════════════════════════════════════════════════════════════
// 单实例评估
// ═══════════════════════════════════════════════════════════════════════════════

/// 在指定工作目录构建 coding agent 并执行单个实例。
///
/// 工作目录应为已 checkout 到 base_commit 的仓库副本；批量评估的调用方
/// 负责准备（如 `git clone` 到临时目录）。
async fn eval_instance(
    cfg: &FoxAgentSdkConfig,
    inst: &Instance,
    work_dir: &Path,
) -> InstanceEval {
    let mut cfg = cfg.clone();
    // 批量评估无人值守：自动放行工具调用，避免卡在权限确认。
    cfg.safety = SafetyConfig {
        default_policy: DefaultSafetyPolicy::Allow,
        productive_tool_confirm: false,
        ..cfg.safety
    };

    let agent = match AgentBuilder::new()
        .working_dir(work_dir)
        .sdk_config(cfg)
        .with_default_tools()
        .build()
        .await
    {
        Ok(a) => a,
        Err(e) => {
            return InstanceEval {
                instance_id: inst.instance_id.clone(),
                repo: inst.repo.clone(),
                difficulty: inst.difficulty.clone().unwrap_or_else(|| "unknown".into()),
                passed: false,
                error: Some(format!("agent build failed: {e}")),
                tokens_in: 0,
                tokens_out: 0,
                tool_calls: 0,
                duration_ms: 0,
            };
        }
    };

    let (tx, mut rx) = tokio::sync::mpsc::channel(128);
    let start = std::time::Instant::now();
    let outcome = agent.run_once_streaming(&inst.generate_prompt(), &tx).await;
    drop(tx);
    let duration_ms = start.elapsed().as_millis() as u64;

    // 收集事件统计
    let mut token = TokenReport::default();
    let mut tool_calls = 0u64;
    while let Some(ev) = rx.recv().await {
        match ev {
            AgentEvent::ModelUsage { usage, .. } => token.record_usage(&usage),
            AgentEvent::ToolCallStart { .. } => tool_calls += 1,
            _ => {}
        }
    }

    let (passed, error) = match outcome {
        Ok(TurnOutcome::Completed { .. }) => (true, None),
        Ok(TurnOutcome::Cancelled) => (false, Some("turn cancelled".to_string())),
        Ok(TurnOutcome::RequiresUserDecision { .. }) => {
            (false, Some("turn paused awaiting permission".to_string()))
        }
        Ok(TurnOutcome::Failed { error }) => (false, Some(error.to_string())),
        Err(e) => (false, Some(format!("agent error: {e}"))),
    };

    InstanceEval {
        instance_id: inst.instance_id.clone(),
        repo: inst.repo.clone(),
        difficulty: inst.difficulty.clone().unwrap_or_else(|| "unknown".into()),
        passed,
        error,
        tokens_in: token.total_input,
        tokens_out: token.total_output,
        tool_calls,
        duration_ms,
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// 批量执行
// ═══════════════════════════════════════════════════════════════════════════════

/// 执行批量 SWE-bench 评估，返回报告。
///
/// `work_root` 下每个实例使用 `{work_root}/{instance_id}` 作为工作目录；
/// 目录不存在时自动创建（agent 在空目录中开始，可自行创建项目文件）。
pub async fn run_batch(
    dataset_path: &Path,
    config_path: &Path,
    work_root: &Path,
    max_instances: usize,
) -> anyhow::Result<BatchReport> {
    let instances = load_and_filter(dataset_path, max_instances)?;
    let cfg = FoxAgentSdkConfig::load_from_file(config_path)
        .map_err(|e| anyhow::anyhow!("failed to load {}: {e}", config_path.display()))?;

    let mut report = BatchReport::default();
    for inst in &instances {
        let work_dir = work_root.join(&inst.instance_id);
        std::fs::create_dir_all(&work_dir)?;
        let result = eval_instance(&cfg, inst, &work_dir).await;
        let tag = if result.passed { "PASS" } else { "FAIL" };
        println!("[{tag}] {} — {:?}ms, {} tool calls", inst.instance_id, result.duration_ms, result.tool_calls);
        report.add(result);
    }
    report.compute_by_difficulty(&instances);
    Ok(report)
}

// ═══════════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════════

/// 批量评估（需要真实 LLM + 数据集文件，feature-gated + ignored）。
#[tokio::test]
#[cfg(feature = "swe_bench")]
#[ignore = "requires real LLM provider and SWE-bench Lite dataset (data/swe-bench-lite.jsonl)"]
async fn run_swe_bench_lite() -> anyhow::Result<()> {
    let project_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dataset_path = project_root.join("data/swe-bench-lite.jsonl");
    if !dataset_path.exists() {
        println!(
            "SWE-bench Lite dataset not found at {}. \
             Download from https://huggingface.co/datasets/princeton-nlp/SWE-bench_Lite",
            dataset_path.display()
        );
        return Ok(());
    }

    let config_path = project_root.join("agent.toml");
    let work_root = project_root.join("target/swe-bench-work");
    let report = run_batch(&dataset_path, &config_path, &work_root, 10).await?;

    println!(
        "通过率: {:.2}% ({}/{})",
        report.pass_rate * 100.0,
        report.passed,
        report.total
    );
    for (diff, total, passed) in &report.by_difficulty {
        println!("  {diff}: {passed}/{total}");
    }

    // 输出 JSON 报告
    let out = project_root.join("target/swe-bench-results.json");
    let json = serde_json::to_string_pretty(&report)?;
    std::fs::write(&out, json)?;
    println!("Report written to {}", out.display());
    Ok(())
}

/// 打印数据集统计信息（需要数据集文件，ignored）。
#[test]
#[cfg(feature = "swe_bench")]
#[ignore = "requires SWE-bench Lite dataset file"]
fn print_dataset_statistics() -> anyhow::Result<()> {
    let project_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dataset_path = project_root.join("data/swe-bench-lite.jsonl");
    if !dataset_path.exists() {
        println!("SWE-bench Lite dataset not found — skipping statistics test.");
        return Ok(());
    }
    let instances = load_and_filter(&dataset_path, usize::MAX)?;
    println!("Instances (Easy + Medium): {}", instances.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_instance(id: &str, difficulty: &str) -> Instance {
        Instance {
            instance_id: id.to_string(),
            repo: "django/django".to_string(),
            difficulty: Some(difficulty.to_string()),
            problem_statement: "Fix the bug".to_string(),
            hints_text: None,
        }
    }

    fn result(id: &str, difficulty: &str, passed: bool, error: Option<&str>) -> InstanceEval {
        InstanceEval {
            instance_id: id.to_string(),
            repo: "django/django".to_string(),
            difficulty: difficulty.to_string(),
            passed,
            error: error.map(|s| s.to_string()),
            tokens_in: 100,
            tokens_out: 50,
            tool_calls: 3,
            duration_ms: 1234,
        }
    }

    #[test]
    fn batch_report_tracks_pass_fail_error() {
        let mut report = BatchReport::default();
        report.add(result("d1", "Easy", true, None));
        report.add(result("d2", "Easy", false, None));
        report.add(result("d3", "Medium", false, Some("agent error")));
        report.add(result("d4", "Medium", true, None));

        assert_eq!(report.total, 4);
        assert_eq!(report.passed, 2);
        assert_eq!(report.failed, 1);
        assert_eq!(report.errors, 1);
        assert_eq!(report.pass_rate, 0.5);
    }

    #[test]
    fn load_and_filter_keeps_easy_and_medium() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("dataset.jsonl");
        let lines = vec![
            serde_json::to_string(&sample_instance("easy1", "Easy"))?,
            serde_json::to_string(&sample_instance("med1", "Medium"))?,
            serde_json::to_string(&sample_instance("hard1", "Hard"))?,
            serde_json::to_string(&sample_instance("none1", ""))?,
        ];
        std::fs::write(&path, lines.join("\n"))?;

        let all = load_and_filter(&path, usize::MAX)?;
        let ids: Vec<_> = all.iter().map(|i| i.instance_id.as_str()).collect();
        assert_eq!(ids, vec!["easy1", "med1"]);
        Ok(())
    }

    #[test]
    fn compute_by_difficulty_groups_results() {
        let instances = vec![
            sample_instance("e1", "Easy"),
            sample_instance("e2", "Easy"),
            sample_instance("m1", "Medium"),
        ];
        let mut report = BatchReport::default();
        report.add(result("e1", "Easy", true, None));
        report.add(result("e2", "Easy", false, None));
        report.add(result("m1", "Medium", true, None));
        report.compute_by_difficulty(&instances);

        let easy = report
            .by_difficulty
            .iter()
            .find(|(d, _, _)| d == "Easy")
            .expect("Easy group exists");
        assert_eq!((easy.1, easy.2), (2, 1));

        let medium = report
            .by_difficulty
            .iter()
            .find(|(d, _, _)| d == "Medium")
            .expect("Medium group exists");
        assert_eq!((medium.1, medium.2), (1, 1));
    }

    #[test]
    fn generate_prompt_includes_issue_and_hints() {
        let mut inst = sample_instance("x1", "Easy");
        inst.hints_text = Some("Check the timezone handling".to_string());
        let prompt = inst.generate_prompt();
        assert!(prompt.contains("Fix the bug"));
        assert!(prompt.contains("Check the timezone handling"));
        assert!(prompt.contains("django/django"));
    }
}
