use fox_agent_core::{
    CompactionCircuitBreaker, CompactionConfig, CompactionEvent, CompactionTrigger, ContentBlock,
    Message, NarrativeRecord, Role,
};
use std::future::Future;
use std::pin::Pin;

/// Async summarizer callback: given the messages being dropped, optionally
/// returns an LLM-generated semantic summary. Returning `None` (or an empty
/// string) makes compaction fall back to mechanical truncation.
pub type SummarizerFuture = Pin<Box<dyn Future<Output = Option<String>> + Send>>;

/// When/why `maybe_compact` is being invoked, which controls how aggressive
/// the trigger is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionMode {
    /// Called right before sending the context to the model. Acts purely as
    /// an overflow safety net: compacts ONLY when the context is strictly
    /// over `token_budget`, and bypasses the anti-thrash gap gate (an
    /// overflow must be resolved before the request can succeed). This is
    /// also what protects the first turn after a session restore, where the
    /// restored working context may already exceed the budget.
    ///
    /// It deliberately does NOT fire on the "approaching"/turn-count
    /// triggers, so a follow-up question that is still within budget keeps
    /// the full, un-summarized evidence from previous turns.
    PreSend,
    /// Called after a user-visible turn completes (or on restore warm-up).
    /// Preemptively converges the context so the NEXT turn starts small and
    /// the compaction latency is hidden from the user. Fires on budget,
    /// "approaching" threshold, or turn count, and is gap-gated to avoid
    /// thrashing.
    Proactive,
}

#[derive(Debug, Clone)]
pub struct CompactionManager {
    cfg: CompactionConfig,
    compaction_count: u64,
    turns_since_last_compaction: u32,
    /// L5 circuit breaker — prevents compaction thrashing loops.
    circuit_breaker: CompactionCircuitBreaker,
}

impl CompactionManager {
    pub fn new(cfg: CompactionConfig) -> Self {
        Self {
            cfg,
            compaction_count: 0,
            turns_since_last_compaction: 0,
            circuit_breaker: CompactionCircuitBreaker::default(),
        }
    }

    /// Attach a pre-configured circuit breaker (e.g. from [`ContextManagementConfig`]).
    pub fn with_circuit_breaker(mut self, breaker: CompactionCircuitBreaker) -> Self {
        self.circuit_breaker = breaker;
        self
    }

    /// Check whether compaction is possible (not exceeded max count).
    pub fn can_compact(&self) -> bool {
        self.cfg.enabled && self.compaction_count < self.cfg.max_compaction_count as u64
    }

    /// Auto-compact based on token budget / turn count triggers.
    ///
    /// `mode` selects the trigger aggressiveness (see [`CompactionMode`]).
    /// `summarizer` is invoked with the dropped messages to produce a
    /// semantic summary; if it returns `None` the manager falls back to
    /// mechanical truncation.
    pub async fn maybe_compact<F>(
        &mut self,
        messages: &mut Vec<Message>,
        summarizer: F,
        mode: CompactionMode,
        turn_start: u64,
        turn_end: u64,
    ) -> Option<(CompactionEvent, Vec<NarrativeRecord>)>
    where
        F: FnOnce(Vec<Message>) -> SummarizerFuture,
    {
        if !self.cfg.enabled || messages.len() <= self.cfg.preserve_recent_messages {
            return None;
        }

        let total_chars = message_chars(messages);
        let token_trigger = total_chars > self.cfg.token_budget;

        let threshold_chars =
            (self.cfg.token_budget as f64 * self.cfg.context_limit_threshold) as usize;
        let approaching_trigger = total_chars > threshold_chars && !token_trigger;
        let turn_trigger = messages.len() > self.cfg.max_turns_before_compaction;

        let should_compact = match mode {
            // Overflow safety net only. Bypasses the gap gate: if we're over
            // budget the request cannot proceed, so we must compact now.
            CompactionMode::PreSend => token_trigger,
            // Preemptive convergence. Count this as a turn for gap-gating and
            // fire on any of the three triggers.
            CompactionMode::Proactive => {
                self.turns_since_last_compaction += 1;
                if self.compaction_count > 0
                    && self.turns_since_last_compaction <= self.cfg.min_compaction_gap_turns
                {
                    return None;
                }
                token_trigger || approaching_trigger || turn_trigger
            }
        };

        if !should_compact {
            return None;
        }

        // L5 circuit breaker — prevent thrashing loops
        if !self.circuit_breaker.allow_compaction(turn_end) {
            tracing::warn!(
                state = ?self.circuit_breaker.state,
                consecutive_failures = self.circuit_breaker.consecutive_failures,
                "L5 circuit breaker OPEN — skipping compaction",
            );
            return None;
        }

        let trigger = if token_trigger {
            CompactionTrigger::TokenBudget
        } else if approaching_trigger {
            CompactionTrigger::ContextLimitApproaching
        } else {
            CompactionTrigger::TurnCount
        };

        self.circuit_breaker.record_pre_compact(messages.len());
        let result = self
            .do_compact(messages, trigger, Some(summarizer), turn_start, turn_end)
            .await;
        self.circuit_breaker.report(messages.len(), turn_end);

        if !self.circuit_breaker.is_closed() {
            tracing::warn!(
                state = ?self.circuit_breaker.state,
                consecutive_failures = self.circuit_breaker.consecutive_failures,
                "L5 circuit breaker tripped — compaction did not reduce message count",
            );
        }

        Some(result)
    }

