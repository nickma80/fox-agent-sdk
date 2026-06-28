use fox_agent_core::{ContextInfo, PlanningStore, PromptBuilder as CorePromptBuilder, SkillInfo, SplitPrompt, render_planning_context_with_store};
use std::path::Path;
use std::sync::Arc;

/// SDK-level prompt builder that wraps the core PromptBuilder with
/// SDK-specific sections (planning context, session identity, AGENTS.md).
#[derive(Clone)]
pub struct PromptBuilder {
    core: CorePromptBuilder,
    /// Optional path to the global/domain-level AGENTS.md.
    /// When `None`, falls back to `$FOX_AGENT_DIR/AGENTS.md` or `~/.fox-agent/AGENTS.md`.
    global_agents_md_path: Option<std::path::PathBuf>,
}

impl PromptBuilder {
    /// Create a new SDK PromptBuilder with version metadata.
    pub fn new(version: impl Into<String>, git_hash: impl Into<String>) -> Self {
        Self {
            core: CorePromptBuilder::new(version, git_hash),
            global_agents_md_path: None,
        }
    }

    /// Set the path to a global/domain-level AGENTS.md.
    ///
    /// Use this when embedding the SDK in a domain-specific application
    /// (e.g. a coding agent) that ships with its own global domain instructions.
    /// The file is loaded in addition to the per-project `<working_dir>/AGENTS.md`.
    pub fn with_global_agents_md_path(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.global_agents_md_path = Some(path.into());
        self
    }

    /// Build a split prompt from all available sections.
    ///
    /// Sections routed to `static_part` (cacheable):
    /// - system template
    /// - session context (date, OS, hardware, git)
    /// - AGENTS.md (project + global)
    /// - prompt overlay
    /// - skills list
    ///
    /// Sections routed to `dynamic_part` (per-turn):
    /// - planning context (todos, plan, goals)
    /// - memory injection
    /// - active skill prompt
    pub fn build_split(
        &self,
        session_id: &str,
        planning_store: &Arc<dyn PlanningStore>,
        working_dir: Option<&Path>,
        skills: &[SkillInfo],
        memory_injection: Option<&str>,
        active_skill: Option<&str>,
    ) -> (SplitPrompt, ContextInfo) {
        // === Dynamic sections (per-turn) ===
        let mut dynamic_sections: Vec<String> = Vec::new();

        let planning_context = render_planning_context_with_store(planning_store.as_ref(), session_id);
        let has_planning_state = !planning_context.is_empty();

        if has_planning_state {
            dynamic_sections.push(format!("# Planning Context\n\n{}", planning_context));
        }

        if let Some(mem) = memory_injection {
            dynamic_sections.push(mem.to_string());
        }

        if let Some(skill) = active_skill {
            dynamic_sections.push(format!("# Active Skill\n\n{}", skill));
        }

        // === Static sections (cacheable) ===
        let mut static_sections: Vec<String> = Vec::new();

        // System template
        static_sections.push(self.core.system_template().to_string());

        // Session context
        let session_ctx = self.core.build_session_context(working_dir);
        static_sections.push(session_ctx);

        // AGENTS.md (project + optional global/domain)
        if let (Some(content), _info) = CorePromptBuilder::load_agents_md(working_dir, self.global_agents_md_path.as_deref()) {
            static_sections.push(content);
        }

        // Prompt overlay
        if let (Some(content), _size) = CorePromptBuilder::load_prompt_overlay(working_dir) {
            static_sections.push(content);
        }

        // Skills list
        if !skills.is_empty() {
            let mut section =
                "# Available Skills\n\nYou have access to the following skills that the user can invoke with `/skillname`:\n"
                    .to_string();
            for skill in skills {
                section.push_str(&format!("\n- `/{} ` - {}", skill.name, skill.description));
            }
            section.push_str(
                "\n\nWhen a user asks about available skills or capabilities, mention these skills.",
            );
            static_sections.push(section);
        }

        let split = self.core.build_split(static_sections, dynamic_sections);

        // Build ContextInfo
        let mut info = ContextInfo {
            system_prompt_chars: self.core.system_template().len(),
            ..Default::default()
        };
        info.total_chars = split.chars();

        (split, info)
    }

    /// Access the underlying core PromptBuilder.
    pub fn core(&self) -> &CorePromptBuilder {
        &self.core
    }

    /// Override the compiled-in system prompt with a custom template.
    ///
    /// Passes through to [`CorePromptBuilder::with_system_template`].
    /// Use for non-coding agent applications that need a different persona.
    pub fn with_system_template(mut self, template: impl Into<String>) -> Self {
        self.core = self.core.with_system_template(template);
        self
    }
}
