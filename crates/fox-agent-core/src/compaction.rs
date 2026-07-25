use crate::message::{ContentBlock, Message, Role};

/// What triggered a compaction event.
#[derive(Clone, Debug)]
pub enum CompactionTrigger {
    /// Token/character budget exceeded
    TokenBudget,
    /// Too many turns have occurred
    TurnCount,
    /// User manually requested compaction
    Manual,
    /// Provider is approaching context limit (mid-stream detection)
    ContextLimitApproaching,
    /// Provider-side automatic compaction
    Provider,
}

/// Describes a compaction that occurred.
#[derive(Clone, Debug)]
pub struct CompactionEvent {
    /// What triggered the compaction
    pub trigger: CompactionTrigger,
    /// Number of old messages removed
    pub removed_messages: usize,
    /// Number of messages retained
    pub kept_messages: usize,
    /// Character count of the generated summary
    pub summary_chars: usize,
}

// ── L5: Circuit Breaker (Phase E) ──

/// Circuit breaker state for compaction loops.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CircuitState {
    /// Normal operation — compaction allowed.
    Closed,
    /// Breaker tripped — compaction temporarily disabled.
    Open,
    /// Testing if compaction can work again.
    HalfOpen,
}

/// Circuit breaker that prevents compaction thrashing loops.
///
/// When compaction repeatedly fails to reduce context size (i.e. after
/// compaction, the context is still over budget), the breaker opens and
/// disables further compaction for `cooldown_turns`.
#[derive(Debug, Clone)]
pub struct CompactionCircuitBreaker {
    /// How many times compaction has consecutively failed.
    pub consecutive_failures: u32,
    /// Maximum failures before breaker opens.
    pub max_consecutive_failures: u32,
    /// How many turns to cool down before trying again.
    pub cooldown_turns: u32,
    /// The turn number when the last failure occurred.
    pub last_failure_turn: u64,
    /// Current circuit state.
    pub state: CircuitState,
    /// Track the pre-compaction message count for success/failure detection.
    pre_compact_count: usize,
}

impl CompactionCircuitBreaker {
    pub fn new(max_failures: u32, cooldown: u32) -> Self {
        Self {
            consecutive_failures: 0,
            max_consecutive_failures: max_failures,
            cooldown_turns: cooldown,
            last_failure_turn: 0,
            state: CircuitState::Closed,
            pre_compact_count: 0,
        }
    }

    /// Record the message count before compaction. Call before do_compact().
    pub fn record_pre_compact(&mut self, message_count: usize) {
        self.pre_compact_count = message_count;
    }

    /// Returns true if compaction is allowed.
    pub fn allow_compaction(&mut self, current_turn: u64) -> bool {
        match self.state {
            CircuitState::Closed => true,
            CircuitState::Open => {
                let turns_since_failure = current_turn.saturating_sub(self.last_failure_turn);
                if turns_since_failure > self.cooldown_turns as u64 {
                    self.state = CircuitState::HalfOpen;
                    true
                } else {
                    false
                }
            }
            CircuitState::HalfOpen => true,
        }
    }

    /// Report compaction result.
    /// `post_compact_count` is the message count after compaction.
    pub fn report(&mut self, post_compact_count: usize, current_turn: u64) {
        let reduced = post_compact_count < self.pre_compact_count
            && post_compact_count > 0;

        if reduced {
            self.state = CircuitState::Closed;
            self.consecutive_failures = 0;
        } else {
            self.last_failure_turn = current_turn;
            match self.state {
                CircuitState::Closed | CircuitState::HalfOpen => {
                    self.consecutive_failures += 1;
                    if self.consecutive_failures >= self.max_consecutive_failures {
                        self.state = CircuitState::Open;
                    }
                }
                CircuitState::Open => {}
            }
        }
    }

    /// Returns true if the breaker is currently closed (normal operation).
    pub fn is_closed(&self) -> bool {
        self.state == CircuitState::Closed
    }
}

impl Default for CompactionCircuitBreaker {
    fn default() -> Self {
        Self::new(3, 5)
    }
}

// ── L3: API-level micro-compression (Phase E) ──

