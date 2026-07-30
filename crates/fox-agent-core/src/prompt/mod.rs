//! System prompt assembly — PromptBuilder + supporting types.
//!
//! PromptBuilder encapsulates the static prompt configuration (embedded markdown
//! templates) and provides methods to build split system prompts, session context,
//! and full prompts from dynamic parameters.

use std::path::{Path, PathBuf};
use std::process::Command;

// ── Embedded prompt templates ──

const DEFAULT_SYSTEM_PROMPT: &str = include_str!("templates/system.md");

// ── Public types ──

/// A system prompt split into static (cacheable) and dynamic (per-turn) parts.
#[derive(Debug, Clone, Default)]
pub struct SplitPrompt {
    /// Static content suitable for provider prompt caching (template, skills, AGENTS.md)
    pub static_part: String,
    /// Dynamic content that changes every turn (memory injection, plan context)
    pub dynamic_part: String,
    /// Line number of the cache-anchor boundary (number of lines in `static_part`).
    /// Providers can use this to identify which prefix is eligible for KV caching.
    /// `None` when there is no static content.
    pub cache_anchor_line: Option<usize>,
}

impl SplitPrompt {
    /// Total character count.
    pub fn chars(&self) -> usize {
        match (self.static_part.is_empty(), self.dynamic_part.is_empty()) {
            (true, true) => 0,
            (false, true) => self.static_part.len(),
            (true, false) => self.dynamic_part.len(),
            (false, false) => self.static_part.len() + 2 + self.dynamic_part.len(),
        }
    }

    /// Rough token estimate (chars / 4).
    pub fn estimated_tokens(&self) -> usize {
        let combined = if self.static_part.is_empty() {
            self.dynamic_part.clone()
        } else if self.dynamic_part.is_empty() {
            self.static_part.clone()
        } else {
            format!("{}\n\n{}", self.static_part, self.dynamic_part)
        };
        combined.len() / 4
    }
}

/// Skill information for system prompt.
#[derive(Debug, Clone)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
}

/// Information about what's loaded in the context window.
#[derive(Debug, Clone, Default)]
pub struct ContextInfo {
    // === Static sections ===
    pub system_prompt_chars: usize,
    pub session_context_chars: usize,
    pub has_project_agents_md: bool,
    pub project_agents_md_chars: usize,
    pub has_global_agents_md: bool,
    pub global_agents_md_chars: usize,
    pub skills_chars: usize,
    pub memory_chars: usize,
    pub prompt_overlay_chars: usize,

    // === Tool definitions ===
    pub tool_defs_chars: usize,
    pub tool_defs_count: usize,

    // === Messages ===
    pub user_messages_chars: usize,
    pub user_messages_count: usize,
    pub assistant_messages_chars: usize,
    pub assistant_messages_count: usize,
    pub tool_calls_chars: usize,
    pub tool_calls_count: usize,
    pub tool_results_chars: usize,
    pub tool_results_count: usize,

    /// Total characters across all sections
    pub total_chars: usize,
}

impl ContextInfo {
    /// Rough token estimate (chars / 4).
    pub fn estimated_tokens(&self) -> usize {
        self.total_chars / 4
    }

    /// Total characters in the prompt prefix (system + static context).
    pub fn prompt_prefix_chars(&self) -> usize {
        self.system_prompt_chars
            + self.session_context_chars
            + self.project_agents_md_chars
            + self.global_agents_md_chars
            + self.skills_chars
            + self.memory_chars
            + self.prompt_overlay_chars
            + self.tool_defs_chars
    }

    /// Get breakdown as (label, chars, icon) tuples for display.
    pub fn breakdown(&self) -> Vec<(&'static str, usize, &'static str)> {
        let mut parts = vec![
            ("sys", self.system_prompt_chars, "\u{2699}"),
            ("session", self.session_context_chars, "\u{1f30d}"),
        ];
        if self.has_project_agents_md {
            parts.push(("agents", self.project_agents_md_chars, "\u{1f4cb}"));
        }
        if self.has_global_agents_md {
            parts.push(("~agents", self.global_agents_md_chars, "\u{1f4cb}"));
        }
        if self.skills_chars > 0 {
            parts.push(("skills", self.skills_chars, "\u{1f527}"));
        }
        if self.memory_chars > 0 {
            parts.push(("mem", self.memory_chars, "\u{1f9e0}"));
        }
        if self.prompt_overlay_chars > 0 {
            parts.push(("overlay", self.prompt_overlay_chars, "\u{1f9e9}"));
        }
        parts
    }
}

