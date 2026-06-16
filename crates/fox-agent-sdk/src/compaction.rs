use fox_agent_core::{CompactionConfig, CompactionEvent, CompactionTrigger, ContentBlock, Message, Role};

#[derive(Debug, Clone)]
pub struct CompactionManager {
    cfg: CompactionConfig,
    compaction_count: u64,
}

impl CompactionManager {
    pub fn new(cfg: CompactionConfig) -> Self {
        Self { cfg, compaction_count: 0 }
    }

    /// Check whether compaction is possible (not exceeded max count).
    pub fn can_compact(&self) -> bool {
        self.cfg.enabled && self.compaction_count < self.cfg.max_compaction_count as u64
    }

    /// Auto-compact based on token budget / turn count triggers.
    pub fn maybe_compact(&mut self, messages: &mut Vec<Message>) -> Option<CompactionEvent> {
        if !self.cfg.enabled || messages.len() <= self.cfg.preserve_recent_messages {
            return None;
        }

        let total_chars = message_chars(messages);
        let turn_trigger = messages.len() > self.cfg.max_turns_before_compaction;
        let token_trigger = total_chars > self.cfg.token_budget;

        // ContextLimitApproaching: context is above threshold but not yet over budget
        let threshold_chars = (self.cfg.token_budget as f64 * self.cfg.context_limit_threshold) as usize;
        let approaching_trigger = total_chars > threshold_chars && total_chars <= self.cfg.token_budget;

        if !(turn_trigger || token_trigger || approaching_trigger) {
            return None;
        }

        let trigger = if token_trigger {
            CompactionTrigger::TokenBudget
        } else if approaching_trigger {
            CompactionTrigger::ContextLimitApproaching
        } else {
            CompactionTrigger::TurnCount
        };

        Some(self.do_compact(messages, trigger))
    }

    /// Force compaction immediately (e.g. Manual trigger or context-limit retry).
    pub fn force_compact(&mut self, messages: &mut Vec<Message>, trigger: CompactionTrigger) -> CompactionEvent {
        self.do_compact(messages, trigger)
    }

    /// Perform the actual compaction operation.
    fn do_compact(&mut self, messages: &mut Vec<Message>, trigger: CompactionTrigger) -> CompactionEvent {
        let preserve = self.cfg.preserve_recent_messages.min(messages.len());
        let split_at = if messages.len() > preserve { messages.len() - preserve } else { 0 };
        let old_messages: Vec<Message> = messages.drain(..split_at).collect();

        let summary_text = if old_messages.is_empty() {
            String::new()
        } else {
            summarize_messages(&old_messages)
        };
        let summary_chars = summary_text.len();

        if !summary_text.is_empty() {
            messages.insert(0, Message {
                role: Role::System,
                content: vec![ContentBlock::Text { text: format!("Conversation summary:\n{summary_text}") }],
            });
        }

        self.compaction_count += 1;

        CompactionEvent {
            trigger,
            removed_messages: old_messages.len(),
            kept_messages: messages.len(),
            summary_chars,
        }
    }
}

// ── Internal helpers (moved from util.rs) ──

fn summarize_messages(messages: &[Message]) -> String {
    messages.iter().map(|message| {
        let role = match message.role {
            Role::System => "system", Role::User => "user",
            Role::Assistant => "assistant", Role::Tool => "tool",
        };
        let content = message.content.iter().map(|block| match block {
            ContentBlock::Text { text } => text.as_str(),
            ContentBlock::Reasoning { text } => text.as_str(),
            ContentBlock::ToolResult { text, .. } => text.as_str(),
            ContentBlock::ToolUse { .. } | ContentBlock::Image { .. } => "",
        }).collect::<Vec<_>>().join(" ");
        format!("[{role}] {content}")
    }).collect::<Vec<_>>().join("\n")
}

fn message_chars(messages: &[Message]) -> usize {
    messages.iter().flat_map(|m| &m.content).map(|block| match block {
        ContentBlock::Text { text } => text.len(),
        ContentBlock::Reasoning { text } => text.len(),
        ContentBlock::ToolResult { text, .. } => text.len(),
        ContentBlock::ToolUse { .. } | ContentBlock::Image { .. } => 0,
    }).sum()
}
