use std::collections::HashMap;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};

// ── Skill source ──

/// Where a skill was loaded from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillSource {
    /// `<working_dir>/.claude/skills/`
    Project,
    /// `{storage_dir}/skills/`
    Global,
    /// An additional user-configured directory
    Additional(PathBuf),
    /// Loaded from a plugin (value is plugin name)
    Plugin(String),
}

// ── Skill arg ──

/// Parameter definition for skills that accept arguments.
#[derive(Debug, Clone)]
pub struct SkillArg {
    pub name: String,
    pub description: String,
    pub required: bool,
}

// ── Skill ──

/// A skill compatible with Claude Code skill format.
///
/// File format (YAML frontmatter + markdown body):
/// ```markdown
/// ---
/// name: my-skill
/// description: Does something useful
/// version: 1.0
/// allowed-tools: [read, write, bash]
/// model: claude-sonnet-4-20250514
/// args:
///   - name: format
///     description: Output format (json or text)
///     required: false
/// disable-model-invocation: false
/// ---
///
/// You are an expert at X. When asked about Y, do Z...
///
/// ## Instructions
/// 1. First check {{WORKING_DIR}}...
/// 2. Use files from {{SKILL_DIR}}/data/...
/// ```
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub prompt: String,
    pub allowed_tools: Vec<String>,
    pub model: Option<String>,
    pub version: Option<String>,
    pub args: Vec<SkillArg>,
    pub disable_model_invocation: bool,
    pub base_directory: Option<String>,
    pub source: SkillSource,
}

impl Default for Skill {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: String::new(),
            prompt: String::new(),
            allowed_tools: Vec::new(),
            model: None,
            version: None,
            args: Vec::new(),
            disable_model_invocation: false,
            base_directory: None,
            source: SkillSource::Project,
        }
    }
}

impl Skill {
    /// Parse a skill from file content (requires YAML frontmatter).
    pub fn parse(name: impl Into<String>, content: &str) -> Result<Self, String> {
        let name = name.into();
        let trimmed = content.trim();
        if trimmed.is_empty() {
            return Err(format!("skill `{name}` file is empty"));
        }

        if trimmed.starts_with("---") {
            let body = &trimmed[3..];
            if let Some(end) = body.find("\n---") {
                let frontmatter = &body[..end];
                let prompt = body[end + 4..].trim().to_string();
                let fm = parse_frontmatter(frontmatter);

                let skill_name = fm.get("name").cloned().unwrap_or(name);
                let description = fm
                    .get("description")
                    .cloned()
                    .unwrap_or_else(|| skill_name.clone());
                let allowed_tools = fm
                    .get("allowed-tools")
                    .map(|s| parse_list(s))
                    .unwrap_or_default();
                let model = fm.get("model").cloned();
                let version = fm.get("version").cloned();
                let disable_model_invocation = fm
                    .get("disable-model-invocation")
                    .map(|v| v == "true")
                    .unwrap_or(false);

                // Parse args block (Claude Code format: multiline "- name: ..." lines)
                let args = parse_skill_args(&fm);

                return Ok(Self {
                    name: skill_name,
                    description,
                    prompt,
                    allowed_tools,
                    model,
                    version,
                    args,
                    disable_model_invocation,
                    base_directory: None,
                    source: SkillSource::Project,
                });
            }
        }

        Err(format!("skill `{name}`: missing YAML frontmatter. Skills must start with `---`."))
    }

    /// Create a new skill from a markdown file on disk.
    pub fn from_file(name: impl Into<String>, path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read skill `{}`: {e}", path.display()))?;
        let mut skill = Self::parse(name, &content)?;
        if let Some(parent) = path.parent() {
            skill.base_directory = Some(parent.to_string_lossy().to_string());
        }
        Ok(skill)
    }

