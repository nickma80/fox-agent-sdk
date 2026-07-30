use fox_agent_core::{
    ContextInfo, PlanningStore, PromptBuilder as CorePromptBuilder, SkillInfo, SplitPrompt,
    render_planning_context_with_store,
};
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
    /// Optional summary of connected MCP resources and prompts.
    mcp_context_summary: Option<String>,
}

impl PromptBuilder {
    /// Create a new SDK PromptBuilder with version metadata.
    pub fn new(version: impl Into<String>, git_hash: impl Into<String>) -> Self {
        Self {
            core: CorePromptBuilder::new(version, git_hash),
            global_agents_md_path: None,
            mcp_context_summary: None,
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

    /// Inject a summary of connected MCP resources and prompts.
    ///
    /// This is generated during `AgentBuilder::build()` from the connected
    /// MCP servers and will appear in the system prompt's MCP context section.
    pub fn with_mcp_context(mut self, summary: String) -> Self {
        self.mcp_context_summary = Some(summary);
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
    /// - intent anchor (first user message — always visible)
    /// - planning context (todos, plan, goals)
    /// - memory injection
    /// - active skill prompt
    #[allow(clippy::too_many_arguments)]
    pub fn build_split(
        &self,
        session_id: &str,
        planning_store: &Arc<dyn PlanningStore>,
        working_dir: Option<&Path>,
        skills: &[SkillInfo],
        memory_injection: Option<&str>,
        active_skill: Option<&str>,
        intent_anchor: Option<&str>,
        narrative_prompt: Option<&str>,
        status_text: Option<&str>,
    ) -> (SplitPrompt, ContextInfo) {
        // === Dynamic sections (per-turn, ordered by change frequency: low → high) ===
        let mut dynamic_sections: Vec<String> = Vec::new();

        // 1. Intent Anchor (低频变) — always visible, keeps agent focused on the CURRENT task.
        // This reflects the LATEST user message (not the first), so when the user
        // changes tasks mid-session, the agent follows the new instruction.
        if let Some(anchor) = intent_anchor {
            dynamic_sections.push(format!(
                "# Current Task\n\nThe user's latest instruction is:\n\"\"\"\n{anchor}\n\"\"\"\n\n\
                 SCOPE: Focus on THIS task. Do NOT perform actions from earlier in the \
                 conversation unless explicitly requested again. If the user asked you to \
                 ANALYZE, do NOT modify code. If the user asked you to IMPLEMENT, focus \
                 only on the requested change. If you believe follow-up work is needed, \
                 finish the current task first, then ASK.\n"
            ));
        }

        // 2. Narrative session history (低频变) — compaction-generated summaries.
        // Placed before planning/memory so those sections benefit from prefix cache hits.
        if let Some(narrative_text) = narrative_prompt {
            dynamic_sections.push(narrative_text.to_string());
        }

        // 3. Planning context (中频变) — todo items, plan state, goals.
        let planning_context =
            render_planning_context_with_store(planning_store.as_ref(), session_id);
        let has_planning_state = !planning_context.is_empty();

        if has_planning_state {
            dynamic_sections.push(format!("# Planning Context\n\n{}", planning_context));
        }

        // 4. Memory injection (中频变) — semantic recall results.
        if let Some(mem) = memory_injection {
            dynamic_sections.push(mem.to_string());
        }

        // 5. Active skill (低频变) — skill prompt overlay.
        if let Some(skill) = active_skill {
            dynamic_sections.push(format!(
                "You are currently operating under the following skill: \"{skill}\". Follow its instructions."
            ));
        }

        // 6. Agent Status Bar (高频变) — rendered at the end of the dynamic section.
        // Always visible to the model without polluting the message history.
        if let Some(bar) = status_text {
            dynamic_sections.push(bar.to_string());
        }

        let mut static_sections: Vec<String> = Vec::new();

        // System template
        static_sections.push(self.core.system_template().to_string());

        // Session context
        let session_ctx = self.core.build_session_context(working_dir);
        static_sections.push(session_ctx);

        // AGENTS.md (project + optional global/domain)
        if let (Some(content), _info) =
            CorePromptBuilder::load_agents_md(working_dir, self.global_agents_md_path.as_deref())
        {
            static_sections.push(content);
        }

        // MCP resources & prompts context
        if let Some(ref mcp_ctx) = self.mcp_context_summary {
            static_sections.push(mcp_ctx.clone());
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

    /// Set MCP context after builder construction (non-builder setter).
    pub(crate) fn set_mcp_context(&mut self, summary: String) {
        self.mcp_context_summary = Some(summary);
    }
}
