use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

// ── Core enums ──

/// Trust level for a memory entry.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum TrustLevel {
    /// User explicitly stated this (highest confidence)
    High,
    /// Observed from user behavior
    #[default]
    Medium,
    /// Inferred by the agent (lowest confidence)
    Low,
}

/// Category of a memory entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum MemoryCategory {
    Fact,
    Preference,
    Entity,
    Correction,
    /// A structured narrative record: user asked X → agent did Y → result was Z.
    /// Used by compaction to preserve turn-by-turn conversational history.
    Narrative,
    Custom(String),
}

impl MemoryCategory {
    /// Parse from LLM extraction output.
    pub fn from_extracted(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "fact" | "facts" => MemoryCategory::Fact,
            "preference" | "preferences" | "pref" => MemoryCategory::Preference,
            "correction" | "corrections" | "fix" | "bug" => MemoryCategory::Correction,
            "entity" | "entities" => MemoryCategory::Entity,
            "narrative" | "narratives" => MemoryCategory::Narrative,
            "observation" | "lesson" | "learning" => MemoryCategory::Fact,
            other => MemoryCategory::Custom(other.into()),
        }
    }
}

impl std::fmt::Display for MemoryCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MemoryCategory::Fact => write!(f, "fact"),
            MemoryCategory::Preference => write!(f, "preference"),
            MemoryCategory::Entity => write!(f, "entity"),
            MemoryCategory::Correction => write!(f, "correction"),
            MemoryCategory::Narrative => write!(f, "narrative"),
            MemoryCategory::Custom(s) => write!(f, "{s}"),
        }
    }
}

/// Scope for memory retrieval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryScope {
    /// Session-local memory, not shared across sessions.
    /// Temporary / task-scoped notes, intermediate hypotheses, scratchpad.
    /// Stored in `{storage}/sessions/{session_id}.json`, deleted when session ends.
    Session,
    /// Project-wide memory, shared across all sessions working on the same project.
    /// Long-lived facts, conventions, preferences specific to the project.
    Project,
    /// Global memory, shared across all projects and sessions.
    /// Cross-project preferences, global conventions.
    Global,
    /// All scopes combined (Session + Project + Global).
    /// Used for broad searches; recall will merge and rank across all layers.
    All,
}

impl MemoryScope {
    pub fn includes_session(self) -> bool {
        matches!(self, Self::Session | Self::All)
    }
    pub fn includes_project(self) -> bool {
        matches!(self, Self::Project | Self::All)
    }
    pub fn includes_global(self) -> bool {
        matches!(self, Self::Global | Self::All)
    }
}

/// How to retrieve memories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecallMode {
    /// Most recently updated first
    Recent,
    /// Plain-text keyword search on search_text
    Keyword,
    /// Embedding cosine similarity (needs `memory-embeddings` feature)
    Semantic,
    /// Semantic + BFS graph cascade expansion
    Cascade,
}

// ── Data structures ──

/// A reinforcement breadcrumb tracking when/where a memory was reinforced.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reinforcement {
    pub session_id: String,
    pub message_index: usize,
    pub timestamp: DateTime<Utc>,
}

/// A single memory entry stored in the graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: String,
    pub category: MemoryCategory,
    pub content: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Pre-normalized lowercase search text (content + tags).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub search_text: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub access_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default)]
    pub trust: TrustLevel,
    #[serde(default)]
    pub strength: u32,
    #[serde(default = "default_active")]
    pub active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
    #[serde(default)]
    pub reinforcements: Vec<Reinforcement>,
    /// Embedding vector (384-dim for MiniLM-style).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<Vec<f32>>,
    /// Embedding model identifier used to generate `embedding`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_model: Option<String>,
    /// Embedding model/version fingerprint used to detect re-embed needs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_version: Option<String>,
    /// Confidence score 0.0-1.0.
    #[serde(default = "default_confidence")]
    pub confidence: f32,
}

fn default_confidence() -> f32 {
    1.0
}

fn default_active() -> bool {
    true
}

impl MemoryEntry {
    pub fn new(category: MemoryCategory, content: impl Into<String>) -> Self {
        let now = Utc::now();
        let content = content.into();
        let id = format!("mem_{}", uuid::Uuid::new_v4());
        Self {
            id,
            category,
            search_text: normalize_memory_search_text(&content, &[]),
            content,
            tags: Vec::new(),
            created_at: now,
            updated_at: now,
            access_count: 0,
            source: None,
            trust: TrustLevel::default(),
            strength: 1,
            active: true,
            superseded_by: None,
            reinforcements: Vec::new(),
            embedding: None,
            embedding_model: None,
            embedding_version: None,
            confidence: 1.0,
        }
    }