    /// Force compaction immediately (e.g. Manual trigger or context-limit retry).
    /// Uses mechanical summarization only (no LLM).
    pub async fn force_compact(
        &mut self,
        messages: &mut Vec<Message>,
        trigger: CompactionTrigger,
        turn_start: u64,
        turn_end: u64,
    ) -> (CompactionEvent, Vec<NarrativeRecord>) {
        self.do_compact(
            messages,
            trigger,
            None::<fn(Vec<Message>) -> SummarizerFuture>,
            turn_start,
            turn_end,
        )
        .await
    }

    /// Perform the actual compaction operation.
    ///
    /// Returns the CompactionEvent and any NarrativeRecords extracted from
    /// the dropped messages. The caller should store these narratives in
    /// the MemoryGraph for cross-turn and cross-session persistence.
    async fn do_compact<F>(
        &mut self,
        messages: &mut Vec<Message>,
        trigger: CompactionTrigger,
        summarizer: Option<F>,
        turn_start: u64,
        turn_end: u64,
    ) -> (CompactionEvent, Vec<fox_agent_core::NarrativeRecord>)
    where
        F: FnOnce(Vec<Message>) -> SummarizerFuture,
    {
        self.compaction_count += 1;
        self.turns_since_last_compaction = 0;
        let preserve = self.cfg.preserve_recent_messages.min(messages.len());
        let mut split_at = if messages.len() > preserve {
            messages.len() - preserve
        } else {
            0
        };

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
            // Prefer an LLM semantic summary with structured narrative format;
            // fall back to mechanical transcript if disabled or unavailable.
            let llm_summary = match summarizer {
                Some(f) => f(old_messages.clone()).await,
                None => None,
            };
            match llm_summary {
                Some(s) if !s.trim().is_empty() => s,
                _ => mechanical_transcript(&old_messages),
            }
        };
        let summary_chars = summary_text.len();

