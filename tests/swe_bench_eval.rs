//! SWE-bench 单实例评估流程。
//!
//! 加载指定实例，构建 coding agent，执行并评估补丁质量。
//! 此测试需要 `swe_bench` feature 才能启用（需要真实 LLM）。
//!
//! ```bash
//! cargo test --test swe_bench_eval --features swe_bench -- --ignored
//! ```

use serde::Serialize;

mod swe_bench_loader;
use swe_bench_loader::{Difficulty, SweBenchInstance, SweBenchLoader};

// ═══════════════════════════════════════════════════════════════════════════════
// Evaluation Result
// ═══════════════════════════════════════════════════════════════════════════════

/// 单个 SWE-bench 实例的评估结果。
#[derive(Debug, Clone, Serialize)]
pub struct EvalResult {
    pub instance_id: String,
    pub repo: String,
    pub passed: bool,
    pub patch: Option<String>,
    pub error: Option<String>,
    pub duration_ms: u64,
}

/// 批量评估报告。
#[derive(Debug, Clone, Serialize)]
pub struct BatchReport {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub errors: usize,
    pub pass_rate: f64,
    pub results: Vec<EvalResult>,
    pub by_difficulty: Vec<(String, usize, usize)>, // (difficulty, total, passed)
}

impl BatchReport {
    pub fn new() -> Self {
        Self {
            total: 0,
            passed: 0,
            failed: 0,
            errors: 0,
            pass_rate: 0.0,
            results: Vec::new(),
            by_difficulty: Vec::new(),
        }
    }

    pub fn add_result(&mut self, _instance_id: String, _repo: String, result: EvalResult) {
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

    /// 计算按难度分组的统计。
    pub fn compute_by_difficulty(&mut self, instances: &[SweBenchInstance]) {
        let mut map: std::collections::HashMap<String, (usize, usize)> =
            std::collections::HashMap::new();
        for inst in instances {
            let diff = inst
                .difficulty
                .map(|d| format!("{d:?}"))
                .unwrap_or_else(|| "unknown".to_string());
            let entry = map.entry(diff).or_insert((0, 0));
            entry.0 += 1;
        }
        for result in &self.results {
            if let Some(inst) = instances.iter().find(|i| i.instance_id == result.instance_id) {
                let diff = inst
                    .difficulty
                    .map(|d| format!("{d:?}"))
                    .unwrap_or_else(|| "unknown".to_string());
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
// Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_report_tracks_pass_fail_error() {
        let mut report = BatchReport::new();
        report.add_result(
            "a".into(),
            "r".into(),
            EvalResult {
                instance_id: "a".into(),
                repo: "r".into(),
                passed: true,
                patch: None,
                error: None,
                duration_ms: 100,
            },
        );
        report.add_result(
            "b".into(),
            "r".into(),
            EvalResult {
                instance_id: "b".into(),
                repo: "r".into(),
                passed: false,
                patch: None,
                error: None,
                duration_ms: 200,
            },
        );
        report.add_result(
            "c".into(),
            "r".into(),
            EvalResult {
                instance_id: "c".into(),
                repo: "r".into(),
                passed: false,
                patch: None,
                error: Some("timeout".into()),
                duration_ms: 300,
            },
        );

        assert_eq!(report.total, 3);
        assert_eq!(report.passed, 1);
        assert_eq!(report.failed, 1);
        assert_eq!(report.errors, 1);
        assert!((report.pass_rate - 1.0 / 3.0).abs() < 0.01);
    }

    /// 内联数据集 → 验证完整评估流程数据结构。
    #[test]
    fn full_pipeline_structures() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let jsonl = r#"{"instance_id":"test-1","repo":"a/b","base_commit":"abc","problem_statement":"Fix","difficulty":"easy"}
{"instance_id":"test-2","repo":"c/d","base_commit":"def","problem_statement":"Add","difficulty":"hard"}
"#;
        std::fs::write(tmp.path(), jsonl).unwrap();

        let loader = SweBenchLoader::new(tmp.path());
        let instances = loader.load_all().unwrap();

        let mut report = BatchReport::new();
        for inst in &instances {
            let prompt = inst.generate_prompt();
            assert!(!prompt.is_empty());
            report.add_result(
                inst.instance_id.clone(),
                inst.repo.clone(),
                EvalResult {
                    instance_id: inst.instance_id.clone(),
                    repo: inst.repo.clone(),
                    passed: inst.difficulty == Some(Difficulty::Easy),
                    patch: None,
                    error: None,
                    duration_ms: 0,
                },
            );
        }

        report.compute_by_difficulty(&instances);
        assert_eq!(report.total, 2);
        assert!(!report.by_difficulty.is_empty());

        // 序列化测试
        let json = serde_json::to_string_pretty(&report).unwrap();
        assert!(json.contains("test-1"));
        assert!(json.contains("pass_rate"));
    }
}