// ── PromptBuilder ──

/// Builder for system prompts.
///
/// Holds the static embedded prompt templates and build-time version info.
/// Dynamic parameters are passed to each build method.
#[derive(Debug, Clone)]
pub struct PromptBuilder {
    pub version: String,
    pub git_hash: String,
    /// When set, overrides the compiled-in default system template.
    custom_system_template: Option<String>,
}

impl PromptBuilder {
    pub fn new(version: impl Into<String>, git_hash: impl Into<String>) -> Self {
        Self {
            version: version.into(),
            git_hash: git_hash.into(),
            custom_system_template: None,
        }
    }

    /// Build a split prompt from static and dynamic parts.
    ///
    /// `static_sections` — items joined into `static_part` (cached): template, AGENTS.md, session context, skills, overlay
    /// `dynamic_sections` — items joined into `dynamic_part` (per-turn): memory, plan context, active skill
    pub fn build_split<S: Into<String>>(
        &self,
        static_sections: Vec<S>,
        dynamic_sections: Vec<S>,
    ) -> SplitPrompt {
        let static_part: Vec<String> = static_sections.into_iter().map(|s| s.into()).filter(|s| !s.is_empty()).collect();
        let dynamic_part: Vec<String> = dynamic_sections.into_iter().map(|s| s.into()).filter(|s| !s.is_empty()).collect();
        let static_joined = static_part.join("\n\n");
        let cache_anchor_line = if static_joined.is_empty() {
            None
        } else {
            Some(static_joined.lines().count())
        };
        SplitPrompt {
            static_part: static_joined,
            dynamic_part: dynamic_part.join("\n\n"),
            cache_anchor_line,
        }
    }

    /// Build the full system prompt with all sections.
    ///
    /// Returns (full_prompt, ContextInfo).
    #[expect(clippy::too_many_arguments)]
    pub fn build_full(
        &self,
        session_context: Option<&str>,
        agents_md: Option<&str>,
        prompt_overlay: Option<&str>,
        skills: &[SkillInfo],
        memory: Option<&str>,
        planning_context: Option<&str>,
        active_skill: Option<&str>,
    ) -> (String, ContextInfo) {
        let template = self.system_template().to_string();
        let mut parts = vec![template.clone()];
        let mut info = ContextInfo {
            system_prompt_chars: template.len(),
            ..Default::default()
        };

        // Session context
        if let Some(ctx) = session_context {
            info.session_context_chars = ctx.len();
            parts.push(ctx.to_string());
        }

        // AGENTS.md
        if let Some(md) = agents_md {
            // info already filled by caller
            parts.push(md.to_string());
        }

        // Prompt overlay
        if let Some(overlay) = prompt_overlay {
            info.prompt_overlay_chars = overlay.len();
            parts.push(overlay.to_string());
        }

        // Skills list
        if !skills.is_empty() {
            let mut section = "# Available Skills\n\n\
                The following skills are available. Use `skill(action=\"list\")` to see them again, \
                `skill(action=\"activate\", name=\"<name>\")` to load one, and \
                `skill(action=\"deactivate\")` to unload.\n"
                .to_string();
            for skill in skills {
                section.push_str(&format!("\n- `{}` — {}", skill.name, skill.description));
            }
            info.skills_chars = section.len();
            parts.push(section);
        }

        // Memory
        if let Some(mem) = memory {
            info.memory_chars = mem.len();
            parts.push(mem.to_string());
        }

        // Planning context
        if let Some(plan) = planning_context {
            parts.push(plan.to_string());
        }

        // Active skill
        if let Some(skill) = active_skill {
            parts.push(format!("# Active Skill\n\n{}", skill));
        }

        let prompt = parts.join("\n\n");
        info.total_chars = prompt.len();
        (prompt, info)
    }

    // ── Session context ──