        if !summary_text.is_empty() {
            // Replace or insert a single "Conversation summary:" system block.
            // Repeated compactions replace (not stack) this block — the new
            // summary subsumes previous ones.
            if let Some(existing) = messages.iter_mut().find(|m| {
                m.role == Role::System
                    && m.content.first().is_some_and(|b| {
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

        // Extract narrative records from the summary for structured memory
        let narratives: Vec<NarrativeRecord> = if !summary_text.is_empty() && summary_chars > 0 {
            extract_narrative_records(&summary_text, turn_start, turn_end)
        } else {
            Vec::new()
        };

        (
            CompactionEvent {
                trigger,
                removed_messages: old_messages.len(),
                kept_messages: messages.len(),
                summary_chars,
            },
            narratives,
        )
    }
}

/// Build the summarization prompt sent to the LLM for a set of dropped messages.
///
/// The prompt asks the LLM to produce a structured narrative record covering:
/// what the user wanted, what the agent did, what was found, and what decisions
/// were made. This structured format survives compaction and session restore.
pub(crate) fn build_summarization_prompt(messages: &[Message]) -> String {
    let transcript = mechanical_transcript(messages);
    format!(
        "You are compacting a long agent conversation to save context space. \
         Extract the following structured information from the conversation \
         segment below. Output ONLY a JSON object with these fields:\n\n\
         ```json\n\
         {{\n\
           \"user_intent\": \"<what the user asked for, one sentence>\",\n\
           \"actions_taken\": [\"<tool name>: <brief description>\", ...],\n\
           \"findings\": [\"<key discovery or result>\", ...],\n\
           \"files_modified\": [\"<file path that was created or edited>\", ...],\n\
           \"decisions\": [\"<decision or conclusion reached>\", ...],\n\
           \"pending_work\": [\"<unfinished task that still needs to be done>\", ...]\n\
         }}\n\
         ```\n\n\
         Rules:\n\
         - user_intent: capture what the USER asked for (not what the assistant did)\n\
         - actions_taken: list the main tools used and what they did (e.g. \"read: docs/plan.md\", \"grep: Sprint 4\", \"write: src/main.rs\")\n\
         - findings: key results, discoveries, evidence from tool outputs\n\
         - files_modified: ONLY files that were actually created or edited (not read)\n\
         - decisions: conclusions the assistant reached, next steps committed to\n\
         - pending_work: tasks the assistant started but did NOT finish\n\
         - Keep arrays concise (max 5 items each). Be specific, not generic.\n\n\
         ## Conversation segment\n{transcript}\n\n## JSON Output"
    )
}

/// Parse structured narrative records from a compaction summary.
///
/// Tries JSON parsing first (for LLM-generated summaries), falls back to
/// building a single narrative record from the raw summary text.
pub(crate) fn extract_narrative_records(
    summary_text: &str,
    turn_start: u64,
    turn_end: u64,
) -> Vec<fox_agent_core::NarrativeRecord> {
    // Try structured JSON first
    if let Ok(record) = serde_json::from_str::<fox_agent_core::NarrativeRecord>(summary_text.trim())
    {
        return vec![record];
    }
    // Try extracting JSON from within markdown code fences
    if let Some(json_start) = summary_text.find("```json") {
        let after_fence = &summary_text[json_start + 7..];
        if let Some(json_end) = after_fence.find("```") {
            let json_str = after_fence[..json_end].trim();
            if let Ok(record) = serde_json::from_str::<fox_agent_core::NarrativeRecord>(json_str) {
                return vec![record];
            }
        }
    }
    // Fallback: wrap the raw summary as a single narrative record
    let text = summary_text.trim();
    if text.is_empty() || text.len() < 20 {
        return Vec::new();
    }
    let mut record = NarrativeRecord::new(
        (turn_start, turn_end),
        format!("(compacted conversation, turns {turn_start}-{turn_end})"),
    );
    record.findings = vec![text.to_string()];
    vec![record]
}

// ── L4: Archival Summarization (Phase D) ──

/// Format a NarrativeRecord into compact markdown suitable for a
/// NarrativeSummary content block.
pub(crate) fn format_narrative_for_prompt(record: &NarrativeRecord, max_chars: usize) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.push(format!(
        "## Turn {}-{}: {}",
        record.turn_range.0, record.turn_range.1, record.user_intent
    ));

    if !record.actions_taken.is_empty() {
        let actions = record
            .actions_taken
            .iter()
            .take(5)
            .map(|a| format!("  - {a}"))
            .collect::<Vec<_>>()
            .join("\n");
        parts.push(format!("Actions:\n{actions}"));
    }

    if !record.findings.is_empty() {
        let findings = record
            .findings
            .iter()
            .take(3)
            .map(|f| format!("  - {f}"))
            .collect::<Vec<_>>()
            .join("\n");
        parts.push(format!("Findings:\n{findings}"));
    }

    if !record.files_modified.is_empty() {
        let files = record
            .files_modified
            .iter()
            .take(5)
            .map(|f| format!("  - {f}"))
            .collect::<Vec<_>>()
            .join("\n");
        parts.push(format!("Files:\n{files}"));
    }

    if !record.decisions.is_empty() {
        let decs = record
            .decisions
            .iter()
            .take(3)
            .map(|d| format!("  - {d}"))
            .collect::<Vec<_>>()
            .join("\n");
        parts.push(format!("Decisions:\n{decs}"));
    }

    let text = parts.join("\n\n");
    if text.len() <= max_chars {
        text
    } else {
        // Truncate to budget while keeping structure
        let truncated: String = text.chars().take(max_chars.saturating_sub(20)).collect();
        format!("{truncated}\n\n... (truncated)")
    }
}

/// Inject NarrativeRecords as NarrativeSummary content blocks into the message
/// history. These accumulate over time — old narratives are retained (up to
/// `max_narratives`) rather than replaced.
///
/// Narratives are inserted after the existing conversation summary (if any),
/// before the preserved messages from recent turns.
pub(crate) fn inject_narrative_summaries(
    messages: &mut Vec<Message>,
    narratives: &[NarrativeRecord],
    max_narratives: usize,
) {
    if narratives.is_empty() {
        return;
    }

    // Build narrative summary content blocks
    let narrative_msgs: Vec<Message> = narratives
        .iter()
        .map(|rec| {
            let text = format_narrative_for_prompt(rec, 500);
            Message {
                role: Role::System,
                content: vec![ContentBlock::NarrativeSummary { text }],
            }
        })
        .collect();

    // Remove old NarrativeSummary blocks if over the limit.
    // Count existing narratives + new ones.
    let existing_count = messages
        .iter()
        .filter(|m| {
            m.content
                .iter()
                .any(|b| matches!(b, ContentBlock::NarrativeSummary { .. }))
        })
        .count();

    // Insert new narratives after any existing conversation summary,
    // but before the rest of the messages.
    // Find the index of the last 'Conversation summary:' system message
    let insert_pos = messages
        .iter()
        .position(|m| {
            m.role == Role::System
                && m.content.first().is_some_and(|b| {
                        matches!(b, ContentBlock::NarrativeSummary { .. })
                    })
        })
        .map(|pos| pos + 1) // After the first narrative
        .unwrap_or_else(|| {
            // No existing narratives — insert after conversation summary if present
            messages
                .iter()
                .position(|m| {
                    m.role == Role::System
                        && m.content.first().is_some_and(|b| {
                                matches!(b, ContentBlock::Text { text } if text.starts_with("Conversation summary:"))
                            })
                })
                .map(|pos| pos + 1)
                .unwrap_or(0)
        });

    // Splice the new narrative messages into position
    for (offset, msg) in narrative_msgs.into_iter().enumerate() {
        messages.insert(insert_pos + offset, msg);
    }

    // Trim total NarrativeSummary blocks to max_narratives
    let total_narratives = existing_count + narratives.len();
    if total_narratives > max_narratives {
        let to_remove = total_narratives - max_narratives;
        let mut removed = 0usize;
        messages.retain(|m| {
            let is_narrative = m
                .content
                .iter()
                .any(|b| matches!(b, ContentBlock::NarrativeSummary { .. }));
            if is_narrative && removed < to_remove {
                removed += 1;
                false
            } else {
                true
            }
        });
        tracing::debug!(
            removed = to_remove,
            max = max_narratives,
            "L4 archival trimmed old narratives"
        );
    }

    tracing::debug!(
        new_narratives = narratives.len(),
        total = total_narratives.min(max_narratives),
        "L4 archival injected narrative summaries",
    );
}

// ── Internal helpers (moved from util.rs) ──

/// Mechanical (non-LLM) transcript builder used both as the LLM prompt input
/// and as the fallback summary when the LLM is unavailable.
fn mechanical_transcript(messages: &[Message]) -> String {
    const MAX_CONTENT_LEN: usize = 500; // Truncate each content block to 500 chars
    const MAX_SUMMARY_LEN: usize = 4000; // Truncate total summary to 4KB

    let mut summary = String::new();

    for message in messages {
        let role = match message.role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };

        let content = message
            .content
            .iter()
            .map(|block| {
                let text = match block {
                    ContentBlock::Text { text } => text.as_str(),
                    ContentBlock::Reasoning { text } => text.as_str(),
                    ContentBlock::ToolResult { text, .. } => text.as_str(),
                    ContentBlock::NarrativeSummary { text } => text.as_str(),
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
            })
            .collect::<Vec<_>>()
            .join(" ");

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
    messages
        .iter()
        .flat_map(|m| &m.content)
        .map(|block| match block {
            ContentBlock::Text { text } => text.len(),
            ContentBlock::Reasoning { text } => text.len(),
            ContentBlock::ToolResult { text, .. } => text.len(),
            ContentBlock::ToolUse { .. } | ContentBlock::Image { .. } => 0,
            ContentBlock::NarrativeSummary { .. } => 0,
        })
        .sum()
}

// ── Compaction artifact detection ──

/// Whether a message is the leading "Conversation summary:" System block.
fn is_summary_block(m: &Message) -> bool {
    m.role == Role::System
        && m.content.first().is_some_and(|b| {
            matches!(b, ContentBlock::Text { text } if text.starts_with("Conversation summary:"))
        })
}

/// Whether a message is a compaction-generated artifact.
#[expect(dead_code)]
fn is_compaction_artifact(m: &Message) -> bool {
    is_summary_block(m)
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
        Message {
            role: Role::Assistant,
            content,
        }
    }

    fn build_tool_result(call_id: &str, text: &str) -> Message {
        Message::tool_result(call_id, text, false)
    }

    fn build_user(text: &str) -> Message {
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }

    /// Regression test: compaction must NOT leave orphaned Tool results
    /// when the split point cuts between an Assistant(tool_calls) and its
    /// Tool result(s).  DeepSeek/OpenAI reject such messages with:
    /// "Messages with role 'tool' must be a response to a preceding message with 'tool_calls'"
    #[tokio::test]
    async fn do_compact_never_leaves_orphaned_tool_results() {
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
        let (event, _) = mgr
            .force_compact(&mut messages, CompactionTrigger::TokenBudget, 1, 1)
            .await;

        // verify no Tool messages appear without preceding Assistant(tool_calls)
        let mut last_was_tool_calls = false;
        for msg in &messages {
            let is_assistant_tool_calls = msg.role == Role::Assistant
                && msg
                    .content
                    .iter()
                    .any(|b| matches!(b, ContentBlock::ToolUse { .. }));

            match msg.role {
                Role::Tool => {
                    assert!(last_was_tool_calls, "orphaned tool result found: {:?}", msg);
                }
                _ => {}
            }
            last_was_tool_calls = is_assistant_tool_calls;
        }

        // also verify that the summary message is present
        assert!(
            messages[0].role == Role::System || messages[0].role == Role::Tool,
            "first message should be system or tool"
        );
        println!(
            "fine: {} removed, {} kept",
            event.removed_messages, event.kept_messages
        );
    }

    /// When the split boundary is safe (before a User message), the orphan
    /// guard should NOT drain extra messages.
    #[tokio::test]
    async fn do_compact_preserves_when_boundary_is_safe() {
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
        let (_, _) = mgr
            .force_compact(&mut messages, CompactionTrigger::TokenBudget, 1, 1)
            .await;

        // After compaction, first non-system message should be User
        let has_non_system = messages.iter().find(|m| m.role != Role::System);
        if let Some(non_sys) = has_non_system {
            assert_eq!(
                non_sys.role,
                Role::User,
                "safe boundary should preserve User as first"
            );
        }
    }

    /// Summarizer stub that always falls back to mechanical truncation.
    fn noop_summarizer(_dropped: Vec<Message>) -> SummarizerFuture {
        Box::pin(async { None })
    }

    fn mode_test_cfg() -> CompactionConfig {
        CompactionConfig {
            enabled: true,
            preserve_recent_messages: 2,
            token_budget: 100,
            context_limit_threshold: 0.85, // threshold = 85 chars
            max_turns_before_compaction: 1000,
            ..Default::default()
        }
    }

    /// PreSend is an overflow safety net: it must NOT compact when the context
    /// is merely "approaching" the budget (so follow-up questions keep the
    /// full evidence), but MUST compact when strictly over budget.
    #[tokio::test]
    async fn presend_only_compacts_on_overflow() {
        // 5 messages × 18 chars = 90 chars: above threshold (85), below budget (100).
        let mut approaching: Vec<Message> = (0..5).map(|_| build_user(&"x".repeat(18))).collect();
        let mut mgr = CompactionManager::new(mode_test_cfg());
        let ev = mgr
            .maybe_compact(
                &mut approaching,
                noop_summarizer,
                CompactionMode::PreSend,
                1,
                1,
            )
            .await;
        assert!(ev.is_none(), "PreSend must not fire on approaching-only");

        // 6 messages × 20 chars = 120 chars: over budget (100).
        let mut overflow: Vec<Message> = (0..6).map(|_| build_user(&"x".repeat(20))).collect();
        let mut mgr2 = CompactionManager::new(mode_test_cfg());
        let ev2 = mgr2
            .maybe_compact(
                &mut overflow,
                noop_summarizer,
                CompactionMode::PreSend,
                1,
                1,
            )
            .await;
        assert!(ev2.is_some(), "PreSend must fire on overflow");
    }

    /// Proactive convergence fires on the "approaching" threshold too, so the
    /// next turn starts smaller.
    #[tokio::test]
    async fn proactive_compacts_on_approaching() {
        let mut approaching: Vec<Message> = (0..5).map(|_| build_user(&"x".repeat(18))).collect();
        let mut mgr = CompactionManager::new(mode_test_cfg());
        let ev = mgr
            .maybe_compact(
                &mut approaching,
                noop_summarizer,
                CompactionMode::Proactive,
                1,
                1,
            )
            .await;
        assert!(ev.is_some(), "Proactive must fire on approaching threshold");
    }
}