    pub fn refresh_search_text(&mut self) {
        self.search_text = normalize_memory_search_text(&self.content, &self.tags);
    }

    pub fn searchable_text(&self) -> std::borrow::Cow<'_, str> {
        if self.search_text.is_empty() {
            std::borrow::Cow::Owned(normalize_memory_search_text(&self.content, &self.tags))
        } else {
            std::borrow::Cow::Borrowed(&self.search_text)
        }
    }

    /// Effective confidence after time-based decay.
    /// Half-life: Correction=365d, Preference=90d, Fact=30d, Entity=60d.
    pub fn effective_confidence(&self) -> f32 {
        let age_days = (Utc::now() - self.created_at).num_days() as f32;
        let half_life = match self.category {
            MemoryCategory::Correction => 365.0,
            MemoryCategory::Preference => 90.0,
            MemoryCategory::Fact => 30.0,
            MemoryCategory::Entity => 60.0,
            MemoryCategory::Narrative => 30.0,
            MemoryCategory::Custom(_) => 45.0,
        };
        let decay = f32::exp(-age_days / half_life * 0.693);
        let access_boost = 1.0 + 0.1 * f32::ln(self.access_count as f32 + 1.0);
        (self.confidence * decay * access_boost).min(1.0)
    }

    pub fn boost_confidence(&mut self, amount: f32) {
        self.confidence = (self.confidence + amount).min(1.0);
        self.access_count += 1;
        self.updated_at = Utc::now();
    }

    pub fn decay_confidence(&mut self, amount: f32) {
        self.confidence = (self.confidence - amount).max(0.0);
    }

    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self.refresh_search_text();
        self
    }

    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }

    pub fn with_trust(mut self, trust: TrustLevel) -> Self {
        self.trust = trust;
        self
    }

    pub fn touch(&mut self) {
        self.updated_at = Utc::now();
        self.access_count += 1;
    }

    pub fn reinforce(&mut self, session_id: &str, message_index: usize) {
        self.strength += 1;
        self.updated_at = Utc::now();
        self.reinforcements.push(Reinforcement {
            session_id: session_id.into(),
            message_index,
            timestamp: Utc::now(),
        });
    }

    pub fn supersede(&mut self, new_id: &str) {
        self.active = false;
        self.superseded_by = Some(new_id.into());
    }

    pub fn with_embedding(mut self, embedding: Vec<f32>) -> Self {
        self.embedding = Some(embedding);
        self
    }

    pub fn set_embedding_metadata(
        &mut self,
        embedding: Vec<f32>,
        model_name: impl Into<String>,
        version: impl Into<String>,
    ) {
        self.embedding = Some(embedding);
        self.embedding_model = Some(model_name.into());
        self.embedding_version = Some(version.into());
    }
}

// ── Narrative record ──

/// A structured record of "what happened" in a conversation turn or group
/// of turns. Unlike a `MemoryEntry` (which stores a single fact/preference),
/// a `NarrativeRecord` captures the full arc: user's request → agent's
/// actions → key findings → decisions made.
///
/// These are produced by compaction and stored in the `MemoryGraph` as
/// `MemoryCategory::Narrative` entries (with the record serialized as JSON
/// in the `content` field). They serve as a compact, structured alternative
/// to raw message history, enabling:
///
/// - Fast session restore (load narratives instead of full messages)
/// - Cross-session context (previous session's work visible to the model)
/// - Semantic search over past tasks ("what did I ask about last time?")
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NarrativeRecord {
    /// Turn range this record covers (inclusive).
    pub turn_range: (u64, u64),

    /// What the user asked for.
    pub user_intent: String,

    /// What the agent did (tool-level summary).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions_taken: Vec<String>,

    /// Key findings, discoveries, or results.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<String>,

    /// Files that were created or modified.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files_modified: Vec<String>,

    /// Decisions made or conclusions reached.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<String>,

    /// Unfinished work remaining from this narrative.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_work: Vec<String>,

    /// When this record was created.
    #[serde(default = "chrono::Utc::now")]
    pub created_at: DateTime<Utc>,
}

impl NarrativeRecord {
    /// Create a minimal record with just the user's intent.
    pub fn new(turn_range: (u64, u64), user_intent: impl Into<String>) -> Self {
        Self {
            turn_range,
            user_intent: user_intent.into(),
            actions_taken: Vec::new(),
            findings: Vec::new(),
            files_modified: Vec::new(),
            decisions: Vec::new(),
            pending_work: Vec::new(),
            created_at: Utc::now(),
        }
    }