    /// Build immutable session context captured once per session.
    pub fn build_session_context(&self, working_dir: Option<&Path>) -> String {
        let mut lines = vec!["# Session Context".to_string()];

        let now = chrono::Utc::now();
        lines.push(format!("Date: {}", now.format("%Y-%m-%d")));
        lines.push(format!("Time: {} UTC", now.format("%H:%M:%S")));
        lines.push("Timezone: UTC".to_string());
        lines.push(format!("OS: {}", std::env::consts::OS));
        lines.push(format!("Architecture: {}", std::env::consts::ARCH));
        lines.push(format!("Agent version: {} ({})", self.version, self.git_hash));

        // Hardware context
        if let Some(hw) = Self::hardware_context() {
            lines.push(hw);
        }

        // Working directory
        let cwd = working_dir
            .map(Path::to_path_buf)
            .or_else(|| std::env::current_dir().ok());
        if let Some(cwd) = cwd.as_ref() {
            lines.push(format!("Working directory: {}", cwd.display()));
        }

        // Git info
        if let Some(git) = Self::git_info(cwd.as_deref()) {
            lines.push(git);
        }

        lines.join("\n")
    }

    /// Get the default system prompt template.
    pub fn default_template() -> &'static str {
        DEFAULT_SYSTEM_PROMPT
    }

    // ── AGENTS.md loading ──

    /// Load AGENTS.md files from project and (optionally) global directories.
    ///
    /// When `global_path` is `Some`, the file at that path is loaded as the
    /// global/domain-level AGENTS.md.  When `None`, falls back to the default
    /// global location (`$FOX_AGENT_DIR/AGENTS.md` or `~/.fox-agent/AGENTS.md`).
    pub fn load_agents_md(working_dir: Option<&Path>, global_path: Option<&Path>) -> (Option<String>, ContextInfo) {
        let mut contents = vec![];
        let mut info = ContextInfo::default();

        let load = |path: &Path, label: &str| -> Option<(String, usize)> {
            if path.exists() {
                std::fs::read_to_string(path).ok().map(|content| {
                    let raw = content.len();
                    let formatted = format!("# {}\n\n{}", label, content.trim());
                    (formatted, raw)
                })
            } else {
                None
            }
        };

        let project = working_dir.unwrap_or(Path::new("."));
        if let Some((content, size)) = load(&project.join("AGENTS.md"), "Project Instructions (AGENTS.md)") {
            info.has_project_agents_md = true;
            info.project_agents_md_chars = size;
            contents.push(content);
        }

        // Global AGENTS.md: explicit path takes priority, otherwise fall back to default
        let global_md = match global_path {
            Some(p) => load(p, "Domain Instructions (AGENTS.md)"),
            None => global_config_path("AGENTS.md")
                .and_then(|p| load(&p, "Global Instructions (~/.fox/AGENTS.md)")),
        };
        if let Some((content, size)) = global_md {
            info.has_global_agents_md = true;
            info.global_agents_md_chars = size;
            contents.push(content);
        }

        if contents.is_empty() {
            (None, info)
        } else {
            (Some(contents.join("\n\n")), info)
        }
    }

    /// Load prompt overlay files from project and global directories.
    pub fn load_prompt_overlay(working_dir: Option<&Path>) -> (Option<String>, usize) {
        let mut contents = vec![];
        let mut total = 0usize;

        let load = |path: &Path, label: &str| -> Option<(String, usize)> {
            if path.exists() {
                std::fs::read_to_string(path).ok().map(|content| {
                    let raw = content.len();
                    let formatted = format!("# {}\n\n{}", label, content.trim());
                    (formatted, raw)
                })
            } else {
                None
            }
        };

        let project = working_dir.unwrap_or(Path::new("."));
        if let Some((content, size)) = load(&project.join(".fox").join("prompt-overlay.md"), "Project Prompt Overlay (.fox/prompt-overlay.md)") {
            total += size;
            contents.push(content);
        }

        if let Some(global_path) = global_config_path("prompt-overlay.md")
            && let Some((content, size)) = load(&global_path, "Global Prompt Overlay (~/.fox/prompt-overlay.md)")
        {
            total += size;
            contents.push(content);
        }

        if contents.is_empty() {
            (None, 0)
        } else {
            (Some(contents.join("\n\n")), total)
        }
    }

    /// Get the default system prompt string (for use as a static section).
    pub fn system_template(&self) -> &str {
        self.custom_system_template
            .as_deref()
            .unwrap_or(DEFAULT_SYSTEM_PROMPT)
    }

    /// Override the compiled-in system prompt with a custom template.
    ///
    /// The provided string replaces `DEFAULT_SYSTEM_PROMPT` in all subsequent
    /// prompt builds. Useful for generic (non-coding) agent applications that
    /// need a different persona or domain-specific instructions.
    pub fn with_system_template(mut self, template: impl Into<String>) -> Self {
        self.custom_system_template = Some(template.into());
        self
    }

    // ── Private helpers ──

    fn hardware_context() -> Option<String> {
        let mut lines = Vec::new();
        if let Some(cpu) = Self::cpu_model() {
            lines.push(format!("  CPU: {}", cpu));
        }
        if let Some(mem) = Self::memory_total() {
            lines.push(format!("  Memory: {}", mem));
        }
        if lines.is_empty() {
            None
        } else {
            let mut out = vec!["Hardware:".to_string()];
            out.extend(lines);
            Some(out.join("\n"))
        }
    }

    #[expect(dead_code)]
    fn read_trimmed(file_path: impl Into<PathBuf>) -> Option<String> {
        std::fs::read_to_string(file_path.into()).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    }

    fn cpu_model() -> Option<String> {
        #[cfg(target_os = "linux")]
        {
            return {
                let info = std::fs::read_to_string("/proc/cpuinfo").ok()?;
                info.lines().find_map(|line| {
                    let (_, value) = line.split_once(':')?;
                    if line.trim_start().starts_with("model name") {
                        let v = value.trim();
                        if v.is_empty() {
                            None
                        } else {
                            Some(v.to_string())
                        }
                    } else {
                        None
                    }
                })
            };
        }
        None
    }

    fn memory_total() -> Option<String> {
        #[cfg(target_os = "linux")]
        {
            return {
                let info = std::fs::read_to_string("/proc/meminfo").ok()?;
                let kb = info.lines().find_map(|line| {
                    let rest = line.strip_prefix("MemTotal:")?.trim();
                    rest.split_whitespace().next()?.parse::<u64>().ok()
                })?;
                let gib = kb as f64 / 1024.0 / 1024.0;
                Some(format!("{:.1} GiB", gib))
            };
        }
        None
    }

    fn git_info(working_dir: Option<&Path>) -> Option<String> {
        let check = || -> Option<String> {
            let dir = working_dir.unwrap_or(Path::new("."));
            let output = Command::new("git")
                .args(["rev-parse", "--is-inside-work-tree"])
                .current_dir(dir)
                .output().ok()?;
            if !output.status.success() { return None; }

            let mut info = vec!["Git:".to_string()];

            // Branch
            if let Ok(out) = Command::new("git")
                .args(["branch", "--show-current"])
                .current_dir(dir)
                .output()
                && out.status.success()
            {
                let branch = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !branch.is_empty() {
                    info.push(format!("  Branch: {}", branch));
                }
            }

            // Status
            if let Ok(out) = Command::new("git")
                .args(["status", "--porcelain"])
                .current_dir(dir)
                .output()
                && out.status.success()
            {
                let status = String::from_utf8_lossy(&out.stdout);
                let count = status.lines().count();
                if count > 0 {
                    info.push(format!("  Modified: {} files", count));
                    for file in status.lines().take(5) {
                        info.push(format!("    {}", file));
                    }
                    if count > 5 { info.push("    ...".to_string()); }
                }
            }

            if info.len() > 1 { Some(info.join("\n")) } else { None }
        };
        check()
    }
}