    /// Expand template variables in the prompt.
    ///
    /// Supported variables:
    /// - `{{SKILL_DIR}}` — resolved to `base_directory` absolute path
    /// - `{{WORKING_DIR}}` — resolved to `working_dir`
    /// - `{{ARGS.<name>}}` — resolved to argument values
    ///
    /// Returns an error when a required argument is not provided,
    /// preventing silent `{{ARGS.xxx}}` placeholders in the final prompt.
    pub fn expand_prompt(
        &self,
        working_dir: Option<&Path>,
        args: &HashMap<String, String>,
    ) -> Result<String, String> {
        let mut prompt = self.prompt.clone();

        if let Some(ref base) = self.base_directory {
            prompt = prompt.replace("{{SKILL_DIR}}", base);
        }

        if let Some(wd) = working_dir {
            prompt = prompt.replace("{{WORKING_DIR}}", &wd.to_string_lossy());
        }

        for arg in &self.args {
            let placeholder = format!("{{{{ARGS.{}}}}}", arg.name);
            if let Some(val) = args.get(&arg.name) {
                prompt = prompt.replace(&placeholder, val);
            } else if arg.required {
                return Err(format!(
                    "required argument `{}` is missing for skill `{}`",
                    arg.name, self.name
                ));
            }
        }

        Ok(prompt)
    }
}

// ── Frontmatter parsing ──

/// Parse YAML-like frontmatter with basic nested list support for `args`.
fn parse_frontmatter(text: &str) -> HashMap<String, String> {
    let mut map: HashMap<String, String> = HashMap::new();
    let mut args_lines: Vec<String> = Vec::new();
    let mut in_args = false;

    for line_raw in text.lines() {
        let line = line_raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Args multi-line block: collect all indented and `-` lines
        if line == "args:" {
            in_args = true;
            continue;
        }
        if in_args {
            // A line that has NO leading whitespace AND starts with something
            // other than `-` signals the end of the args block
            let has_leading_whitespace = line_raw.starts_with(' ') || line_raw.starts_with('\t');
            if !has_leading_whitespace && !line.starts_with('-') {
                in_args = false;
                if !args_lines.is_empty() {
                    map.insert("args".to_string(), args_lines.join("\n"));
                    args_lines.clear();
                }
                // Fall through to process this line as a regular key
            } else {
                args_lines.push(line.to_string());
                continue;
            }
        }

        if let Some(pos) = line.find(':') {
            let key = line[..pos].trim().to_string();
            let value = line[pos + 1..].trim().to_string();
            map.insert(key, value);
        }
    }

    // Flush remaining args
    if in_args && !args_lines.is_empty() {
        map.insert("args".to_string(), args_lines.join("\n"));
    }

    map
}

/// Parse args from multi-line format:
/// ```yaml
/// args:
///   - name: format
///     description: Output format
///     required: false
/// ```
fn parse_skill_args(fm: &HashMap<String, String>) -> Vec<SkillArg> {
    let Some(raw) = fm.get("args") else {
        return Vec::new();
    };

    let mut args = Vec::new();
    let mut name = String::new();
    let mut desc = String::new();
    let mut required = false;
    let mut has_name = false;

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("- name:") || trimmed.starts_with("name:") {
            // Flush previous entry
            if has_name {
                args.push(SkillArg { name: name.clone(), description: desc.clone(), required });
            }
            let name_part = trimmed.trim_start_matches('-').trim();
            name = name_part["name:".len()..].trim().to_string();
            desc.clear();
            required = false;
            has_name = true;
        } else if trimmed.starts_with("description:") {
            desc = trimmed["description:".len()..].trim().to_string();
        } else if trimmed.starts_with("required:") {
            required = trimmed["required:".len()..].trim() == "true";
        }
    }

    // Flush last entry
    if has_name {
        args.push(SkillArg { name, description: desc, required });
    }

    args
}

/// Parse a bracketed list like `[read, write, bash]` or `[read]`.
fn parse_list(s: &str) -> Vec<String> {
    let s = s.trim();
    let inner = if s.starts_with('[') && s.ends_with(']') {
        &s[1..s.len() - 1]
    } else {
        s
    };
    inner
        .split(',')
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .collect()
}

// ── SkillRegistry ──

/// Registry of loaded skills with multi-source support.
#[derive(Debug, Clone)]
pub struct SkillRegistry {
    skills: HashMap<String, Skill>,
    source_index: HashMap<String, SkillSource>, // name → source
}

