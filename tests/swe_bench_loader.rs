//! SWE-bench 数据加载器。
//!
//! 加载 JSON/JSONL 格式的 SWE-bench Lite 数据集，
//! 提供任务列表、实例查询、难度分级等功能。
//!
//! 启用方式：
//! ```toml
//! # Cargo.toml
//! [features]
//! swe_bench = []
//! ```
//!
//! ```bash
//! cargo test --test swe_bench_loader --features swe_bench
//! ```

use std::path::{Path, PathBuf};

use serde::Deserialize;

// ═══════════════════════════════════════════════════════════════════════════════
// Data Types
// ═══════════════════════════════════════════════════════════════════════════════

/// SWE-bench 任务难度分级。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Difficulty {
    Easy,
    Medium,
    Hard,
    Expert,
}

/// SWE-bench Lite 单个实例（精简字段）。
///
/// 完整字段参见 <https://github.com/princeton-nlp/SWE-bench> 。
#[derive(Debug, Clone, Deserialize)]
pub struct SweBenchInstance {
    /// 实例 ID，如 `django__django-12345`
    pub instance_id: String,
    /// 仓库全名，如 `django/django`
    pub repo: String,
    /// 基准 commit（问题存在时的 commit）
    pub base_commit: String,
    /// Issue / PR 的问题描述
    pub problem_statement: String,
    /// 提示词（从 problem_statement 生成）
    #[serde(default)]
    pub hints_text: Option<String>,
    /// 难度分级
    #[serde(default)]
    pub difficulty: Option<Difficulty>,
    /// 失败→通过 的测试 patch（仅用于评估，不泄漏给 Agent）
    #[serde(default)]
    pub patch: Option<String>,
}

impl SweBenchInstance {
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

// ═══════════════════════════════════════════════════════════════════════════════
// Loader
// ═══════════════════════════════════════════════════════════════════════════════

/// SWE-bench 数据加载器。
pub struct SweBenchLoader {
    dataset_path: PathBuf,
}

impl SweBenchLoader {
    /// 创建加载器，指向 JSONL 数据集文件。
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            dataset_path: path.as_ref().to_path_buf(),
        }
    }

    /// 加载全部实例。
    pub fn load_all(&self) -> anyhow::Result<Vec<SweBenchInstance>> {
        let content = std::fs::read_to_string(&self.dataset_path)?;
        let instances: Vec<SweBenchInstance> = content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).map_err(anyhow::Error::from))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(instances)
    }

    /// 按 ID 加载单个实例。
    pub fn load_instance(&self, instance_id: &str) -> anyhow::Result<Option<SweBenchInstance>> {
        let instances = self.load_all()?;
        Ok(instances.into_iter().find(|i| i.instance_id == instance_id))
    }

    /// 按难度筛选实例。
    pub fn filter_by_difficulty(
        &self,
        difficulty: Difficulty,
    ) -> anyhow::Result<Vec<SweBenchInstance>> {
        let instances = self.load_all()?;
        Ok(instances
            .into_iter()
            .filter(|i| i.difficulty == Some(difficulty))
            .collect())
    }

    /// 获取各难度实例数量统计。
    pub fn difficulty_counts(&self) -> anyhow::Result<Vec<(Difficulty, usize)>> {
        let instances = self.load_all()?;
        let mut counts = std::collections::HashMap::new();
        for inst in &instances {
            if let Some(d) = inst.difficulty {
                *counts.entry(d).or_insert(0) += 1;
            }
        }
        let mut result: Vec<_> = counts.into_iter().collect();
        result.sort_by_key(|(d, _)| match d {
            Difficulty::Easy => 0,
            Difficulty::Medium => 1,
            Difficulty::Hard => 2,
            Difficulty::Expert => 3,
        });
        Ok(result)
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    /// 从内联 JSONL 创建临时测试文件并加载，验证解析正确。
    #[test]
    fn parse_inline_jsonl() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let jsonl = r#"{"instance_id":"test__test-1","repo":"test/test","base_commit":"abc123","problem_statement":"Fix bug","difficulty":"easy"}
{"instance_id":"test__test-2","repo":"test/test","base_commit":"def456","problem_statement":"Add feature","difficulty":"medium"}
"#;
        std::fs::write(tmp.path(), jsonl).unwrap();

        let loader = SweBenchLoader::new(tmp.path());
        let instances = loader.load_all().unwrap();
        assert_eq!(instances.len(), 2);
        assert_eq!(instances[0].instance_id, "test__test-1");
        assert_eq!(instances[0].difficulty, Some(Difficulty::Easy));
        assert_eq!(instances[1].difficulty, Some(Difficulty::Medium));

        let prompt = instances[0].generate_prompt();
        assert!(prompt.contains("Fix bug"));
        assert!(prompt.contains("test/test"));
    }

    #[test]
    fn filter_by_difficulty() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let jsonl = r#"{"instance_id":"a-1","repo":"a/b","base_commit":"x","problem_statement":"p","difficulty":"easy"}
{"instance_id":"a-2","repo":"a/b","base_commit":"y","problem_statement":"p","difficulty":"hard"}
{"instance_id":"a-3","repo":"a/b","base_commit":"z","problem_statement":"p","difficulty":"easy"}
"#;
        std::fs::write(tmp.path(), jsonl).unwrap();

        let loader = SweBenchLoader::new(tmp.path());
        let easy = loader.filter_by_difficulty(Difficulty::Easy).unwrap();
        assert_eq!(easy.len(), 2);

        let stats = loader.difficulty_counts().unwrap();
        assert_eq!(stats.len(), 2); // easy=2, hard=1
    }

    #[test]
    fn load_nonexistent_file() {
        let loader = SweBenchLoader::new("nonexistent.jsonl");
        assert!(loader.load_all().is_err());
    }

    #[test]
    fn load_instance_by_id() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let jsonl = r#"{"instance_id":"django__django-12345","repo":"django/django","base_commit":"abc","problem_statement":"Fix X"}"#;
        std::fs::write(tmp.path(), jsonl).unwrap();

        let loader = SweBenchLoader::new(tmp.path());
        let inst = loader.load_instance("django__django-12345").unwrap();
        assert!(inst.is_some());
        assert!(loader.load_instance("nonexistent").unwrap().is_none());
    }
}