/// Threshold for context pressure before L3 micro-compression triggers.
pub const L3_MICRO_COMPRESSION_PRESSURE_THRESHOLD: f64 = 0.9;

/// Candidates for L3 removal: large tool results with low reference counts.
#[derive(Debug)]
pub struct L3RemovalCandidate {
    /// Index in the messages array.
    pub message_index: usize,
    /// Whether this is a tool result or tool use that should be removed together.
    pub reason: String,
}

/// Select tool result messages that are candidates for L3 micro-compression.
///
/// When context pressure exceeds `L3_MICRO_COMPRESSION_PRESSURE_THRESHOLD`,
/// this function identifies large tool results that are unlikely to be
/// referenced by future turns and can be safely removed from the conversation
/// prefix. Each removal invalidates downstream KV cache entries, so the
/// selection is conservative.
pub fn select_messages_for_l3_removal(messages: &[Message], max_removals: usize) -> Vec<usize> {
    const LARGE_TOOL_RESULT_CHARS: usize = 4000;

    let mut candidates: Vec<usize> = Vec::new();

    for (i, msg) in messages.iter().enumerate() {
        if msg.role != Role::Tool {
            continue;
        }
        let text_len: usize = msg.content.iter().map(|b| match b {
            ContentBlock::ToolResult { text, .. } => text.len(),
            _ => 0,
        }).sum();

        if text_len < LARGE_TOOL_RESULT_CHARS {
            continue;
        }

        // Check if preceding message is an assistant that called this tool — if so,
        // remove the tool_use message too (it becomes meaningless without the result).
        let mut indices_to_remove = vec![i];
        if i > 0 && messages[i - 1].role == Role::Assistant {
            let has_tool_call = messages[i - 1].content.iter().any(|b| {
                matches!(b, ContentBlock::ToolUse { .. })
            });
            if has_tool_call {
                indices_to_remove.push(i - 1);
            }
        }
        // Sort descending so removal indices stay valid
        indices_to_remove.sort_by(|a, b| b.cmp(a));
        candidates.extend(indices_to_remove);
    }

    // Deduplicate and sort descending
    candidates.sort_by(|a, b| b.cmp(a));
    candidates.dedup();

    if candidates.len() > max_removals {
        candidates.truncate(max_removals);
    }

    candidates
}