impl Default for SkillRegistry {
    fn default() -> Self {
        Self {
            skills: HashMap::new(),
            source_index: HashMap::new(),
        }
    }
}

impl SkillRegistry {
    /// Insert a skill. Lower-priority skills with the same name are skipped.
    fn insert_with_priority(&mut self, skill: Skill, priority: u8) {
        if let Some(existing) = self.skills.get(&skill.name) {
            let existing_priority = source_priority(&existing.source);
            if existing_priority <= priority {
                return; // existing has higher or equal priority
            }
        }
        self.source_index
            .insert(skill.name.clone(), skill.source.clone());
        self.skills.insert(skill.name.clone(), skill);
    }

    /// Insert skill for backward compat (no priority check).
    pub fn insert(&mut self, skill: Skill) {
        self.source_index
            .insert(skill.name.clone(), skill.source.clone());
        self.skills.insert(skill.name.clone(), skill);
    }

    pub fn list(&self) -> Vec<Skill> {
        self.skills.values().cloned().collect()
    }

    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.get(name)
    }

    pub fn get_with_args(
        &self,
        name: &str,
        working_dir: Option<&Path>,
        args: &HashMap<String, String>,
    ) -> Option<Skill> {
        self.skills.get(name).and_then(|s| {
            match s.expand_prompt(working_dir, args) {
                Ok(prompt) => {
                    let mut expanded = s.clone();
                    expanded.prompt = prompt;
                    Some(expanded)
                }
                Err(e) => {
                    tracing::warn!(skill = %name, error = %e, "failed to expand skill prompt");
                    None
                }
            }
        })
    }

    pub fn remove(&mut self, name: &str) -> Option<Skill> {
        self.source_index.remove(name);
        self.skills.remove(name)
    }

    pub fn unload_source(&mut self, source: &SkillSource) -> usize {
        let names: Vec<String> = self
            .source_index
            .iter()
            .filter(|(_, s)| *s == source)
            .map(|(n, _)| n.clone())
            .collect();
        for name in &names {
            self.skills.remove(name);
            self.source_index.remove(name);
        }
        names.len()
    }

    // ── Bulk loading ──

    /// Load all skills from a directory (non-recursive).
    pub fn load_from_dir(&mut self, dir: &Path, source: SkillSource) -> Result<usize, String> {
        if !dir.exists() {
            return Ok(0);
        }
        let priority = source_priority(&source);
        let mut count = 0;
        scan_dir_for_skills(dir, &source, priority, self, &mut count, true)?;
        Ok(count)
    }

    /// Load skills from `.claude/skills/`.
    pub fn load_from_working_dir(&mut self, working_dir: Option<&Path>) -> Result<usize, String> {
        let Some(dir) = working_dir else { return Ok(0) };
        let claude_skills = dir.join(".claude").join("skills");
        self.load_from_dir(&claude_skills, SkillSource::Project)
    }

    /// Load skills from the global storage directory.
    pub fn load_from_global_dir(&mut self, storage_dir: &Path) -> Result<usize, String> {
        let global_skills = storage_dir.join("skills");
        self.load_from_dir(&global_skills, SkillSource::Global)
    }

    /// Load skills from additional user-configured directories.
    pub fn load_from_config(
        &mut self,
        storage_dir: &Path,
        working_dir: Option<&Path>,
        config: &SkillsConfig,
    ) -> Result<usize, String> {
        let mut total = 0;

        // Project skills (highest priority)
        total += self.load_from_working_dir(working_dir)?;

        // Global skills
        if config.load_global {
            total += self.load_from_global_dir(storage_dir)?;
        }

        // Additional directories
        for dir in &config.additional_directories {
            total += self.load_from_dir(dir, SkillSource::Additional(dir.clone()))?;
        }

        Ok(total)
    }
}

// ── Helpers ──

/// Source priority: lower = higher priority
fn source_priority(source: &SkillSource) -> u8 {
    match source {
        SkillSource::Project => 0,
        SkillSource::Additional(_) => 1,
        SkillSource::Global => 2,
        SkillSource::Plugin(_) => 3,
    }
}

