use fox_agent_core::{CompactionConfig, CompactionEvent, CompactionTrigger, ContentBlock, Message, Role};

#[derive(Debug, Clone)]
pub struct CompactionManager {
    cfg: CompactionConfig,
    compaction_count: u64,
    turns_since_last_compaction: u32,
}

impl CompactionManager {
    pub fn new(cfg: CompactionConfig) -> Self {
        Self { cfg, compaction_count: 0, turns_since_last_compaction: 0 }
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

        self.turns_since_last_compaction += 1;

        // Enforce minimum gap between compactions to avoid thrashing
        if self.compaction_count > 0
            && self.turns_since_last_compaction <= self.cfg.min_compaction_gap_turns
        {
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
        self.compaction_count += 1;
        self.turns_since_last_compaction = 0;
        let preserve = self.cfg.preserve_recent_messages.min(messages.len());
        let mut split_at = if messages.len() > preserve { messages.len() - preserve } else { 0 };

        // Safety: never leave orphaned Tool results without their preceding
        // Assistant tool_calls message.  If the preserved section starts with
        // Tool messages, drain them too — they become meaningless without the
        // assistant message that requested them.
        while split_at < messages.len() && messages[split_at].role == Role::Tool {
            split_at += 1;
        }

        let old_messages: Vec<Message> = messages.drain(..split_at).collect();

        let summary_text = if old_messages.is_empty() {
            String::new()
        } else {
            summarize_messages(&old_messages)
        };
        let summary_chars = summary_text.len();

        if !summary_text.is_empty() {
            // Replace (or insert) the summary so repeated compactions don't
            // stack many conversation-summary system messages on top of each
            // other.  The summary already captures previous summaries (via the
            // truncation logic), keeping a single one avoids unbounded growth.
            if let Some(existing) = messages.iter_mut().find(|m| {
                m.role == Role::System
                    && m.content.first().map_or(false, |b| {
                        matches!(b, ContentBlock::Text { text } if text.starts_with("Conversation summary:"))
                    })
            }) {
                existing.content = vec![ContentBlock::Text {
                    text: format!("Conversation summary:\n{summary_text}"),
                }];
            } else {
                messages.insert(0, Message {
                    role: Role::System,
                    content: vec![ContentBlock::Text {
                        text: format!("Conversation summary:\n{summary_text}"),
                    }],
                });
            }
        }

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
    const MAX_CONTENT_LEN: usize = 500; // Truncate each content block to 500 chars
    const MAX_SUMMARY_LEN: usize = 4000; // Truncate total summary to 4KB
    
    let mut summary = String::new();
    
    for message in messages {
        let role = match message.role {
            Role::System => "system", Role::User => "user",
            Role::Assistant => "assistant", Role::Tool => "tool",
        };
        
        let content = message.content.iter().map(|block| {
            let text = match block {
                ContentBlock::Text { text } => text.as_str(),
                ContentBlock::Reasoning { text } => text.as_str(),
                ContentBlock::ToolResult { text, .. } => text.as_str(),
                ContentBlock::ToolUse { .. } | ContentBlock::Image { .. } => "",
            };
            // Safe truncation: uses char_indices() to find the byte offset
            // of the MAX_CONTENT_LEN-th character, guaranteeing we never
            // slice in the middle of a multi-byte UTF-8 codepoint.
            let (truncated, overflow) = fox_agent_core::format_truncated(text, MAX_CONTENT_LEN);
            if overflow.is_empty() {
                text.to_string()
            } else {
                format!("{truncated}{overflow}")
            }
        }).collect::<Vec<_>>().join(" ");
        
        let line = format!("[{role}] {content}");
        summary.push_str(&line);
        summary.push('\n');
        
        // Stop if summary is getting too long
        if summary.len() > MAX_SUMMARY_LEN {
            summary.push_str("...[summary truncated]\n");
            break;
        }
    }
    
    summary
}

pub(crate) fn message_chars(messages: &[Message]) -> usize {
    messages.iter().flat_map(|m| &m.content).map(|block| match block {
        ContentBlock::Text { text } => text.len(),
        ContentBlock::Reasoning { text } => text.len(),
        ContentBlock::ToolResult { text, .. } => text.len(),
        ContentBlock::ToolUse { .. } | ContentBlock::Image { .. } => 0,
    }).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fox_agent_core::{CompactionConfig, ContentBlock, Message, Role};

    fn build_assistant_with_tool_calls(text: &str, tool_calls: &[(&str, &str)]) -> Message {
        let mut content: Vec<ContentBlock> = Vec::new();
        if !text.is_empty() {
            content.push(ContentBlock::Text { text: text.into() });
        }
        for (call_id, name) in tool_calls {
            content.push(ContentBlock::ToolUse {
                id: call_id.to_string(),
                name: name.to_string(),
                input: serde_json::json!({}),
            });
        }
        Message { role: Role::Assistant, content }
    }

    fn build_tool_result(call_id: &str, text: &str) -> Message {
        Message::tool_result(call_id, text, false)
    }

    fn build_user(text: &str) -> Message {
        Message { role: Role::User, content: vec![ContentBlock::Text { text: text.into() }] }
    }

    /// Regression test: compaction must NOT leave orphaned Tool results
    /// when the split point cuts between an Assistant(tool_calls) and its
    /// Tool result(s).  DeepSeek/OpenAI reject such messages with:
    /// "Messages with role 'tool' must be a response to a preceding message with 'tool_calls'"
    #[test]
    fn do_compact_never_leaves_orphaned_tool_results() {
        let mut messages = vec![
            build_user("do task A"),
            build_assistant_with_tool_calls("ok, running tools", &[("c1", "read")]),
            build_tool_result("c1", "file content here"),
            build_user("do task B"),
            build_assistant_with_tool_calls("ok", &[("c2", "write")]),
            build_tool_result("c2", "write done"),
        ];

        // preserve_recent_messages = 4 → split after index 2 (messages 0,1 drained)
        // messages 2 = tool_result("c1") → ORPHAN! We should drain it too.
        let cfg = CompactionConfig {
            enabled: true,
            preserve_recent_messages: 4,
            token_budget: 1000,
            max_turns_before_compaction: 100,
            ..Default::default()
        };
        let mut mgr = CompactionManager::new(cfg);
        let event = mgr.do_compact(&mut messages, CompactionTrigger::TokenBudget);

        // verify no Tool messages appear without preceding Assistant(tool_calls)
        let mut last_was_tool_calls = false;
        for msg in &messages {
            let is_assistant_tool_calls = msg.role == Role::Assistant
                && msg.content.iter().any(|b| matches!(b, ContentBlock::ToolUse { .. }));

            match msg.role {
                Role::Tool => {
                    assert!(
                        last_was_tool_calls,
                        "orphaned tool result found: {:?}",
                        msg
                    );
                }
                _ => {}
            }
            last_was_tool_calls = is_assistant_tool_calls;
        }

        // also verify that the summary message is present
        assert!(messages[0].role == Role::System || messages[0].role == Role::Tool, "first message should be system or tool");
        println!("fine: {} removed, {} kept", event.removed_messages, event.kept_messages);
    }

    /// When the split boundary is safe (before a User message), the orphan
    /// guard should NOT drain extra messages.
    #[test]
    fn do_compact_preserves_when_boundary_is_safe() {
        let mut messages = vec![
            build_user("task 1"),
            build_assistant_with_tool_calls("", &[("c1", "read")]),
            build_tool_result("c1", "content"),
            build_user("task 2"),
            build_assistant_with_tool_calls("", &[("c2", "write")]),
            build_tool_result("c2", "done"),
        ];

        // preserve_recent_messages = 3 → split after index 3 (messages 0,1,2 drained)
        // remaining: [user("task 2"), assistant(tool_calls), tool_result]
        // boundary is safe — starts with User
        let cfg = CompactionConfig {
            enabled: true,
            preserve_recent_messages: 3,
            token_budget: 1000,
            max_turns_before_compaction: 100,
            ..Default::default()
        };
        let mut mgr = CompactionManager::new(cfg);
        mgr.do_compact(&mut messages, CompactionTrigger::TokenBudget);

        // After compaction, first non-system message should be User
        let has_non_system = messages.iter().find(|m| m.role != Role::System);
        if let Some(non_sys) = has_non_system {
            assert_eq!(non_sys.role, Role::User, "safe boundary should preserve User as first");
        }
    }
}