    /// Format as a compact markdown line for prompt injection.
    pub fn to_prompt_line(&self) -> String {
        let t = self.turn_range;
        let turn = if t.0 == t.1 {
            format!("Turn {}", t.0)
        } else {
            format!("Turns {}-{}", t.0, t.1)
        };
        let mut line = format!("- **{turn}**: {}", self.user_intent);
        if !self.actions_taken.is_empty() {
            line.push_str(&format!(" → {}", self.actions_taken.join(", ")));
        }
        if !self.findings.is_empty() {
            line.push_str(&format!(". Findings: {}", self.findings.join("; ")));
        }
        if !self.decisions.is_empty() {
            line.push_str(&format!(". Decided: {}", self.decisions.join("; ")));
        }
        line
    }

    /// Serialize to JSON for storage as MemoryEntry.content.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Deserialize from MemoryEntry.content.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Convert to a MemoryEntry for storage in MemoryGraph.
    pub fn to_memory_entry(&self, session_id: &str) -> MemoryEntry {
        let content = self.to_json().unwrap_or_default();
        let mut entry = MemoryEntry::new(MemoryCategory::Narrative, content);
        entry.tags = vec![
            format!("session:{session_id}"),
            format!("turn:{}", self.turn_range.0),
            "narrative".to_string(),
        ];
        entry.trust = TrustLevel::High;
        entry.confidence = 0.95;
        entry
    }
}

// ── Text normalization ──

/// Normalize text for search: lowercase, collapse whitespace, normalize separators.
pub fn normalize_search_text(text: &str) -> String {
    let lowered = text.trim().to_lowercase();
    let mut out = String::with_capacity(lowered.len());
    let mut last_was_space = true;
    for ch in lowered.chars() {
        let mapped = if ch.is_whitespace() || matches!(ch, '-' | '_' | '/' | '\\' | '.' | ':') {
            ' '
        } else {
            ch
        };
        if mapped == ' ' {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
        } else {
            out.push(mapped);
            last_was_space = false;
        }
    }
    out.trim_end().to_string()
}

pub fn normalize_memory_search_text(content: &str, tags: &[String]) -> String {
    let nc = normalize_search_text(content);
    let nt: Vec<String> = tags.iter().map(|t| normalize_search_text(t)).filter(|t| !t.is_empty()).collect();
    if nt.is_empty() {
        return nc;
    }
    if nc.is_empty() {
        return nt.join(" ");
    }
    format!("{nc} {}", nt.join(" "))
}

pub fn memory_matches_search(memory: &MemoryEntry, query: &str) -> bool {
    memory.searchable_text().contains(query)
}

/// Score a memory entry for relevance ranking (higher = more relevant).
pub fn memory_score(entry: &MemoryEntry) -> f64 {
    if !entry.active {
        return 0.0;
    }
    let mut score = 0.0;
    let age_hours = (Utc::now() - entry.updated_at).num_hours() as f64;
    score += 100.0 / (1.0 + age_hours / 24.0);
    score += (entry.access_count as f64).sqrt() * 10.0;
    score += match entry.category {
        MemoryCategory::Correction => 50.0,
        MemoryCategory::Preference => 30.0,
        MemoryCategory::Fact => 20.0,
        MemoryCategory::Entity => 10.0,
        MemoryCategory::Narrative => 25.0,
        MemoryCategory::Custom(_) => 5.0,
    };
    score *= match entry.trust {
        TrustLevel::High => 1.5,
        TrustLevel::Medium => 1.0,
        TrustLevel::Low => 0.7,
    };
    score += (entry.strength as f64).ln() * 5.0;
    score
}

pub fn is_skill_memory(entry: &MemoryEntry) -> bool {
    entry.id.starts_with("skill:")
        || entry.source.as_deref() == Some("skill_registry")
        || matches!(&entry.category, MemoryCategory::Custom(name) if name.eq_ignore_ascii_case("Skills"))
}

pub fn collect_skill_query_terms(query_text: &str) -> HashSet<String> {
    const STOPWORDS: &[&str] = &[
        "about", "after", "before", "could", "from", "have", "just", "make", "ready",
        "should", "start", "that", "their", "there", "they", "this", "what", "when",
        "where", "which", "while", "will", "with", "work", "would", "your",
    ];
    let normalized = normalize_search_text(query_text);
    normalized
        .split_whitespace()
        .filter(|term| term.len() >= 4)
        .filter(|term| !STOPWORDS.contains(term))
        .map(str::to_string)
        .collect()
}

pub fn skill_retrieval_bonus(entry: &MemoryEntry, query_terms: &HashSet<String>) -> f32 {
    if !is_skill_memory(entry) || query_terms.is_empty() {
        return 0.0;
    }
    let searchable = entry.searchable_text();
    let overlap = query_terms.iter().filter(|t| searchable.contains(t.as_str())).count();
    match overlap {
        0 | 1 => 0.0,
        2 => 0.08,
        3 => 0.14,
        _ => 0.20,
    }
}
