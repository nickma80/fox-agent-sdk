use std::collections::HashMap;
use std::path::Path;

/// A loaded skill with its prompt fragment.
#[derive(Debug, Clone, Default)]
pub struct Skill {
    /// Unique skill name
    pub name: String,
    /// Human-readable description
    pub description: String,
    /// Prompt fragment injected when the skill is loaded
    pub prompt: String,
}

impl Skill {
    /// Create a new skill from a markdown file.
    ///
    /// File format:
    ///   First line: description (optional, starts with # or is plain text)
    ///   Remaining lines: prompt content
    pub fn from_file(name: impl Into<String>, path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read skill `{}`: {e}", path.display()))?;
        let name = name.into();
        let trimmed = content.trim();
        if trimmed.is_empty() {
            return Err(format!("skill `{name}` file is empty"));
        }

        let (description, prompt) = if let Some(first_line) = trimmed.lines().next() {
            let desc = first_line.trim_start_matches("# ").trim().to_string();
            let rest: Vec<&str> = trimmed.lines().skip(1).collect();
            let prompt = if rest.is_empty() {
                String::new()
            } else {
                rest.join("\n").trim().to_string()
            };
            (if desc.is_empty() { name.clone() } else { desc }, prompt)
        } else {
            (name.clone(), String::new())
        };

        Ok(Self { name, description, prompt })
    }
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
    /// The filename (without `.md` extension) becomes the skill name.
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

    /// Load skills from the default `.fox/skills/` directory under `working_dir`.
    pub fn load_from_working_dir(&mut self, working_dir: Option<&Path>) -> Result<usize, String> {
        let Some(dir) = working_dir else { return Ok(0); };
        let skills_dir = dir.join(".fox").join("skills");
        self.load_from_dir(&skills_dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_skill_from_file() {
        let dir = std::env::temp_dir().join(format!("skill-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.md");
        std::fs::write(&path, "A test skill\n\nThis is the prompt content.").unwrap();

        let skill = Skill::from_file("test", &path).unwrap();
        assert_eq!(skill.name, "test");
        assert_eq!(skill.description, "A test skill");
        assert_eq!(skill.prompt, "This is the prompt content.");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_skill_registry_load_dir() {
        let dir = std::env::temp_dir().join(format!("skill-load-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("one.md"), "First skill\n\ncontent one").unwrap();
        std::fs::write(dir.join("two.md"), "Second skill\n\ncontent two").unwrap();
        std::fs::write(dir.join("note.txt"), "not a skill").unwrap();

        let mut registry = SkillRegistry::default();
        let count = registry.load_from_dir(&dir).unwrap();
        assert_eq!(count, 2);
        assert!(registry.get("one").is_some());
        assert!(registry.get("two").is_some());
        assert_eq!(registry.get("one").unwrap().description, "First skill");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_load_from_nonexistent_dir() {
        let mut registry = SkillRegistry::default();
        let count = registry.load_from_dir(Path::new("/nonexistent/path")).unwrap();
        assert_eq!(count, 0);
    }
}