/// Apply L3 micro-compression by removing selected messages from the history.
/// Returns the number of messages removed.
pub fn apply_l3_micro_compression(messages: &mut Vec<Message>, max_removals: usize) -> usize {
    let indices = select_messages_for_l3_removal(messages, max_removals);
    if indices.is_empty() {
        return 0;
    }

    let count = indices.len();
    // Remove in reverse order to keep indices valid
    for idx in &indices {
        if *idx < messages.len() {
            messages.remove(*idx);
        }
    }

    tracing::debug!(
        removed = count,
        remaining = messages.len(),
        "L3 micro-compression removed large tool results",
    );

    count
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Circuit Breaker tests ──

    #[test]
    fn test_circuit_breaker_default_is_closed() {
        let cb = CompactionCircuitBreaker::default();
        assert_eq!(cb.state, CircuitState::Closed);
        assert!(cb.is_closed());
    }

    #[test]
    fn test_circuit_breaker_stays_closed_on_success() {
        let mut cb = CompactionCircuitBreaker::new(3, 5);
        cb.record_pre_compact(10);
        assert!(cb.allow_compaction(1));
        cb.report(5, 1); // reduced from 10 → 5 (success)
        assert_eq!(cb.state, CircuitState::Closed);
        assert_eq!(cb.consecutive_failures, 0);
    }

    #[test]
    fn test_circuit_breaker_opens_after_consecutive_failures() {
        let mut cb = CompactionCircuitBreaker::new(3, 5);
        // Fail 3 times
        for turn in 1..=3 {
            cb.record_pre_compact(10);
            assert!(cb.allow_compaction(turn));
            cb.report(10, turn); // same count = failure
        }
        assert_eq!(cb.state, CircuitState::Open);
        assert!(!cb.is_closed());
    }

    #[test]
    fn test_circuit_breaker_blocks_compaction_when_open() {
        let mut cb = CompactionCircuitBreaker::new(2, 5);
        // Fail 2 times → open
        cb.record_pre_compact(10);
        cb.allow_compaction(1);
        cb.report(10, 1);
        cb.record_pre_compact(10);
        cb.allow_compaction(2);
        cb.report(10, 2);
        assert_eq!(cb.state, CircuitState::Open);

        // Next turn: still open, not enough cooldown
        assert!(!cb.allow_compaction(3));
    }

    #[test]
    fn test_circuit_breaker_goes_half_open_after_cooldown() {
        let mut cb = CompactionCircuitBreaker::new(2, 5);
        // Fail 2 times at turn 1, 2 → open
        cb.record_pre_compact(10);
        cb.allow_compaction(1);
        cb.report(10, 1);
        cb.record_pre_compact(10);
        cb.allow_compaction(2);
        cb.report(10, 2);
        assert_eq!(cb.state, CircuitState::Open);

        // After cooldown (5 turns), should allow half-open
        assert!(cb.allow_compaction(8)); // 8 - 2 = 6 > 5
        assert_eq!(cb.state, CircuitState::HalfOpen);
    }

    #[test]
    fn test_circuit_breaker_recloses_after_success() {
        let mut cb = CompactionCircuitBreaker::new(2, 5);
        // Fail 2 times → open
        cb.record_pre_compact(10);
        cb.allow_compaction(1);
        cb.report(10, 1);
        cb.record_pre_compact(10);
        cb.allow_compaction(2);
        cb.report(10, 2);
        assert_eq!(cb.state, CircuitState::Open);

        // Cooldown → half-open → success
        assert!(cb.allow_compaction(8));
        cb.record_pre_compact(10);
        cb.report(5, 8); // success: 10 → 5
        assert_eq!(cb.state, CircuitState::Closed);
    }

    // ── L3 micro-compression tests ──

    fn make_tool_result_msg(text: &str) -> Message {
        Message {
            role: Role::Tool,
            content: vec![ContentBlock::ToolResult {
                call_id: "call_1".into(),
                text: text.to_string(),
                is_error: false,
            }],
        }
    }

    fn make_assistant_msg(text: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
        }
    }

    #[test]
    fn test_l3_no_removal_when_short() {
        let messages = vec![make_tool_result_msg("short")];
        let indices = select_messages_for_l3_removal(&messages, 10);
        assert!(indices.is_empty(), "short text should not be removed");
    }

    #[test]
    fn test_l3_removes_large_tool_results() {
        let large_text = "x".repeat(5000);
        let messages = vec![
            make_tool_result_msg(&large_text),
        ];
        let indices = select_messages_for_l3_removal(&messages, 10);
        assert!(!indices.is_empty());
        assert!(indices.contains(&0));
    }

    #[test]
    fn test_l3_removes_assistant_tool_call_with_result() {
        let large_text = "x".repeat(5000);
        let messages = vec![
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "read".into(),
                    input: serde_json::json!({"path": "foo"}),
                }],
            },
            Message {
                role: Role::Tool,
                content: vec![ContentBlock::ToolResult {
                    call_id: "call_1".into(),
                    text: large_text,
                    is_error: false,
                }],
            },
        ];
        let indices = select_messages_for_l3_removal(&messages, 10);
        assert!(indices.contains(&1));
        assert!(indices.contains(&0));
    }

    #[test]
    fn test_l3_apply_removes_messages() {
        let large_text = "x".repeat(5000);
        let mut messages = vec![
            make_assistant_msg("Let me search"),
            make_tool_result_msg(&large_text),
            make_assistant_msg("Found results"),
        ];
        let removed = apply_l3_micro_compression(&mut messages, 10);
        assert_eq!(removed, 1);
        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn test_l3_skips_small_results() {
        let mut messages = vec![
            make_assistant_msg("processing"),
            make_tool_result_msg("small output"),
            make_tool_result_msg(&"m".repeat(5000)),
            make_assistant_msg("done"),
        ];
        let removed = apply_l3_micro_compression(&mut messages, 10);
        assert_eq!(removed, 1);
        assert_eq!(messages.len(), 3);
    }
}