/// Recursively scan a directory for `.md` skill files.
fn scan_dir_for_skills(
    dir: &Path,
    source: &SkillSource,
    priority: u8,
    registry: &mut SkillRegistry,
    count: &mut usize,
    _is_root: bool,
) -> Result<(), String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| format!("failed to read skills dir `{}`: {e}", dir.display()))?;

    for entry in entries.flatten() {
        let path = entry.path();
        let file_type = entry.file_type().map_err(|e| {
            format!("failed to read file type `{}`: {e}", path.display())
        })?;

        if file_type.is_dir() {
            // Recursively scan subdirectories
            scan_dir_for_skills(&path, source, priority, registry, count, false)?;
        } else if path.extension().map(|e| e == "md").unwrap_or(false) {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                match Skill::from_file(stem, &path) {
                    Ok(mut skill) => {
                        skill.source = source.clone();
                        registry.insert_with_priority(skill, priority);
                        *count += 1;
                    }
                    Err(e) => {
                        tracing::warn!("{e}");
                    }
                }
            }
        }
    }
    Ok(())
}

// ── SkillsConfig ──

/// Configuration for the skills system.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillsConfig {
    /// Whether skills are enabled at all.
    pub enabled: bool,

    /// Additional directories to scan for skills (absolute paths).
    #[serde(default)]
    pub additional_directories: Vec<PathBuf>,

    /// Whether to load global skills from `{storage_dir}/skills/`.
    pub load_global: bool,

    /// Reload strategy: auto (fs watcher) or manual (load once at build).
    #[serde(default)]
    pub reload_strategy: ReloadStrategy,
}

impl Default for SkillsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            additional_directories: Vec::new(),
            load_global: true,
            reload_strategy: ReloadStrategy::default(),
        }
    }
}

