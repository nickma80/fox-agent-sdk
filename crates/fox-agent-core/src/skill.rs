use std::collections::HashMap;
use std::path::Path;

/// A skill compatible with Claude Code skill format.
///
/// File format (YAML frontmatter + markdown body):
/// ```markdown
/// ---
/// name: my-skill
/// description: Does something useful
/// allowed-tools: [read, write, bash]
/// model: claude-sonnet-4-20250514
/// ---
///
/// You are an expert at X. When asked about Y, do Z...
///
/// ## Instructions
/// 1. First check...
/// ```
#[derive(Debug, Clone)]
pub struct Skill {
    /// Unique skill name (from frontmatter or filename)
    pub name: String,
    /// Human-readable description (from frontmatter or first line)
    pub description: String,
    /// Prompt fragment injected when the skill is activated
    pub prompt: String,
    /// Tools this skill is allowed to use (Claude Code compat)
    pub allowed_tools: Vec<String>,
    /// Specific model required for this skill (Claude Code compat)
    pub model: Option<String>,
    /// Base directory the skill was loaded from (for relative paths)
    pub base_directory: Option<String>,
}

impl Default for Skill {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: String::new(),
            prompt: String::new(),
            allowed_tools: Vec::new(),
            model: None,
            base_directory: None,
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

        // ── Claude Code format: YAML frontmatter ──
        if trimmed.starts_with("---") {
            let body = &trimmed[3..];
            if let Some(end) = body.find("\n---") {
                let frontmatter = &body[..end];
                let prompt = body[end + 4..].trim().to_string();
                let fm = parse_frontmatter(frontmatter);

                let skill_name = fm.get("name").cloned().unwrap_or(name);
                let description = fm.get("description").cloned().unwrap_or_else(|| skill_name.clone());
                let allowed_tools = fm.get("allowed-tools")
                    .map(|s| parse_list(s))
                    .unwrap_or_default();
                let model = fm.get("model").cloned();

                return Ok(Self {
                    name: skill_name,
                    description,
                    prompt,
                    allowed_tools,
                    model,
                    base_directory: None,
                });
            }
        }

        Err(format!("skill `{name}`: missing YAML frontmatter. Skills must start with `---`."))
    }

    /// Create a new skill from a markdown file.
    pub fn from_file(name: impl Into<String>, path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read skill `{}`: {e}", path.display()))?;
        let mut skill = Self::parse(name, &content)?;
        if let Some(parent) = path.parent() {
            skill.base_directory = Some(parent.to_string_lossy().to_string());
        }
        Ok(skill)
    }
}

/// Parse simple YAML-like frontmatter (key: value pairs, no nesting).
fn parse_frontmatter(text: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(pos) = line.find(':') {
            let key = line[..pos].trim().to_string();
            let value = line[pos + 1..].trim().to_string();
            map.insert(key, value);
        }
    }
    map
}

/// Parse a bracketed list like `[read, write, bash]` or `[read]`.
fn parse_list(s: &str) -> Vec<String> {
    let s = s.trim();
    let inner = if s.starts_with('[') && s.ends_with(']') {
        &s[1..s.len() - 1]
    } else {
        s
    };
    inner.split(',').map(|item| item.trim().to_string()).filter(|item| !item.is_empty()).collect()
}

/// Registry of loaded skills.
#[derive(Debug, Clone, Default)]
pub struct SkillRegistry {
    skills: HashMap<String, Skill>,
}

impl SkillRegistry {
    pub fn insert(&mut self, skill: Skill) {
        self.skills.insert(skill.name.clone(), skill);
    }

    pub fn list(&self) -> Vec<Skill> {
        self.skills.values().cloned().collect()
    }

    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.get(name)
    }

    /// Load all skills from a directory.
    ///
    /// Scans `dir` for `.md` files and loads each as a skill.
    pub fn load_from_dir(&mut self, dir: &Path) -> Result<usize, String> {
        if !dir.exists() {
            return Ok(0);
        }
        let mut count = 0;
        let entries = std::fs::read_dir(dir)
            .map_err(|e| format!("failed to read skills dir `{}`: {e}", dir.display()))?;

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e == "md").unwrap_or(false) {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    match Skill::from_file(stem, &path) {
                        Ok(skill) => {
                            self.insert(skill);
                            count += 1;
                        }
                        Err(e) => {
                            tracing::warn!("{e}");
                        }
                    }
                }
            }
        }
        Ok(count)
    }

    /// Load skills from `.claude/skills/`.
    pub fn load_from_working_dir(&mut self, working_dir: Option<&Path>) -> Result<usize, String> {
        let Some(dir) = working_dir else { return Ok(0); };
        let claude_skills = dir.join(".claude").join("skills");
        self.load_from_dir(&claude_skills)
    }
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
    }

    #[test]
    fn test_claude_code_minimal_frontmatter() {
        let content = "---\nname: my-skill\n---\n\nJust do it.";
        let skill = Skill::parse("my-skill", content).unwrap();
        assert_eq!(skill.name, "my-skill");
        assert_eq!(skill.description, "my-skill"); // fallback to name
        assert!(skill.allowed_tools.is_empty());
        assert_eq!(skill.prompt, "Just do it.");
    }

    #[test]
    fn test_frontmatter_name_takes_priority() {
        let content = "---\nname: renamed-skill\ndescription: Custom desc\n---\n\nPrompt here.";
        let skill = Skill::parse("filename-skill", content).unwrap();
        assert_eq!(skill.name, "renamed-skill");
        assert_eq!(skill.description, "Custom desc");
    }

    #[test]
    fn test_skill_registry_load_dir() {
        let dir = std::env::temp_dir().join(format!("skill-load-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("pdf.md"),
            "---\nname: pdf\ndescription: PDF skill\n---\n\nPrompt body").unwrap();
        std::fs::write(dir.join("other.md"),
            "---\nname: other\ndescription: Another skill\n---\n\nOther prompt").unwrap();
        std::fs::write(dir.join("note.txt"), "not a skill").unwrap();

        let mut registry = SkillRegistry::default();
        let count = registry.load_from_dir(&dir).unwrap();
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
}
