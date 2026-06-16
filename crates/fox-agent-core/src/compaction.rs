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