/// How skills are reloaded.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub enum ReloadStrategy {
    /// Watch filesystem for changes and reload automatically.
    #[default]
    Auto,
    /// Only load once during `build()`.
    Manual,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_claude_code_frontmatter() {
        let content = "---\nname: pdf\ndescription: PDF manipulation\nallowed-tools: [read, write, bash]\n---\n\nYou are a PDF expert.\n\n## Instructions\n1. First read the file.\n2. Then modify it.";
        let skill = Skill::parse("pdf", content).unwrap();
        assert_eq!(skill.name, "pdf");
        assert_eq!(skill.description, "PDF manipulation");
        assert_eq!(skill.allowed_tools, vec!["read", "write", "bash"]);
        assert!(skill.prompt.contains("PDF expert"));
        assert!(skill.prompt.contains("## Instructions"));
        assert_eq!(skill.source, SkillSource::Project);
    }

    #[test]
    fn test_claude_code_minimal_frontmatter() {
        let content = "---\nname: my-skill\n---\n\nJust do it.";
        let skill = Skill::parse("my-skill", content).unwrap();
        assert_eq!(skill.name, "my-skill");
        assert_eq!(skill.description, "my-skill");
        assert!(skill.allowed_tools.is_empty());
        assert_eq!(skill.prompt, "Just do it.");
    }

    #[test]
    fn test_frontmatter_name_takes_priority() {
        let content =
            "---\nname: renamed-skill\ndescription: Custom desc\n---\n\nPrompt here.";
        let skill = Skill::parse("filename-skill", content).unwrap();
        assert_eq!(skill.name, "renamed-skill");
        assert_eq!(skill.description, "Custom desc");
    }

    #[test]
    fn test_with_args() {
        let content = "---\nname: formatter\ndescription: Format code\nargs:\n  - name: style\n    description: Code style\n    required: false\n  - name: language\n    description: Target language\n    required: true\n---\n\nFormat in {{ARGS.style}} style for {{ARGS.language}}.";
        let skill = Skill::parse("formatter", content).unwrap();
        assert_eq!(skill.args.len(), 2);
        assert_eq!(skill.args[0].name, "style");
        assert!(!skill.args[0].required);
        assert_eq!(skill.args[1].name, "language");
        assert!(skill.args[1].required);
    }

    #[test]
    fn test_expand_prompt() {
        let skill = Skill {
            name: "test".into(),
            description: "test".into(),
            prompt: "Dir: {{SKILL_DIR}}, WD: {{WORKING_DIR}}, Style: {{ARGS.style}}"
                .into(),
            allowed_tools: vec![],
            model: None,
            version: None,
            args: vec![SkillArg {
                name: "style".into(),
                description: "style".into(),
                required: false,
            }],
            disable_model_invocation: false,
            base_directory: Some("/skills/test".into()),
            source: SkillSource::Project,
        };

        let args: HashMap<String, String> =
            [("style".into(), "compact".into())].into();
        let expanded = skill.expand_prompt(Some(Path::new("/work")), &args).unwrap();
        assert!(expanded.contains("/skills/test"));
        assert!(expanded.contains("/work"));
        assert!(expanded.contains("compact"));
    }

    #[test]
    fn test_expand_prompt_missing_required_arg() {
        let skill = Skill {
            name: "test".into(),
            description: "test".into(),
            prompt: "Style: {{ARGS.style}}"
                .into(),
            allowed_tools: vec![],
            model: None,
            version: None,
            args: vec![SkillArg {
                name: "style".into(),
                description: "style".into(),
                required: true,
            }],
            disable_model_invocation: false,
            base_directory: None,
            source: SkillSource::Project,
        };

        let args: HashMap<String, String> = HashMap::new();
        let result = skill.expand_prompt(None, &args);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("missing"));
    }

    #[test]
    fn test_skill_registry_load_dir() {
        let dir = std::env::temp_dir().join(format!("skill-load-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("pdf.md"),
            "---\nname: pdf\ndescription: PDF skill\n---\n\nPrompt body",
        )
        .unwrap();
        std::fs::write(
            dir.join("other.md"),
            "---\nname: other\ndescription: Another skill\n---\n\nOther prompt",
        )
        .unwrap();
        std::fs::write(dir.join("note.txt"), "not a skill").unwrap();

        let mut registry = SkillRegistry::default();
        let count = registry
            .load_from_dir(&dir, SkillSource::Project)
            .unwrap();
        assert_eq!(count, 2);
        {
            let pdf = registry.get("pdf").unwrap();
            assert_eq!(pdf.name, "pdf");
            assert_eq!(pdf.description, "PDF skill");
            assert_eq!(pdf.prompt, "Prompt body");
        }
        {
            let other = registry.get("other").unwrap();
            assert_eq!(other.name, "other");
            assert_eq!(other.description, "Another skill");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_nested_directory_scan() {
        let dir = std::env::temp_dir().join(format!("skill-nested-{}", uuid::Uuid::new_v4()));
        let sub = dir.join("sub");
        std::fs::create_dir_all(&sub).unwrap();

        std::fs::write(
            dir.join("root.md"),
            "---\nname: root\ndescription: root\n---\n\nroot",
        )
        .unwrap();
        std::fs::write(
            sub.join("sub.md"),
            "---\nname: sub\ndescription: sub\n---\n\nsub",
        )
        .unwrap();

        let mut registry = SkillRegistry::default();
        let count = registry
            .load_from_dir(&dir, SkillSource::Project)
            .unwrap();
        assert_eq!(count, 2);
        assert!(registry.get("root").is_some());
        assert!(registry.get("sub").is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_source_priority() {
        let mut registry = SkillRegistry::default();

        // Load project skill first
        let project_skill = Skill {
            name: "same-name".into(),
            description: "from project".into(),
            prompt: "project".into(),
            source: SkillSource::Project,
            ..Default::default()
        };
        registry.insert_with_priority(project_skill, 0);

        // Global skill with same name should NOT override
        let global_skill = Skill {
            name: "same-name".into(),
            description: "from global".into(),
            prompt: "global".into(),
            source: SkillSource::Global,
            ..Default::default()
        };
        registry.insert_with_priority(global_skill, 2);

        assert_eq!(registry.get("same-name").unwrap().prompt, "project");
    }
}