/// Path helper: global config directory ($FOX_AGENT_DIR or ~/.fox-agent).
fn global_config_path(filename: &str) -> Option<PathBuf> {
    let dir = std::env::var("FOX_AGENT_DIR")
        .map(PathBuf::from)
        .ok()
        .or_else(|| {
            dirs::data_dir().map(|d| d.join("fox-agent"))
        })
        .or_else(|| {
            let home = std::env::var("HOME").ok()?;
            Some(PathBuf::from(home).join(".fox-agent"))
        })?;
    Some(dir.join(filename))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_prompt_chars() {
        let sp = SplitPrompt {
            static_part: "hello".into(),
            dynamic_part: "world".into(),
            cache_anchor_line: Some(1),
        };
        assert_eq!(sp.chars(), 12); // "hello\n\nworld" = 12
    }

    #[test]
    fn test_split_prompt_estimated_tokens() {
        let sp = SplitPrompt {
            static_part: "hello world".into(),
            dynamic_part: String::new(),
            cache_anchor_line: Some(1),
        };
        assert_eq!(sp.estimated_tokens(), 11 / 4);
    }

    #[test]
    fn test_build_session_context_contains_required_fields() {
        let builder = PromptBuilder::new("1.0.0", "abc123");
        let ctx = builder.build_session_context(None);
        assert!(ctx.contains("# Session Context"));
        assert!(ctx.contains("Date:"));
        assert!(ctx.contains("OS:"));
        assert!(ctx.contains("1.0.0"));
        assert!(ctx.contains("abc123"));
    }

    #[test]
    fn test_build_full_basic() {
        let builder = PromptBuilder::new("1.0.0", "abc123");
        let skills = vec![SkillInfo { name: "test".into(), description: "A test skill".into() }];
        let (prompt, info) = builder.build_full(
            None, None, None, &skills, None, None, None,
        );
        assert!(prompt.contains("## Identity"));
        assert!(prompt.contains("test"));
        assert!(info.skills_chars > 0);
    }

    #[test]
    fn test_context_info_breakdown() {
        let info = ContextInfo {
            system_prompt_chars: 100,
            session_context_chars: 50,
            skills_chars: 30,
            ..Default::default()
        };
        let parts = info.breakdown();
        assert!(parts.iter().any(|(label, _, _)| *label == "sys"));
        assert!(parts.iter().any(|(label, _, _)| *label == "session"));
        assert!(parts.iter().any(|(label, _, _)| *label == "skills"));
    }

    #[test]
    fn test_agents_md_nonexistent() {
        let (content, info) = PromptBuilder::load_agents_md(Some(Path::new("/nonexistent/path")), None);
        assert!(content.is_none());
        assert!(!info.has_project_agents_md);
    }

    #[test]
    fn test_prompt_overlay_nonexistent() {
        let (content, size) = PromptBuilder::load_prompt_overlay(Some(Path::new("/nonexistent/path")));
        assert!(content.is_none());
        assert_eq!(size, 0);
    }

    #[test]
    fn test_build_split_empty() {
        let builder = PromptBuilder::new("1.0", "abc");
        let sp = builder.build_split::<String>(vec![], vec![]);
        assert!(sp.static_part.is_empty());
        assert!(sp.dynamic_part.is_empty());
    }

    #[test]
    fn test_build_split_content() {
        let builder = PromptBuilder::new("1.0", "abc");
        let sp = builder.build_split(
            vec!["static1", "static2"],
            vec!["dynamic1"],
        );
        assert_eq!(sp.static_part, "static1\n\nstatic2");
        assert_eq!(sp.dynamic_part, "dynamic1");
    }

    #[test]
    fn test_cache_anchor_line_with_static_content() {
        let builder = PromptBuilder::new("1.0", "abc");
        let sp = builder.build_split(
            vec!["line1\nline2\nline3", "line4"],
            vec!["dynamic"],
        );
        // "line1\nline2\nline3\n\nline4" = 5 lines total
        assert_eq!(sp.cache_anchor_line, Some(5));
    }

    #[test]
    fn test_cache_anchor_line_empty_static() {
        let builder = PromptBuilder::new("1.0", "abc");
        let sp = builder.build_split::<String>(vec![], vec!["dynamic".to_string()]);
        assert_eq!(sp.cache_anchor_line, None);
    }

    #[test]
    fn test_cache_anchor_line_single_line() {
        let builder = PromptBuilder::new("1.0", "abc");
        let sp = builder.build_split(vec!["hello"], vec!["world"]);
        assert_eq!(sp.cache_anchor_line, Some(1));
    }
}
