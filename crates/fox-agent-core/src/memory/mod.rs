//! Memory system for cross-session learning.
//!
//! Provides persistent memory across sessions, organized by:
//! - Project (per working directory)
//! - Global (user-level preferences)
//!
//! Storage uses MemoryGraph format with JSON files,
//! LRU caching, and automatic backup recovery.

pub mod embedding;
pub mod ann;
pub mod graph;
pub mod prompt;
pub mod ranking;
pub mod relevance;
pub mod storage;
pub mod types;

#[allow(unused_imports)]
pub use embedding::{EmbeddingProvider, MistralEmbeddingProvider};
#[allow(unused_imports)]
pub use graph::{ClusterEntry, Edge, EdgeKind, GRAPH_VERSION, GraphMetadata, MemoryGraph, TagEntry};
#[allow(unused_imports)]
pub use relevance::{ExtractedMemory, MemoryExtractor, MemoryRelevanceChecker};
#[allow(unused_imports)]
pub use storage::{GCResult, MemoryGraphCache, cache_graph, cached_graph, default_storage_dir, gc_memory_files, invalidate_cache, project_hash, read_json, write_json};
#[allow(unused_imports)]
pub use types::{
    MemoryCategory, MemoryEntry, MemoryScope, NarrativeRecord, RecallMode, Reinforcement,
    TrustLevel, memory_matches_search, memory_score, normalize_memory_search_text,
    normalize_search_text,
};

use crate::config::{AutoExtractScope, ContradictionPolicy, MemoryConfig};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::warn;

/// Events emitted by the memory pipeline.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum MemoryStateEvent {
    InjectionComputed { count: u32, memory_ids: Vec<String>, prompt_chars: usize },
    InjectionConsumed { count: u32, memory_ids: Vec<String>, prompt_chars: usize },
    IngestionCompleted {
        created_ids: Vec<String>,
        reinforced_ids: Vec<String>,
        contradiction_ids: Vec<String>,
        skipped: u32,
    },
    Enabled,
    Disabled,
}

/// Memory manager for cross-session learning.
#[derive(Clone)]
pub struct MemoryManager {
    storage_dir: PathBuf,
    project_dir: Option<PathBuf>,
    /// Active session ID for Session-scoped memory isolation.
    session_id: Option<String>,
    test_mode: bool,
    cfg: MemoryConfig,
    embedding_provider: Option<Arc<dyn EmbeddingProvider>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// How a memory was retrieved — used for diagnostics and scoring transparency.
pub enum RetrievalSource {
    /// Returned by recency-only scan (no query).
    Recent,
    /// Matched via keyword term overlap on search_text.
    Keyword,
    /// Matched via cosine similarity over embeddings (brute-force).
    Semantic,
    /// Matched via cosine similarity over embeddings (ANN index).
    SemanticAnn,
    /// Seed hit from semantic/keyword phase in a cascade search.
    CascadeSeed,
    /// Surfaced by graph traversal from a seed hit in a cascade search.
    CascadeGraph,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// Decomposed scoring for a single recall hit — enables explainable retrieval.
pub struct ScoreBreakdown {
    /// Cosine similarity to the query embedding (0.0–1.0). Only present in semantic mode.
    pub semantic_score: Option<f32>,
    /// Keyword term-match ratio (0.0–1.0). Only present in keyword mode.
    pub keyword_score: Option<f32>,
    /// Recency score, derived from `memory_score()` normalized to [0,1].
    pub recency_score: f32,
    /// Graph-traversal relevance score from cascade expansion.
    pub graph_score: Option<f32>,
    /// Trust-level weight (High=1.0, Medium=0.75, Low=0.5).
    pub trust_score: f32,
    /// Weighted composite score used for ranking.
    pub final_score: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// A single memory recall result with full scoring metadata.
pub struct RecallHit {
    /// The matched memory entry.
    pub entry: MemoryEntry,
    /// Weighted composite score (higher = more relevant).
    pub score: f32,
    /// Decomposed scoring explanation.
    pub score_breakdown: ScoreBreakdown,
    /// How this memory was found.
    pub retrieval_source: RetrievalSource,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// Summary of an `ingest_transcript` operation — what was created, reinforced, or contradicted.
pub struct IngestionReport {
    /// IDs of newly created memories.
    pub created_ids: Vec<String>,
    /// IDs of existing memories that were reinforced (duplicate detected).
    pub reinforced_ids: Vec<String>,
    /// IDs of existing memories that the new memory contradicts.
    pub contradiction_ids: Vec<String>,
    /// Duplicate candidates that were skipped.
    pub skipped_duplicates: u32,
    /// Candidates that failed relevance verification.
    pub skipped_irrelevant: u32,
    /// Total number of memories extracted from the transcript.
    pub extracted_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Self-contained memory snapshot for export/import.
///
/// Contains both project and global graphs so a complete memory state can be
/// transferred between sessions or machines.
pub struct MemoryExportBundle {
    /// Format version for forward/backward compatibility.
    pub bundle_version: u32,
    /// Project-scoped memory graph (omitted if not included in export).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<MemoryGraph>,
    /// Session-scoped memory graph (omitted if not included in export).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<MemoryGraph>,
    /// Global-scoped memory graph (omitted if not included in export).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global: Option<MemoryGraph>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// Statistics from an export operation.
pub struct ExportStats {
    /// Number of project memories exported.
    pub project_memories: usize,
    /// Number of session memories exported.
    pub session_memories: usize,
    /// Number of global memories exported.
    pub global_memories: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// Statistics from an import operation.
pub struct ImportStats {
    /// Number of project memories imported (or merged).
    pub project_memories: usize,
    /// Number of session memories imported (or merged).
    pub session_memories: usize,
    /// Number of global memories imported (or merged).
    pub global_memories: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// Statistics from an ANN index rebuild.
pub struct AnnRebuildStats {
    /// Number of vectors indexed for the project scope.
    pub project_vectors: usize,
    /// Number of vectors indexed for the session scope.
    pub session_vectors: usize,
    /// Number of vectors indexed for the global scope.
    pub global_vectors: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// Statistics from a cluster refresh operation.
pub struct ClusterRefreshStats {
    /// Number of clusters created/updated in the project scope.
    pub project_clusters: usize,
    /// Number of clusters created/updated in the global scope.
    pub global_clusters: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// Statistics from a compact (governance) operation.
pub struct CompactStats {
    /// Memories removed from the project graph.
    pub project_removed: usize,
    /// Memories removed from the session graph.
    pub session_removed: usize,
    /// Memories removed from the global graph.
    pub global_removed: usize,
    /// Stale memory files deleted from disk (via GC).
    pub removed_files: usize,
    /// Total memory files scanned during GC.
    pub total_scanned: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// A single audit trail entry for memory operations.
///
/// Appended to `memory.audit.jsonl` for compliance and debugging.
pub struct MemoryAuditEvent {
    /// When the event occurred.
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// Operation name (e.g. "remember", "forget", "redact", "compact").
    pub action: String,
    /// Memory scope affected (omitted for scope-independent operations).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Affected memory ID (omitted for bulk operations).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_id: Option<String>,
    /// Arbitrary operation-specific metadata.
    #[serde(default)]
    pub details: serde_json::Value,
}

impl MemoryManager {
    /// Create a new MemoryManager from config.
    ///
    /// Call `.with_storage_dir()` afterwards to set the actual storage
    /// path (typically `{M::storage_dir}/memory/`).
    pub fn new(config: &MemoryConfig) -> Self {
        Self {
            storage_dir: default_storage_dir(),
            project_dir: None,
            session_id: None,
            test_mode: false,
            cfg: config.clone(),
            embedding_provider: if config.embedding_enabled {
                embedding::create_embedding_provider(config)
            } else {
                None
            },
        }
    }

    /// Set the project directory (for scoping project memories).
    pub fn with_project_dir(mut self, dir: PathBuf) -> Self {
        self.project_dir = Some(dir);
        self
    }

    /// Set the session ID for Session-scoped memory isolation.
    ///
    /// Session memories are stored in `{storage}/sessions/{session_id}.json`
    /// and are not shared with other sessions.  They are intended for
    /// temporary / task-scoped notes, intermediate hypotheses, and
    /// scratchpad entries that should not pollute cross-session recall.
    pub fn with_session_id(mut self, id: impl Into<String>) -> Self {
        self.session_id = Some(id.into());
        self
    }

    /// Create in test mode (uses temp directory).
    pub fn new_test() -> Self {
        let temp = std::env::temp_dir().join(format!("fox-memory-test-{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&temp);
        Self {
            storage_dir: temp.clone(),
            project_dir: None,
            session_id: None,
            test_mode: true,
            cfg: MemoryConfig::default(),
            embedding_provider: None,
        }
    }

    pub fn is_test_mode(&self) -> bool { self.test_mode }

    pub fn with_storage_dir(mut self, dir: PathBuf) -> Self {
        self.storage_dir = dir;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_embedding_provider(mut self, provider: Arc<dyn EmbeddingProvider>) -> Self {
        self.embedding_provider = Some(provider);
        self
    }

    #[cfg(test)]
    pub(crate) fn with_ann_settings(mut self, enabled: bool, min_vectors: usize) -> Self {
        self.cfg.ann_enabled = enabled;
        self.cfg.ann_min_vectors = min_vectors;
        self
    }

    pub fn semantic_enabled(&self) -> bool {
        self.cfg.embedding_enabled && self.embedding_provider.is_some()
    }

    // ── Path helpers ──

    fn project_memory_path(&self) -> Result<PathBuf, String> {
        let project_dir = self.project_dir.clone()
            .or_else(|| std::env::current_dir().ok())
            .ok_or_else(|| "no project directory available".to_string())?;
        let hash = project_hash(&project_dir);
        let dir = if self.test_mode {
            self.storage_dir.clone()
        } else {
            self.storage_dir.join("projects")
        };
        std::fs::create_dir_all(&dir).map_err(|e| format!("failed to create memory dir: {e}"))?;
        Ok(dir.join(format!("{hash}.json")))
    }

    fn global_memory_path(&self) -> PathBuf {
        let dir = if self.test_mode {
            self.storage_dir.clone()
        } else {
            self.storage_dir.clone()
        };
        let _ = std::fs::create_dir_all(&dir);
        dir.join("global.json")
    }

    fn session_memory_path(&self) -> Result<PathBuf, String> {
        let sid = self.session_id.as_ref()
            .ok_or_else(|| "no session ID set — call with_session_id() first".to_string())?;
        // Sanitize: replace path separators to prevent directory traversal
        let safe_id = sid.replace(['/', '\\', ':', '<', '>', '|', '?', '*', '"'], "_");
        let dir = if self.test_mode {
            self.storage_dir.clone()
        } else {
            self.storage_dir.join("session_scoped")
        };
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("failed to create session_scoped dir: {e}"))?;
        Ok(dir.join(format!("{safe_id}.json")))
    }

    // ── Graph load/save ──

    fn load_graph(&self, path: &Path) -> Result<MemoryGraph, String> {
        // Try cache first
        if !self.test_mode {
            if let Some(cached) = storage::cached_graph(path) {
                return Ok(cached);
            }
        }

        if !path.exists() {
            return Ok(MemoryGraph::new());
        }

        let graph = storage::read_json::<MemoryGraph>(path)?;
        if !self.test_mode {
            storage::cache_graph(path.to_path_buf(), &graph);
        }
        Ok(graph)
    }

    fn save_graph(&self, path: &Path, graph: &MemoryGraph) -> Result<(), String> {
        storage::write_json(path, graph)?;
        if self.cfg.ann_enabled {
            ann::invalidate_ann_index(path);
        }
        if !self.test_mode {
            storage::cache_graph(path.to_path_buf(), graph);
        }
        Ok(())
    }

    fn load_project_graph(&self) -> Result<MemoryGraph, String> {
        let path = self.project_memory_path()?;
        self.load_graph(&path)
    }

    fn save_project_graph(&self, graph: &MemoryGraph) -> Result<(), String> {
        let path = self.project_memory_path()?;
        self.save_graph(&path, graph)
    }

    fn load_global_graph(&self) -> Result<MemoryGraph, String> {
        let path = self.global_memory_path();
        self.load_graph(&path)
    }

    fn save_global_graph(&self, graph: &MemoryGraph) -> Result<(), String> {
        let path = self.global_memory_path();
        self.save_graph(&path, graph)
    }

    fn load_session_graph(&self) -> Result<MemoryGraph, String> {
        let path = self.session_memory_path()?;
        self.load_graph(&path)
    }

    fn save_session_graph(&self, graph: &MemoryGraph) -> Result<(), String> {
        let path = self.session_memory_path()?;
        self.save_graph(&path, graph)
    }

    // ── CRUD: remember ──

    /// Store a memory in the project scope.
    pub fn remember_project(&self, entry: MemoryEntry) -> Result<String, String> {
        let mut graph = self.load_project_graph()?;
        if let Some(provider) = &self.embedding_provider {
            self.maybe_rebuild_graph_for_model_change(&mut graph, provider.as_ref())?;
        }
        let entry = self.prepare_entry_for_storage(entry);
        let id = graph.add_memory(entry);
        self.refresh_graph_embedding_metadata(&mut graph);
        self.apply_governance_policies(&mut graph);
        self.save_project_graph(&graph)?;
        Ok(id)
    }

    /// Store a memory in the global scope.
    pub fn remember_global(&self, entry: MemoryEntry) -> Result<String, String> {
        let mut graph = self.load_global_graph()?;
        if let Some(provider) = &self.embedding_provider {
            self.maybe_rebuild_graph_for_model_change(&mut graph, provider.as_ref())?;
        }
        let entry = self.prepare_entry_for_storage(entry);
        let id = graph.add_memory(entry);
        self.refresh_graph_embedding_metadata(&mut graph);
        self.apply_governance_policies(&mut graph);
        self.save_global_graph(&graph)?;
        Ok(id)
    }

    /// Store a memory in the session-local scope.
    ///
    /// Requires `with_session_id()` to have been called.
    pub fn remember_session(&self, entry: MemoryEntry) -> Result<String, String> {
        let mut graph = self.load_session_graph()?;
        if let Some(provider) = &self.embedding_provider {
            self.maybe_rebuild_graph_for_model_change(&mut graph, provider.as_ref())?;
        }
        let entry = self.prepare_entry_for_storage(entry);
        let id = graph.add_memory(entry);
        self.refresh_graph_embedding_metadata(&mut graph);
        self.apply_governance_policies(&mut graph);
        self.save_session_graph(&graph)?;
        Ok(id)
    }

    /// Store a memory in the appropriate scope.
    pub fn remember(&self, entry: MemoryEntry, scope: MemoryScope) -> Result<String, String> {
        match scope {
            MemoryScope::Session => self.remember_session(entry),
            MemoryScope::Project => self.remember_project(entry),
            MemoryScope::Global => self.remember_global(entry),
            MemoryScope::All => self.remember_project(entry), // default to project for All
        }
    }

    /// Promote a memory from one scope to a longer-lived scope.
    ///
    /// Copies the entry into the target scope (preserving its content, tags,
    /// category, strength and confidence), then removes it from the source
    /// scope. Used to graduate valuable session-local memories to the project
    /// or global scope so they survive beyond the current session.
    ///
    /// Returns the ID of the promoted memory in the target scope.
    pub fn promote_memory(
        &self,
        id: &str,
        from: MemoryScope,
        to: MemoryScope,
    ) -> Result<String, String> {
        if from == to {
            return Err("promote: source and target scope are identical".to_string());
        }
        if matches!(to, MemoryScope::Session) {
            return Err("promote: cannot promote INTO session scope".to_string());
        }

        // Extract the entry from the source scope.
        let mut source_graph = self.load_write_scope_graph(from)?;
        let mut entry = source_graph
            .get_memory(id)
            .ok_or_else(|| format!("promote: memory '{id}' not found in {} scope", scope_name(from)))?
            .clone();

        // Record provenance so the promotion is auditable.
        entry.source = Some(format!("promoted_from:{}", scope_name(from)));
        entry.updated_at = chrono::Utc::now();

        // Write the copy into the target scope.
        let mut target_graph = self.load_write_scope_graph(to)?;
        if let Some(provider) = &self.embedding_provider {
            self.maybe_rebuild_graph_for_model_change(&mut target_graph, provider.as_ref())?;
        }
        let new_id = target_graph.add_memory(entry);
        self.refresh_graph_embedding_metadata(&mut target_graph);
        self.apply_governance_policies(&mut target_graph);
        self.save_write_scope_graph(to, &target_graph)?;

        // Remove from the source scope only after the target write succeeds.
        source_graph.remove_memory(id);
        self.save_write_scope_graph(from, &source_graph)?;

        Ok(new_id)
    }

    // ── CRUD: narratives ──

    /// Store a narrative record in session scope.
    pub fn remember_narrative(&self, record: &NarrativeRecord, session_id: &str) -> Result<String, String> {
        let entry = record.to_memory_entry(session_id);
        self.remember_session(entry)
    }

    /// List narrative records for the current session, ordered by turn range.
    pub fn list_narratives(&self, limit: usize) -> Result<Vec<NarrativeRecord>, String> {
        let all = self.list(MemoryScope::Session)?;
        let mut records: Vec<NarrativeRecord> = all
            .into_iter()
            .filter(|e| e.category == MemoryCategory::Narrative && e.active)
            .filter_map(|e| NarrativeRecord::from_json(&e.content).ok())
            .collect();
        records.sort_by_key(|r| r.turn_range.0);
        if limit > 0 && records.len() > limit {
            records = records.into_iter().rev().take(limit).rev().collect();
        }
        Ok(records)
    }

    /// Build a "Session History" prompt section from stored narratives.
    pub fn build_narrative_prompt(&self, limit: usize) -> Option<String> {
        let records = self.list_narratives(limit).ok()?;
        if records.is_empty() {
            return None;
        }
        let mut text = String::from("## Session History\n\n");
        for r in &records {
            text.push_str(&r.to_prompt_line());
            text.push('\n');
        }
        Some(text)
    }

    // ── CRUD: recall ──

    /// Recall memories. Mode controls retrieval strategy.
    pub fn recall(&self, query: Option<&str>, limit: usize, mode: RecallMode, scope: MemoryScope) -> Result<Vec<(MemoryEntry, f32)>, String> {
        Ok(self
            .recall_detailed(query, limit, mode, scope)?
            .into_iter()
            .map(|hit| (hit.entry, hit.score))
            .collect())
    }

    pub fn recall_detailed(&self, query: Option<&str>, limit: usize, mode: RecallMode, scope: MemoryScope) -> Result<Vec<RecallHit>, String> {
        match mode {
            RecallMode::Recent => self.recall_recent(limit, scope),
            RecallMode::Keyword => {
                let q = query.unwrap_or("");
                if q.is_empty() { return Ok(Vec::new()); }
                self.recall_keyword(q, limit, scope)
            }
            RecallMode::Semantic => {
                let q = query.unwrap_or("");
                if q.is_empty() { return Ok(Vec::new()); }
                self.recall_semantic(q, limit, scope)
            }
            RecallMode::Cascade => {
                let q = query.unwrap_or("");
                if q.is_empty() { return Ok(Vec::new()); }
                self.recall_cascade(q, limit, scope)
            }
        }
    }

    fn recall_recent(&self, limit: usize, scope: MemoryScope) -> Result<Vec<RecallHit>, String> {
        let all = self.collect_memories(scope)?;
        let scored: Vec<RecallHit> = all.into_iter()
            .filter(|e| e.active)
            .map(|e| {
                let recency = normalize_memory_score(&e);
                let trust = normalize_trust_score(&e);
                let score = (recency * 0.85) + (trust * 0.15);
                RecallHit {
                    entry: e,
                    score,
                    score_breakdown: ScoreBreakdown {
                        recency_score: recency,
                        trust_score: trust,
                        final_score: score,
                        ..Default::default()
                    },
                    retrieval_source: RetrievalSource::Recent,
                }
            })
            .collect();
        Ok(top_k_hits(scored, limit))
    }

    fn recall_keyword(&self, query: &str, limit: usize, scope: MemoryScope) -> Result<Vec<RecallHit>, String> {
        let nq = normalize_search_text(query);
        if nq.is_empty() { return Ok(Vec::new()); }
        let all = self.collect_memories(scope)?;
        let matches: Vec<RecallHit> = all.into_iter()
            .filter(|e| e.active && memory_matches_search(e, &nq))
            .map(|e| {
                let keyword = keyword_match_score(&e, &nq);
                let recency = normalize_memory_score(&e);
                let trust = normalize_trust_score(&e);
                let score = (keyword * 0.65) + (recency * 0.2) + (trust * 0.15);
                RecallHit {
                    entry: e,
                    score,
                    score_breakdown: ScoreBreakdown {
                        keyword_score: Some(keyword),
                        recency_score: recency,
                        trust_score: trust,
                        final_score: score,
                        ..Default::default()
                    },
                    retrieval_source: RetrievalSource::Keyword,
                }
            })
            .collect();
        Ok(top_k_hits(matches, limit))
    }

    fn recall_cascade(&self, query: &str, limit: usize, scope: MemoryScope) -> Result<Vec<RecallHit>, String> {
        let seed_hits = if self.semantic_enabled() {
            self.recall_semantic(query, limit * 2, scope)?
        } else {
            self.recall_keyword(query, limit * 2, scope)?
        };
        if seed_hits.is_empty() { return Ok(Vec::new()); }

        let seed_ids: Vec<String> = seed_hits.iter().map(|hit| hit.entry.id.clone()).collect();
        let seed_scores: Vec<f32> = seed_hits.iter().map(|hit| hit.score).collect();
        let mut merged: HashMap<String, RecallHit> = seed_hits
            .into_iter()
            .map(|mut hit| {
                hit.retrieval_source = RetrievalSource::CascadeSeed;
                (hit.entry.id.clone(), hit)
            })
            .collect();

        let all = self.collect_memories(scope)?;
        let entry_map: HashMap<String, MemoryEntry> = all.into_iter().map(|entry| (entry.id.clone(), entry)).collect();

        if scope.includes_project() {
            if let Ok(graph) = self.load_project_graph() {
                let cascaded = graph.cascade_retrieve(&seed_ids, &seed_scores, self.cfg.max_graph_depth.max(1), limit * 3);
                apply_cascade_results(&mut merged, &entry_map, cascaded);
            }
        }
        if scope.includes_session() {
            if let Ok(graph) = self.load_session_graph() {
                let cascaded = graph.cascade_retrieve(&seed_ids, &seed_scores, self.cfg.max_graph_depth.max(1), limit * 3);
                apply_cascade_results(&mut merged, &entry_map, cascaded);
            }
        }
        if scope.includes_global() {
            if let Ok(graph) = self.load_global_graph() {
                let cascaded = graph.cascade_retrieve(&seed_ids, &seed_scores, self.cfg.max_graph_depth.max(1), limit * 3);
                apply_cascade_results(&mut merged, &entry_map, cascaded);
            }
        }

        Ok(top_k_hits(merged.into_values().collect(), limit))
    }

    // ── CRUD: search ──

    /// Search memories by text (exact substring match on search_text).
    pub fn search(&self, text: &str, scope: MemoryScope) -> Result<Vec<MemoryEntry>, String> {
        let nq = normalize_search_text(text);
        if nq.is_empty() { return Ok(Vec::new()); }
        let all = self.collect_memories(scope)?;
        Ok(all.into_iter().filter(|e| memory_matches_search(e, &nq)).collect())
    }

    // ── CRUD: list / forget ──

    /// List all memories, newest first.
    pub fn list(&self, scope: MemoryScope) -> Result<Vec<MemoryEntry>, String> {
        let mut all = self.collect_memories(scope)?;
        all.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(all)
    }

    /// Delete a memory by ID.
    pub fn forget(&self, id: &str) -> Result<bool, String> {
        // Try project first
        let mut project = self.load_project_graph()?;
        if project.remove_memory(id).is_some() {
            self.save_project_graph(&project)?;
            self.append_audit_event("forget", Some(MemoryScope::Project), Some(id), json!({}))?;
            return Ok(true);
        }
        // Try global
        let mut global = self.load_global_graph()?;
        if global.remove_memory(id).is_some() {
            self.save_global_graph(&global)?;
            self.append_audit_event("forget", Some(MemoryScope::Global), Some(id), json!({}))?;
            return Ok(true);
        }
        // Try session
        if self.session_id.is_some() {
            let mut session = self.load_session_graph()?;
            if session.remove_memory(id).is_some() {
                self.save_session_graph(&session)?;
                self.append_audit_event("forget", Some(MemoryScope::Session), Some(id), json!({}))?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn disable_memory(&self, id: &str) -> Result<bool, String> {
        self.set_memory_active(id, false)
    }

    pub fn enable_memory(&self, id: &str) -> Result<bool, String> {
        self.set_memory_active(id, true)
    }

    pub fn redact_memory(&self, id: &str, replacement: &str) -> Result<bool, String> {
        if replacement.trim().is_empty() {
            return Err("replacement content cannot be empty".to_string());
        }

        let mut project = self.load_project_graph()?;
        if let Some(entry) = project.get_memory_mut(id) {
            let previous = entry.content.clone();
            entry.content = replacement.to_string();
            entry.embedding = None;
            entry.embedding_model = None;
            entry.embedding_version = None;
            entry.refresh_search_text();
            if let Some(provider) = &self.embedding_provider {
                match provider.embed_text(&entry.content) {
                    Ok(embedding) => entry.set_embedding_metadata(
                        embedding,
                        provider.model_name().to_string(),
                        provider.version().to_string(),
                    ),
                    Err(err) => warn!(memory_id = %id, error = %err, "Failed to regenerate embedding after redaction"),
                }
            }
            self.refresh_graph_embedding_metadata(&mut project);
            self.save_project_graph(&project)?;
            self.append_audit_event(
                "redact",
                Some(MemoryScope::Project),
                Some(id),
                json!({ "before": previous, "after": replacement }),
            )?;
            return Ok(true);
        }

        let mut global = self.load_global_graph()?;
        if let Some(entry) = global.get_memory_mut(id) {
            let previous = entry.content.clone();
            entry.content = replacement.to_string();
            entry.embedding = None;
            entry.embedding_model = None;
            entry.embedding_version = None;
            entry.refresh_search_text();
            if let Some(provider) = &self.embedding_provider {
                match provider.embed_text(&entry.content) {
                    Ok(embedding) => entry.set_embedding_metadata(
                        embedding,
                        provider.model_name().to_string(),
                        provider.version().to_string(),
                    ),
                    Err(err) => warn!(memory_id = %id, error = %err, "Failed to regenerate embedding after redaction"),
                }
            }
            self.refresh_graph_embedding_metadata(&mut global);
            self.save_global_graph(&global)?;
            self.append_audit_event(
                "redact",
                Some(MemoryScope::Global),
                Some(id),
                json!({ "before": previous, "after": replacement }),
            )?;
            return Ok(true);
        }
        Ok(false)
    }

    // ── Graph operations ──

    pub fn tag_memory(&self, memory_id: &str, tag: &str) -> Result<(), String> {
        let mut graph = self.load_project_graph()?;
        if graph.memories.contains_key(memory_id) {
            graph.tag_memory(memory_id, tag);
            return self.save_project_graph(&graph);
        }
        let mut graph = self.load_global_graph()?;
        if graph.memories.contains_key(memory_id) {
            graph.tag_memory(memory_id, tag);
            return self.save_global_graph(&graph);
        }
        Err(format!("Memory not found: {memory_id}"))
    }

    pub fn link_memories(&self, from_id: &str, to_id: &str, weight: f32) -> Result<(), String> {
        // Try project first
        let mut graph = self.load_project_graph()?;
        if graph.memories.contains_key(from_id) && graph.memories.contains_key(to_id) {
            graph.link_memories(from_id, to_id, weight);
            return self.save_project_graph(&graph);
        }
        let mut graph = self.load_global_graph()?;
        if graph.memories.contains_key(from_id) && graph.memories.contains_key(to_id) {
            graph.link_memories(from_id, to_id, weight);
            return self.save_global_graph(&graph);
        }
        Err("Both memories must be in the same store (project or global)".to_string())
    }

    pub fn get_related(&self, memory_id: &str, depth: usize) -> Result<Vec<MemoryEntry>, String> {
        let (graph, _) = {
            let pg = self.load_project_graph()?;
            if pg.memories.contains_key(memory_id) {
                (pg, true)
            } else {
                let gg = self.load_global_graph()?;
                if gg.memories.contains_key(memory_id) {
                    (gg, false)
                } else {
                    return Err(format!("Memory not found: {memory_id}"));
                }
            }
        };
        let results = graph.cascade_retrieve(&[memory_id.to_string()], &[1.0], depth, 20);
        let entries: Vec<MemoryEntry> = results.into_iter()
            .filter(|(id, _)| id != memory_id)
            .filter_map(|(id, _)| graph.get_memory(&id).cloned())
            .collect();
        Ok(entries)
    }

    pub fn graph_stats(&self) -> Result<(usize, usize, usize, usize), String> {
        let project = self.load_project_graph()?;
        let global = self.load_global_graph()?;
        let memories = project.memory_count() + global.memory_count();
        let tags = project.tags.len() + global.tags.len();
        let edges = project.edge_count() + global.edge_count();
        let clusters = project.clusters.len() + global.clusters.len();
        Ok((memories, tags, edges, clusters))
    }

    pub fn refresh_clusters(&self, scope: MemoryScope) -> Result<ClusterRefreshStats, String> {
        let mut stats = ClusterRefreshStats::default();
        if scope.includes_project() {
            let mut graph = self.load_project_graph()?;
            stats.project_clusters = graph.refresh_clusters(
                self.cfg.cluster_similarity_threshold,
                self.cfg.cluster_min_members,
            );
            self.save_project_graph(&graph)?;
        }
        if scope.includes_global() {
            let mut graph = self.load_global_graph()?;
            stats.global_clusters = graph.refresh_clusters(
                self.cfg.cluster_similarity_threshold,
                self.cfg.cluster_min_members,
            );
            self.save_global_graph(&graph)?;
        }
        self.append_audit_event(
            "refresh_clusters",
            Some(scope),
            None,
            json!({
                "project_clusters": stats.project_clusters,
                "global_clusters": stats.global_clusters,
            }),
        )?;
        Ok(stats)
    }

    // ── Persistence ──

    /// Save all graphs to disk.
    pub fn save(&self) -> Result<(), String> {
        // Graphs are saved on every mutation, so this is just a flush
        Ok(())
    }

    /// Run garbage collection on old memory files.
    pub fn gc(&self, max_age_hours: u64) -> Result<GCResult, String> {
        gc_memory_files(&self.storage_dir, max_age_hours)
    }

    pub fn compact(&self, scope: MemoryScope, max_age_hours: u64) -> Result<CompactStats, String> {
        let mut stats = CompactStats::default();
        if scope.includes_project() {
            let mut graph = self.load_project_graph()?;
            stats.project_removed = self.apply_governance_policies(&mut graph);
            self.save_project_graph(&graph)?;
        }
        if scope.includes_session() && self.session_id.is_some() {
            let mut graph = self.load_session_graph()?;
            stats.session_removed = self.apply_governance_policies(&mut graph);
            self.save_session_graph(&graph)?;
        }
        if scope.includes_global() {
            let mut graph = self.load_global_graph()?;
            stats.global_removed = self.apply_governance_policies(&mut graph);
            self.save_global_graph(&graph)?;
        }
        let gc = self.gc(max_age_hours)?;
        stats.removed_files = gc.removed_files;
        stats.total_scanned = gc.total_scanned;
        self.append_audit_event(
            "compact",
            Some(scope),
            None,
            json!({
                "project_removed": stats.project_removed,
                "session_removed": stats.session_removed,
                "global_removed": stats.global_removed,
                "removed_files": stats.removed_files,
                "total_scanned": stats.total_scanned,
            }),
        )?;
        Ok(stats)
    }

    pub fn reembed(&self, scope: MemoryScope) -> Result<usize, String> {
        let provider = match &self.embedding_provider {
            Some(provider) => Arc::clone(provider),
            None => return Ok(0),
        };
        let mut updated = 0usize;
        if scope.includes_project() {
            let mut graph = self.load_project_graph()?;
            updated += self.reembed_graph(&mut graph, provider.as_ref())?;
            self.apply_governance_policies(&mut graph);
            self.save_project_graph(&graph)?;
        }
        if scope.includes_session() && self.session_id.is_some() {
            let mut graph = self.load_session_graph()?;
            updated += self.reembed_graph(&mut graph, provider.as_ref())?;
            self.apply_governance_policies(&mut graph);
            self.save_session_graph(&graph)?;
        }
        if scope.includes_global() {
            let mut graph = self.load_global_graph()?;
            updated += self.reembed_graph(&mut graph, provider.as_ref())?;
            self.apply_governance_policies(&mut graph);
            self.save_global_graph(&graph)?;
        }
        self.append_audit_event("reembed", Some(scope), None, json!({ "updated": updated }))?;
        Ok(updated)
    }

    pub fn reindex(&self, scope: MemoryScope) -> Result<usize, String> {
        let mut updated = 0usize;
        if scope.includes_project() {
            let mut graph = self.load_project_graph()?;
            updated += self.reindex_graph(&mut graph);
            self.apply_governance_policies(&mut graph);
            self.save_project_graph(&graph)?;
        }
        if scope.includes_global() {
            let mut graph = self.load_global_graph()?;
            updated += self.reindex_graph(&mut graph);
            self.apply_governance_policies(&mut graph);
            self.save_global_graph(&graph)?;
        }
        self.append_audit_event("reindex", Some(scope), None, json!({ "updated": updated }))?;
        Ok(updated)
    }

    pub fn rebuild_ann(&self, scope: MemoryScope) -> Result<AnnRebuildStats, String> {
        let mut stats = AnnRebuildStats::default();
        if scope.includes_project() {
            let graph_path = self.project_memory_path()?;
            self.ensure_scope_embeddings_current(MemoryScope::Project)?;
            let graph = self.load_graph(&graph_path)?;
            if graph.metadata.total_embeddings == 0 {
                ann::invalidate_ann_index(&graph_path);
            } else {
                let ann = ann::rebuild_ann_index(
                    &graph_path,
                    &graph,
                    graph.metadata.embedding_model.as_deref(),
                    graph.metadata.embedding_version.as_deref(),
                )?;
                stats.project_vectors = ann.vectors_indexed;
            }
        }
        if scope.includes_session() && self.session_id.is_some() {
            let graph_path = self.session_memory_path()?;
            self.ensure_scope_embeddings_current(MemoryScope::Session)?;
            let graph = self.load_graph(&graph_path)?;
            if graph.metadata.total_embeddings == 0 {
                ann::invalidate_ann_index(&graph_path);
            } else {
                let ann = ann::rebuild_ann_index(
                    &graph_path,
                    &graph,
                    graph.metadata.embedding_model.as_deref(),
                    graph.metadata.embedding_version.as_deref(),
                )?;
                stats.session_vectors = ann.vectors_indexed;
            }
        }
        if scope.includes_global() {
            let graph_path = self.global_memory_path();
            self.ensure_scope_embeddings_current(MemoryScope::Global)?;
            let graph = self.load_graph(&graph_path)?;
            if graph.metadata.total_embeddings == 0 {
                ann::invalidate_ann_index(&graph_path);
            } else {
                let ann = ann::rebuild_ann_index(
                    &graph_path,
                    &graph,
                    graph.metadata.embedding_model.as_deref(),
                    graph.metadata.embedding_version.as_deref(),
                )?;
                stats.global_vectors = ann.vectors_indexed;
            }
        }
        self.append_audit_event(
            "rebuild_ann",
            Some(scope),
            None,
            json!({
                "project_vectors": stats.project_vectors,
                "session_vectors": stats.session_vectors,
                "global_vectors": stats.global_vectors,
            }),
        )?;
        Ok(stats)
    }

    pub fn export_bundle(&self, scope: MemoryScope) -> Result<MemoryExportBundle, String> {
        Ok(MemoryExportBundle {
            bundle_version: 1,
            project: if scope.includes_project() {
                Some(self.load_project_graph()?)
            } else {
                None
            },
            session: if scope.includes_session() && self.session_id.is_some() {
                Some(self.load_session_graph()?)
            } else {
                None
            },
            global: if scope.includes_global() {
                Some(self.load_global_graph()?)
            } else {
                None
            },
        })
    }

    pub fn export_to_path(&self, scope: MemoryScope, path: &Path) -> Result<ExportStats, String> {
        let bundle = self.export_bundle(scope)?;
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create export dir `{}`: {e}", parent.display()))?;
        }
        storage::write_json(path, &bundle)?;
        let stats = ExportStats {
            project_memories: bundle.project.as_ref().map(|g| g.memory_count()).unwrap_or(0),
            session_memories: bundle.session.as_ref().map(|g| g.memory_count()).unwrap_or(0),
            global_memories: bundle.global.as_ref().map(|g| g.memory_count()).unwrap_or(0),
        };
        self.append_audit_event(
            "export",
            Some(scope),
            None,
            json!({
                "path": path.display().to_string(),
                "project_memories": stats.project_memories,
                "session_memories": stats.session_memories,
                "global_memories": stats.global_memories,
            }),
        )?;
        Ok(stats)
    }

    pub fn import_from_path(&self, path: &Path, merge: bool) -> Result<ImportStats, String> {
        let bundle = storage::read_json::<MemoryExportBundle>(path)?;
        self.import_bundle(bundle, merge)
    }

    pub fn import_bundle(&self, bundle: MemoryExportBundle, merge: bool) -> Result<ImportStats, String> {
        let mut stats = ImportStats::default();
        if let Some(project) = bundle.project {
            let mut graph = if merge { self.load_project_graph()? } else { MemoryGraph::new() };
            if merge {
                merge_graph(&mut graph, project);
            } else {
                graph = project;
            }
            normalize_graph_after_import(&mut graph);
            self.refresh_graph_embedding_metadata(&mut graph);
            self.apply_governance_policies(&mut graph);
            stats.project_memories = graph.memory_count();
            self.save_project_graph(&graph)?;
        }
        if let Some(session) = bundle.session
            && self.session_id.is_some()
        {
            let mut graph = if merge { self.load_session_graph()? } else { MemoryGraph::new() };
            if merge {
                merge_graph(&mut graph, session);
            } else {
                graph = session;
            }
            normalize_graph_after_import(&mut graph);
            self.refresh_graph_embedding_metadata(&mut graph);
            self.apply_governance_policies(&mut graph);
            stats.session_memories = graph.memory_count();
            self.save_session_graph(&graph)?;
        }
        if let Some(global) = bundle.global {
            let mut graph = if merge { self.load_global_graph()? } else { MemoryGraph::new() };
            if merge {
                merge_graph(&mut graph, global);
            } else {
                graph = global;
            }
            normalize_graph_after_import(&mut graph);
            self.refresh_graph_embedding_metadata(&mut graph);
            self.apply_governance_policies(&mut graph);
            stats.global_memories = graph.memory_count();
            self.save_global_graph(&graph)?;
        }
        self.append_audit_event(
            "import",
            Some(MemoryScope::All),
            None,
            json!({
                "merge": merge,
                "project_memories": stats.project_memories,
                "session_memories": stats.session_memories,
                "global_memories": stats.global_memories,
            }),
        )?;
        Ok(stats)
    }

    pub async fn ingest_transcript(
        &self,
        transcript: &str,
        extractor: &dyn MemoryExtractor,
        relevance_checker: Option<&dyn MemoryRelevanceChecker>,
    ) -> Result<IngestionReport, String> {
        if transcript.trim().is_empty() {
            return Ok(IngestionReport::default());
        }

        let existing_scope = MemoryScope::All;
        let write_scope = auto_extract_scope_to_memory_scope(self.cfg.auto_extract_scope);

        let existing = self.list(existing_scope)?;
        let existing_texts: Vec<String> = existing
            .iter()
            .filter(|entry| entry.active)
            .map(|entry| entry.content.clone())
            .collect();

        let extracted = extractor.extract(transcript, &existing_texts).await?;
        let mut report = IngestionReport {
            extracted_count: extracted.len() as u32,
            ..Default::default()
        };

        for extracted in extracted
            .into_iter()
            .take(self.cfg.auto_extract_max_items_per_turn)
        {
            let mut candidate = extracted_memory_to_entry(extracted);
            candidate.source = Some("auto_extract".to_string());
            candidate = self.prepare_entry_for_storage(candidate);

            if self.cfg.verify_relevance {
                if let Some(checker) = relevance_checker {
                    let (relevant, _) = checker
                        .check_relevance(&candidate.content, transcript)
                        .await?;
                    if !relevant {
                        report.skipped_irrelevant += 1;
                        continue;
                    }
                }
            }

            if let Some((dup_scope, dup_id)) = self.find_duplicate_for_ingestion(&candidate, existing_scope)? {
                self.reinforce_memory(dup_scope, &dup_id)?;
                report.reinforced_ids.push(dup_id);
                report.skipped_duplicates += 1;
                continue;
            }

            let contradiction = if let Some(checker) = relevance_checker {
                self.find_contradiction_for_ingestion(&candidate, write_scope, checker)
                    .await?
            } else {
                None
            };

            let new_id = self.remember(candidate, write_scope)?;
            report.created_ids.push(new_id.clone());

            if let Some(existing_id) = contradiction {
                self.apply_contradiction_policy(write_scope, &new_id, &existing_id)?;
                report.contradiction_ids.push(existing_id);
            }
        }

        Ok(report)
    }

    // ── Helpers ──

    fn collect_memories(&self, scope: MemoryScope) -> Result<Vec<MemoryEntry>, String> {
        let mut all = Vec::new();
        if scope.includes_project() {
            if let Ok(graph) = self.load_project_graph() {
                all.extend(graph.all_memories().cloned());
            }
        }
        if scope.includes_session() {
            if let Ok(graph) = self.load_session_graph() {
                all.extend(graph.all_memories().cloned());
            }
        }
        if scope.includes_global() {
            if let Ok(graph) = self.load_global_graph() {
                all.extend(graph.all_memories().cloned());
            }
        }
        Ok(all)
    }

    fn prepare_entry_for_storage(&self, mut entry: MemoryEntry) -> MemoryEntry {
        entry.refresh_search_text();
        if let Some(provider) = &self.embedding_provider {
            match provider.embed_text(&entry.content) {
                Ok(embedding) => {
                    entry.set_embedding_metadata(
                        embedding,
                        provider.model_name().to_string(),
                        provider.version().to_string(),
                    );
                }
                Err(err) => {
                    warn!(error = %err, "Failed to generate memory embedding; storing keyword-only memory");
                }
            }
        }
        entry
    }

    fn find_duplicate_for_ingestion(
        &self,
        candidate: &MemoryEntry,
        scope: MemoryScope,
    ) -> Result<Option<(MemoryScope, String)>, String> {
        if scope.includes_project() {
            let graph = self.load_project_graph()?;
            if let Some(id) = find_duplicate_in_graph(&graph, candidate, self.cfg.dedupe_similarity_threshold) {
                return Ok(Some((MemoryScope::Project, id)));
            }
        }
        if scope.includes_global() {
            let graph = self.load_global_graph()?;
            if let Some(id) = find_duplicate_in_graph(&graph, candidate, self.cfg.dedupe_similarity_threshold) {
                return Ok(Some((MemoryScope::Global, id)));
            }
        }
        Ok(None)
    }

    async fn find_contradiction_for_ingestion(
        &self,
        candidate: &MemoryEntry,
        scope: MemoryScope,
        checker: &dyn MemoryRelevanceChecker,
    ) -> Result<Option<String>, String> {
        let candidates = self.list(scope)?;
        for existing in candidates {
            if !existing.active || existing.category != candidate.category {
                continue;
            }
            if !has_text_overlap(&existing.content, &candidate.content)
                && !semantic_duplicate_like(&existing, candidate, self.cfg.dedupe_similarity_threshold - 0.08)
            {
                continue;
            }
            if checker
                .check_contradiction(&candidate.content, &existing.content)
                .await?
            {
                return Ok(Some(existing.id));
            }
        }
        Ok(None)
    }

    fn reinforce_memory(&self, scope: MemoryScope, id: &str) -> Result<(), String> {
        match scope {
            MemoryScope::Session => {
                let mut graph = self.load_session_graph()?;
                if let Some(entry) = graph.get_memory_mut(id) {
                    entry.reinforce("auto_extract", 0);
                    entry.boost_confidence(0.05);
                    let strength = entry.strength;
                    self.save_session_graph(&graph)?;
                    // Auto-promote frequently-reinforced session memories to a
                    // longer-lived scope so valuable knowledge survives the session.
                    if self.cfg.auto_promote_enabled
                        && strength >= self.cfg.auto_promote_strength_threshold
                    {
                        let target = auto_extract_scope_to_memory_scope(self.cfg.auto_promote_target);
                        if !matches!(target, MemoryScope::Session) {
                            if let Err(e) = self.promote_memory(id, MemoryScope::Session, target) {
                                tracing::warn!(error = %e, memory_id = %id, "auto-promote failed");
                            } else {
                                tracing::info!(
                                    memory_id = %id,
                                    strength,
                                    target = %scope_name(target),
                                    "auto-promoted session memory"
                                );
                            }
                        }
                    }
                    return Ok(());
                }
            }
            MemoryScope::Project | MemoryScope::All => {
                let mut graph = self.load_project_graph()?;
                if let Some(entry) = graph.get_memory_mut(id) {
                    entry.reinforce("auto_extract", 0);
                    entry.boost_confidence(0.05);
                    return self.save_project_graph(&graph);
                }
            }
            MemoryScope::Global => {}
        }
        let mut graph = self.load_global_graph()?;
        if let Some(entry) = graph.get_memory_mut(id) {
            entry.reinforce("auto_extract", 0);
            entry.boost_confidence(0.05);
            return self.save_global_graph(&graph);
        }
        Ok(())
    }

    fn apply_contradiction_policy(
        &self,
        scope: MemoryScope,
        new_id: &str,
        existing_id: &str,
    ) -> Result<(), String> {
        match self.cfg.contradiction_policy {
            ContradictionPolicy::Ignore => Ok(()),
            ContradictionPolicy::Supersede => {
                let mut graph = self.load_write_scope_graph(scope)?;
                graph.supersede(new_id, existing_id);
                self.save_write_scope_graph(scope, &graph)
            }
            ContradictionPolicy::DowngradeConfidence => {
                let mut graph = self.load_write_scope_graph(scope)?;
                if let Some(entry) = graph.get_memory_mut(existing_id) {
                    entry.decay_confidence(self.cfg.contradiction_confidence_decay);
                }
                self.save_write_scope_graph(scope, &graph)
            }
            ContradictionPolicy::MarkContradictionEdge => {
                let mut graph = self.load_write_scope_graph(scope)?;
                graph.mark_contradiction(new_id, existing_id);
                self.save_write_scope_graph(scope, &graph)
            }
        }
    }

    fn load_write_scope_graph(&self, scope: MemoryScope) -> Result<MemoryGraph, String> {
        match scope {
            MemoryScope::Session => self.load_session_graph(),
            MemoryScope::Project | MemoryScope::All => self.load_project_graph(),
            MemoryScope::Global => self.load_global_graph(),
        }
    }

    fn save_write_scope_graph(&self, scope: MemoryScope, graph: &MemoryGraph) -> Result<(), String> {
        match scope {
            MemoryScope::Session => self.save_session_graph(graph),
            MemoryScope::Project | MemoryScope::All => self.save_project_graph(graph),
            MemoryScope::Global => self.save_global_graph(graph),
        }
    }

    fn recall_semantic(&self, query: &str, limit: usize, scope: MemoryScope) -> Result<Vec<RecallHit>, String> {
        let provider = match &self.embedding_provider {
            Some(provider) => provider,
            None => return self.recall_keyword(query, limit, scope),
        };
        self.ensure_scope_embeddings_current(scope)?;
        let query_embedding = match provider.embed_text(query) {
            Ok(embedding) => embedding,
            Err(err) => {
                warn!(error = %err, "Semantic recall failed to embed query; falling back to keyword recall");
                return self.recall_keyword(query, limit, scope);
            }
        };
        if self.cfg.ann_enabled {
            if let Ok(ranked) = self.recall_semantic_with_ann(&query_embedding, provider.model_name(), provider.version(), limit, scope) {
                if !ranked.is_empty() {
                    return Ok(ranked);
                }
            }
        }
        let all = self.collect_memories(scope)?;
        let scored = all.into_iter()
            .filter(|entry| entry.active)
            .filter_map(|entry| {
                let embedding = entry.embedding.as_ref()?;
                let cosine = cosine_similarity(&query_embedding, embedding)?;
                if cosine <= 0.0 {
                    return None;
                }
                let recency = normalize_memory_score(&entry);
                let trust = normalize_trust_score(&entry);
                let score = (cosine * 0.7) + (recency * 0.15) + (trust * 0.15);
                Some(RecallHit {
                    entry,
                    score,
                    score_breakdown: ScoreBreakdown {
                        semantic_score: Some(cosine),
                        recency_score: recency,
                        trust_score: trust,
                        final_score: score,
                        ..Default::default()
                    },
                    retrieval_source: RetrievalSource::Semantic,
                })
            });
        let ranked = top_k_hits(scored.collect(), limit);
        if ranked.is_empty() {
            return self.recall_keyword(query, limit, scope);
        }
        Ok(ranked)
    }

    fn recall_semantic_with_ann(
        &self,
        query_embedding: &[f32],
        embedding_model: &str,
        embedding_version: &str,
        limit: usize,
        scope: MemoryScope,
    ) -> Result<Vec<RecallHit>, String> {
        let ann_k = limit.saturating_mul(self.cfg.ann_candidate_multiplier).max(limit).max(8);
        let mut candidates: Vec<MemoryEntry> = Vec::new();

        if scope.includes_project() {
            let graph_path = self.project_memory_path()?;
            let graph = self.load_graph(&graph_path)?;
            let hits = ann::ann_search_candidates(
                &self.cfg,
                &graph_path,
                &graph,
                query_embedding,
                ann_k,
                Some(embedding_model),
                Some(embedding_version),
            )?;
            for hit in hits {
                if let Some(entry) = graph.get_memory(&hit.memory_id) {
                    candidates.push(entry.clone());
                }
            }
        }

        if scope.includes_session() && self.session_id.is_some() {
            let graph_path = self.session_memory_path()?;
            let graph = self.load_graph(&graph_path)?;
            let hits = ann::ann_search_candidates(
                &self.cfg,
                &graph_path,
                &graph,
                query_embedding,
                ann_k,
                Some(embedding_model),
                Some(embedding_version),
            )?;
            for hit in hits {
                if let Some(entry) = graph.get_memory(&hit.memory_id) {
                    candidates.push(entry.clone());
                }
            }
        }

        if scope.includes_global() {
            let graph_path = self.global_memory_path();
            let graph = self.load_graph(&graph_path)?;
            let hits = ann::ann_search_candidates(
                &self.cfg,
                &graph_path,
                &graph,
                query_embedding,
                ann_k,
                Some(embedding_model),
                Some(embedding_version),
            )?;
            for hit in hits {
                if let Some(entry) = graph.get_memory(&hit.memory_id) {
                    candidates.push(entry.clone());
                }
            }
        }

        if candidates.is_empty() {
            return Ok(Vec::new());
        }

        let mut scored = Vec::with_capacity(candidates.len());
        for entry in candidates {
            if !entry.active {
                continue;
            }
            let Some(embedding) = entry.embedding.as_ref() else {
                continue;
            };
            let Some(cosine) = cosine_similarity(query_embedding, embedding) else {
                continue;
            };
            if cosine <= 0.0 {
                continue;
            }
            let recency = normalize_memory_score(&entry);
            let trust = normalize_trust_score(&entry);
            let score = (cosine * 0.7) + (recency * 0.15) + (trust * 0.15);
            scored.push(RecallHit {
                entry,
                score,
                score_breakdown: ScoreBreakdown {
                    semantic_score: Some(cosine),
                    recency_score: recency,
                    trust_score: trust,
                    final_score: score,
                    ..Default::default()
                },
                retrieval_source: RetrievalSource::SemanticAnn,
            });
        }
        Ok(top_k_hits(scored, limit))
    }

    fn reembed_graph(
        &self,
        graph: &mut MemoryGraph,
        provider: &dyn EmbeddingProvider,
    ) -> Result<usize, String> {
        let mut updated = 0usize;
        let mut ids: Vec<String> = graph.memories.keys().cloned().collect();
        ids.sort();
        for id in ids {
            let Some(entry) = graph.get_memory_mut(&id) else {
                continue;
            };
            match provider.embed_text(&entry.content) {
                Ok(embedding) => {
                    entry.set_embedding_metadata(
                        embedding,
                        provider.model_name().to_string(),
                        provider.version().to_string(),
                    );
                    updated += 1;
                }
                Err(err) => {
                    warn!(memory_id = %id, error = %err, "Failed to re-embed memory");
                }
            }
        }
        graph.metadata.last_embedding_rebuild_at = Some(Utc::now());
        self.refresh_graph_embedding_metadata(graph);
        Ok(updated)
    }

    fn reindex_graph(&self, graph: &mut MemoryGraph) -> usize {
        let mut updated = 0usize;
        let mut ids: Vec<String> = graph.memories.keys().cloned().collect();
        ids.sort();
        for id in ids {
            let Some(entry) = graph.get_memory_mut(&id) else {
                continue;
            };
            entry.refresh_search_text();
            updated += 1;
        }
        self.refresh_graph_embedding_metadata(graph);
        updated
    }

    fn refresh_graph_embedding_metadata(&self, graph: &mut MemoryGraph) {
        let total_embeddings = graph
            .memories
            .values()
            .filter(|entry| entry.embedding.is_some())
            .count() as u64;
        graph.metadata.total_embeddings = total_embeddings;
        if let Some(provider) = &self.embedding_provider {
            graph.metadata.embedding_model = Some(provider.model_name().to_string());
            graph.metadata.embedding_version = Some(provider.version().to_string());
        }
    }

    fn set_memory_active(&self, id: &str, active: bool) -> Result<bool, String> {
        let mut project = self.load_project_graph()?;
        if let Some(entry) = project.get_memory_mut(id) {
            entry.active = active;
            entry.updated_at = Utc::now();
            self.save_project_graph(&project)?;
            self.append_audit_event(
                if active { "enable" } else { "disable" },
                Some(MemoryScope::Project),
                Some(id),
                json!({}),
            )?;
            return Ok(true);
        }

        let mut global = self.load_global_graph()?;
        if let Some(entry) = global.get_memory_mut(id) {
            entry.active = active;
            entry.updated_at = Utc::now();
            self.save_global_graph(&global)?;
            self.append_audit_event(
                if active { "enable" } else { "disable" },
                Some(MemoryScope::Global),
                Some(id),
                json!({}),
            )?;
            return Ok(true);
        }
        Ok(false)
    }

    fn ensure_scope_embeddings_current(&self, scope: MemoryScope) -> Result<(), String> {
        let Some(provider) = &self.embedding_provider else {
            return Ok(());
        };
        if !self.cfg.rebuild_on_model_change {
            return Ok(());
        }
        if scope.includes_project() {
            let mut graph = self.load_project_graph()?;
            let rebuilt = self.maybe_rebuild_graph_for_model_change(&mut graph, provider.as_ref())?;
            if rebuilt > 0 {
                self.save_project_graph(&graph)?;
                self.append_audit_event(
                    "reembed_on_model_change",
                    Some(MemoryScope::Project),
                    None,
                    json!({ "updated": rebuilt }),
                )?;
            }
        }
        if scope.includes_session() && self.session_id.is_some() {
            let mut graph = self.load_session_graph()?;
            let rebuilt = self.maybe_rebuild_graph_for_model_change(&mut graph, provider.as_ref())?;
            if rebuilt > 0 {
                self.save_session_graph(&graph)?;
                self.append_audit_event(
                    "reembed_on_model_change",
                    Some(MemoryScope::Session),
                    None,
                    json!({ "updated": rebuilt }),
                )?;
            }
        }
        if scope.includes_global() {
            let mut graph = self.load_global_graph()?;
            let rebuilt = self.maybe_rebuild_graph_for_model_change(&mut graph, provider.as_ref())?;
            if rebuilt > 0 {
                self.save_global_graph(&graph)?;
                self.append_audit_event(
                    "reembed_on_model_change",
                    Some(MemoryScope::Global),
                    None,
                    json!({ "updated": rebuilt }),
                )?;
            }
        }
        Ok(())
    }

    fn maybe_rebuild_graph_for_model_change(
        &self,
        graph: &mut MemoryGraph,
        provider: &dyn EmbeddingProvider,
    ) -> Result<usize, String> {
        if !self.cfg.rebuild_on_model_change {
            return Ok(0);
        }
        if graph.metadata.total_embeddings == 0 {
            return Ok(0);
        }
        let model_changed = graph.metadata.embedding_model.as_deref() != Some(provider.model_name())
            || graph.metadata.embedding_version.as_deref() != Some(provider.version());
        if !model_changed {
            return Ok(0);
        }
        self.reembed_graph(graph, provider)
    }

    fn apply_governance_policies(&self, graph: &mut MemoryGraph) -> usize {
        let removed_by_retention = self.apply_retention_policy(graph);
        let removed_by_size = self.apply_size_limit(graph);
        if removed_by_retention > 0 || removed_by_size > 0 {
            self.refresh_graph_embedding_metadata(graph);
        }
        removed_by_retention + removed_by_size
    }

    fn apply_retention_policy(&self, graph: &mut MemoryGraph) -> usize {
        let Some(retention_days) = self.cfg.retention_days else {
            return 0;
        };
        let cutoff = Utc::now() - chrono::Duration::days(retention_days as i64);
        let stale_ids: Vec<String> = graph
            .memories
            .values()
            .filter(|entry| entry.updated_at < cutoff)
            .map(|entry| entry.id.clone())
            .collect();
        let removed = stale_ids.len();
        for id in stale_ids {
            graph.remove_memory(&id);
        }
        removed
    }

    fn apply_size_limit(&self, graph: &mut MemoryGraph) -> usize {
        let Some(limit) = self.cfg.memory_size_limit else {
            return 0;
        };
        if graph.memory_count() <= limit {
            return 0;
        }
        let mut entries: Vec<(String, f64, chrono::DateTime<chrono::Utc>)> = graph
            .memories
            .values()
            .map(|entry| (entry.id.clone(), memory_score(entry), entry.updated_at))
            .collect();
        entries.sort_by(|a, b| {
            a.1.partial_cmp(&b.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.2.cmp(&b.2))
        });
        let remove_count = graph.memory_count().saturating_sub(limit);
        for (id, _, _) in entries.into_iter().take(remove_count) {
            graph.remove_memory(&id);
        }
        remove_count
    }

    fn audit_log_path(&self) -> PathBuf {
        self.storage_dir.join("memory.audit.jsonl")
    }

    fn append_audit_event(
        &self,
        action: &str,
        scope: Option<MemoryScope>,
        memory_id: Option<&str>,
        details: serde_json::Value,
    ) -> Result<(), String> {
        let path = self.audit_log_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create audit dir `{}`: {e}", parent.display()))?;
        }
        let event = MemoryAuditEvent {
            timestamp: Utc::now(),
            action: action.to_string(),
            scope: scope.map(scope_name),
            memory_id: memory_id.map(|id| id.to_string()),
            details,
        };
        let line = serde_json::to_string(&event)
            .map_err(|e| format!("failed to encode audit event: {e}"))?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| format!("failed to open audit log `{}`: {e}", path.display()))?;
        use std::io::Write;
        writeln!(file, "{line}")
            .map_err(|e| format!("failed to append audit event to `{}`: {e}", path.display()))?;
        Ok(())
    }
}

fn normalize_memory_score(entry: &MemoryEntry) -> f32 {
    let raw = memory_score(entry);
    (raw / (raw + 100.0)) as f32
}

fn scope_name(scope: MemoryScope) -> String {
    match scope {
        MemoryScope::Session => "session".to_string(),
        MemoryScope::Project => "project".to_string(),
        MemoryScope::Global => "global".to_string(),
        MemoryScope::All => "all".to_string(),
    }
}

/// Map an `AutoExtractScope` config value to the corresponding `MemoryScope`.
fn auto_extract_scope_to_memory_scope(scope: AutoExtractScope) -> MemoryScope {
    match scope {
        AutoExtractScope::Session => MemoryScope::Session,
        AutoExtractScope::Project => MemoryScope::Project,
        AutoExtractScope::Global => MemoryScope::Global,
    }
}

fn normalize_trust_score(entry: &MemoryEntry) -> f32 {
    match entry.trust {
        TrustLevel::High => 1.0,
        TrustLevel::Medium => 0.75,
        TrustLevel::Low => 0.5,
    }
}

fn keyword_match_score(entry: &MemoryEntry, normalized_query: &str) -> f32 {
    let query_terms: Vec<&str> = normalized_query
        .split_whitespace()
        .filter(|term| !term.is_empty())
        .collect();
    if query_terms.is_empty() {
        return 0.0;
    }
    let haystack = if entry.search_text.is_empty() {
        normalize_memory_search_text(&entry.content, &entry.tags)
    } else {
        entry.search_text.clone()
    };
    let matched = query_terms
        .iter()
        .filter(|term| haystack.contains(**term))
        .count();
    matched as f32 / query_terms.len() as f32
}

fn top_k_hits(mut hits: Vec<RecallHit>, limit: usize) -> Vec<RecallHit> {
    hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    hits.truncate(limit);
    hits
}

fn merge_hit(merged: &mut HashMap<String, RecallHit>, hit: RecallHit) {
    match merged.get(&hit.entry.id) {
        Some(existing) if existing.score >= hit.score => {}
        _ => {
            merged.insert(hit.entry.id.clone(), hit);
        }
    }
}

fn apply_cascade_results(
    merged: &mut HashMap<String, RecallHit>,
    entry_map: &HashMap<String, MemoryEntry>,
    cascaded: Vec<(String, f32)>,
) {
    for (id, graph_score) in cascaded {
        let Some(entry) = entry_map.get(&id).cloned() else {
            continue;
        };
        let recency = normalize_memory_score(&entry);
        let trust = normalize_trust_score(&entry);
        let semantic_score = merged
            .get(&id)
            .and_then(|hit| hit.score_breakdown.semantic_score);
        let keyword_score = merged
            .get(&id)
            .and_then(|hit| hit.score_breakdown.keyword_score);
        let base_score = merged.get(&id).map(|hit| hit.score).unwrap_or(0.0);
        let final_score = base_score.max(graph_score * 0.75) * 0.75
            + (graph_score * 0.15)
            + (recency * 0.05)
            + (trust * 0.05);
        let hit = RecallHit {
            entry,
            score: final_score,
            score_breakdown: ScoreBreakdown {
                semantic_score,
                keyword_score,
                recency_score: recency,
                graph_score: Some(graph_score),
                trust_score: trust,
                final_score,
            },
            retrieval_source: if base_score > 0.0 {
                RetrievalSource::CascadeSeed
            } else {
                RetrievalSource::CascadeGraph
            },
        };
        merge_hit(merged, hit);
    }
}

fn merge_graph(target: &mut MemoryGraph, incoming: MemoryGraph) {
    for (id, memory) in incoming.memories {
        target.memories.insert(id, memory);
    }
    for (id, tag) in incoming.tags {
        target.tags.insert(id, tag);
    }
    for (id, cluster) in incoming.clusters {
        target.clusters.insert(id, cluster);
    }
    for (source, edges) in incoming.edges {
        let existing = target.edges.entry(source).or_default();
        for edge in edges {
            if !existing.iter().any(|current| current.target == edge.target && current.kind == edge.kind) {
                existing.push(edge);
            }
        }
    }
    if incoming.metadata.last_cluster_update > target.metadata.last_cluster_update {
        target.metadata.last_cluster_update = incoming.metadata.last_cluster_update;
    }
    if incoming.metadata.last_embedding_rebuild_at > target.metadata.last_embedding_rebuild_at {
        target.metadata.last_embedding_rebuild_at = incoming.metadata.last_embedding_rebuild_at;
    }
    if target.metadata.embedding_model.is_none() {
        target.metadata.embedding_model = incoming.metadata.embedding_model;
    }
    if target.metadata.embedding_version.is_none() {
        target.metadata.embedding_version = incoming.metadata.embedding_version;
    }
}

fn normalize_graph_after_import(graph: &mut MemoryGraph) {
    graph.graph_version = graph::GRAPH_VERSION;
    graph.reverse_edges.clear();
    for memory in graph.memories.values_mut() {
        memory.refresh_search_text();
    }

    let edge_snapshot: Vec<(String, Vec<graph::Edge>)> = graph
        .edges
        .iter()
        .map(|(source, edges)| (source.clone(), edges.clone()))
        .collect();
    for (source, edges) in edge_snapshot {
        for edge in edges {
            graph.reverse_edges.entry(edge.target.clone()).or_default().push(source.clone());
        }
    }

    for tag in graph.tags.values_mut() {
        tag.count = 0;
    }
    for (source, edges) in &graph.edges {
        if !graph.memories.contains_key(source) {
            continue;
        }
        for edge in edges {
            if matches!(edge.kind, graph::EdgeKind::HasTag)
                && let Some(tag) = graph.tags.get_mut(&edge.target)
            {
                tag.count += 1;
            }
        }
    }
}

fn extracted_memory_to_entry(extracted: ExtractedMemory) -> MemoryEntry {
    let mut entry = MemoryEntry::new(
        MemoryCategory::from_extracted(&extracted.category),
        extracted.content,
    );
    entry.trust = parse_trust_level(&extracted.trust);
    entry.confidence = match entry.trust {
        TrustLevel::High => 0.95,
        TrustLevel::Medium => 0.75,
        TrustLevel::Low => 0.55,
    };
    entry
}

fn parse_trust_level(trust: &str) -> TrustLevel {
    match trust.trim().to_ascii_lowercase().as_str() {
        "high" => TrustLevel::High,
        "low" => TrustLevel::Low,
        _ => TrustLevel::Medium,
    }
}

fn find_duplicate_in_graph(
    graph: &MemoryGraph,
    candidate: &MemoryEntry,
    threshold: f32,
) -> Option<String> {
    graph
        .all_memories()
        .filter(|entry| entry.active && entry.category == candidate.category)
        .find(|entry| duplicate_match(entry, candidate, threshold))
        .map(|entry| entry.id.clone())
}

fn duplicate_match(existing: &MemoryEntry, candidate: &MemoryEntry, threshold: f32) -> bool {
    let existing_text = existing.searchable_text();
    let candidate_text = candidate.searchable_text();
    if existing_text == candidate_text {
        return true;
    }
    semantic_duplicate_like(existing, candidate, threshold)
}

fn semantic_duplicate_like(existing: &MemoryEntry, candidate: &MemoryEntry, threshold: f32) -> bool {
    match (existing.embedding.as_ref(), candidate.embedding.as_ref()) {
        (Some(lhs), Some(rhs)) => cosine_similarity(lhs, rhs)
            .map(|score| score >= threshold)
            .unwrap_or(false),
        _ => has_text_overlap(&existing.content, &candidate.content),
    }
}

fn has_text_overlap(lhs: &str, rhs: &str) -> bool {
    let lhs_terms: std::collections::HashSet<String> = normalize_search_text(lhs)
        .split_whitespace()
        .filter(|term| term.len() > 2)
        .map(|term| term.to_string())
        .collect();
    let rhs_terms: std::collections::HashSet<String> = normalize_search_text(rhs)
        .split_whitespace()
        .filter(|term| term.len() > 2)
        .map(|term| term.to_string())
        .collect();
    if lhs_terms.is_empty() || rhs_terms.is_empty() {
        return false;
    }
    lhs_terms.intersection(&rhs_terms).next().is_some()
}

fn cosine_similarity(lhs: &[f32], rhs: &[f32]) -> Option<f32> {
    if lhs.len() != rhs.len() || lhs.is_empty() {
        return None;
    }
    let mut dot = 0.0f32;
    let mut lhs_norm = 0.0f32;
    let mut rhs_norm = 0.0f32;
    for (a, b) in lhs.iter().zip(rhs.iter()) {
        dot += a * b;
        lhs_norm += a * a;
        rhs_norm += b * b;
    }
    if lhs_norm <= f32::EPSILON || rhs_norm <= f32::EPSILON {
        return None;
    }
    Some(dot / (lhs_norm.sqrt() * rhs_norm.sqrt()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use super::embedding::FixedEmbeddingProvider;
    use std::sync::Arc;

    struct StaticExtractor {
        items: Vec<ExtractedMemory>,
    }

    #[async_trait]
    impl MemoryExtractor for StaticExtractor {
        async fn extract(&self, _transcript: &str, _existing: &[String]) -> Result<Vec<ExtractedMemory>, String> {
            Ok(self.items.clone())
        }
    }

    struct StaticChecker {
        relevant: bool,
        contradictions: Vec<(String, String)>,
    }

    #[async_trait]
    impl MemoryRelevanceChecker for StaticChecker {
        async fn check_relevance(&self, _memory: &str, _context: &str) -> Result<(bool, String), String> {
            Ok((self.relevant, "static".to_string()))
        }

        async fn check_contradiction(&self, new: &str, existing: &str) -> Result<bool, String> {
            Ok(self
                .contradictions
                .iter()
                .any(|(lhs, rhs)| lhs == new && rhs == existing))
        }
    }

    #[test]
    fn test_remember_and_recall() {
        let mgr = MemoryManager::new_test();
        let entry = MemoryEntry::new(MemoryCategory::Fact, "Rust is fast")
            .with_tags(vec!["rust".to_string(), "language".to_string()]);
        let id = mgr.remember_project(entry).unwrap();

        let results = mgr.recall(Some("Rust"), 10, RecallMode::Keyword, MemoryScope::Project).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].0.id, id);
    }

    #[test]
    fn test_search_and_forget() {
        let mgr = MemoryManager::new_test();
        let entry = MemoryEntry::new(MemoryCategory::Preference, "I like tabs");
        let id = mgr.remember_global(entry).unwrap();

        let found = mgr.search("tabs", MemoryScope::Global).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, id);

        mgr.forget(&id).unwrap();
        assert!(mgr.search("tabs", MemoryScope::Global).unwrap().is_empty());
    }

    #[test]
    fn test_tag_and_link() {
        let mgr = MemoryManager::new_test();
        let e1 = MemoryEntry::new(MemoryCategory::Fact, "memory 1");
        let e2 = MemoryEntry::new(MemoryCategory::Fact, "memory 2");
        let id1 = mgr.remember_project(e1).unwrap();
        let id2 = mgr.remember_project(e2).unwrap();
        mgr.tag_memory(&id1, "important").unwrap();
        mgr.link_memories(&id1, &id2, 0.8).unwrap();

        let related = mgr.get_related(&id1, 2).unwrap();
        assert_eq!(related.len(), 1);
        assert_eq!(related[0].id, id2);
    }

    #[test]
    fn test_graph_stats() {
        let mgr = MemoryManager::new_test();
        for i in 0..5 {
            let entry = MemoryEntry::new(MemoryCategory::Fact, format!("memory {i}"));
            mgr.remember_project(entry).unwrap();
        }
        let (memories, _, _, _) = mgr.graph_stats().unwrap();
        assert_eq!(memories, 5);
    }

    #[test]
    fn test_prompt_formatting() {
        use crate::memory::prompt::format_relevant_prompt;
        let e1 = MemoryEntry::new(MemoryCategory::Fact, "Rust is safe");
        let e2 = MemoryEntry::new(MemoryCategory::Preference, "Use async/await");

        let formatted = format_relevant_prompt(&[e1, e2], 10);
        assert!(formatted.is_some());
        let text = formatted.unwrap();
        assert!(text.contains("# Memory"));
        assert!(text.contains("Rust is safe"));
        assert!(text.contains("Use async/await"));
    }

    #[test]
    fn test_semantic_recall_prefers_embedding_similarity() {
        let provider = FixedEmbeddingProvider::new("test-embed", |inputs| {
            inputs
                .iter()
                .map(|input| {
                    if input.contains("small direct rust") {
                        vec![1.0, 0.0]
                    } else if input.contains("short and concise rust") {
                        vec![0.9, 0.1]
                    } else {
                        vec![0.0, 1.0]
                    }
                })
                .collect()
        });
        let mgr = MemoryManager::new_test().with_embedding_provider(Arc::new(provider));
        let preferred_id = mgr
            .remember_project(MemoryEntry::new(
                MemoryCategory::Preference,
                "Prefer small direct rust answers",
            ))
            .unwrap();
        let _other_id = mgr
            .remember_project(MemoryEntry::new(
                MemoryCategory::Preference,
                "Prefer detailed python walkthroughs",
            ))
            .unwrap();

        let results = mgr
            .recall(
                Some("short and concise rust"),
                5,
                RecallMode::Semantic,
                MemoryScope::Project,
            )
            .unwrap();

        assert!(!results.is_empty());
        assert_eq!(results[0].0.id, preferred_id);
        assert!(results[0].0.embedding.is_some());
        assert_eq!(
            results[0].0.embedding_model.as_deref(),
            Some("test-embed")
        );
    }

    #[test]
    fn test_recall_detailed_exposes_source_and_breakdown() {
        let provider = FixedEmbeddingProvider::new("test-embed", |inputs| {
            inputs
                .iter()
                .map(|input| {
                    if input.contains("rust style") || input.contains("small direct") {
                        vec![1.0, 0.0]
                    } else {
                        vec![0.0, 1.0]
                    }
                })
                .collect()
        });
        let mgr = MemoryManager::new_test().with_embedding_provider(Arc::new(provider));
        mgr.remember_project(MemoryEntry::new(
            MemoryCategory::Preference,
            "Prefer small direct rust style answers",
        ))
        .unwrap();

        let hits = mgr
            .recall_detailed(
                Some("rust style"),
                5,
                RecallMode::Semantic,
                MemoryScope::Project,
            )
            .unwrap();
        assert!(!hits.is_empty());
        assert!(matches!(
            hits[0].retrieval_source,
            RetrievalSource::Semantic | RetrievalSource::SemanticAnn
        ));
        assert!(hits[0].score_breakdown.semantic_score.is_some());
        assert!(hits[0].score_breakdown.final_score > 0.0);
    }

    #[test]
    fn test_cascade_recall_surfaces_graph_hits() {
        let provider = FixedEmbeddingProvider::new("test-embed", |inputs| {
            inputs
                .iter()
                .map(|input| {
                    if input.contains("rust seed") || input.contains("rust query") {
                        vec![1.0, 0.0]
                    } else {
                        vec![0.0, 1.0]
                    }
                })
                .collect()
        });
        let mgr = MemoryManager::new_test().with_embedding_provider(Arc::new(provider));
        let seed_id = mgr
            .remember_project(MemoryEntry::new(MemoryCategory::Fact, "rust seed"))
            .unwrap();
        let linked_id = mgr
            .remember_project(MemoryEntry::new(MemoryCategory::Fact, "related graph memory"))
            .unwrap();
        mgr.link_memories(&seed_id, &linked_id, 1.0).unwrap();

        let hits = mgr
            .recall_detailed(
                Some("rust query"),
                5,
                RecallMode::Cascade,
                MemoryScope::Project,
            )
            .unwrap();
        assert!(hits.iter().any(|hit| hit.entry.id == linked_id));
        let linked = hits.iter().find(|hit| hit.entry.id == linked_id).unwrap();
        assert!(matches!(linked.retrieval_source, RetrievalSource::CascadeGraph));
        assert!(linked.score_breakdown.graph_score.is_some());
    }

    #[test]
    fn test_reembed_populates_missing_embeddings() {
        let provider = FixedEmbeddingProvider::new("test-embed", |_inputs| vec![vec![0.2, 0.8]]);
        let mgr = MemoryManager::new_test().with_embedding_provider(Arc::new(provider));
        let id = mgr
            .remember_project(MemoryEntry::new(MemoryCategory::Fact, "remember this"))
            .unwrap();

        {
            let mut graph = mgr.load_project_graph().unwrap();
            let entry = graph.get_memory_mut(&id).unwrap();
            entry.embedding = None;
            entry.embedding_model = None;
            entry.embedding_version = None;
            mgr.save_project_graph(&graph).unwrap();
        }

        let updated = mgr.reembed(MemoryScope::Project).unwrap();
        assert_eq!(updated, 1);

        let graph = mgr.load_project_graph().unwrap();
        let entry = graph.get_memory(&id).unwrap();
        assert!(entry.embedding.is_some());
        assert_eq!(graph.metadata.total_embeddings, 1);
        assert!(graph.metadata.last_embedding_rebuild_at.is_some());
    }

    #[test]
    fn test_ann_index_is_built_and_persisted_on_semantic_recall() {
        let provider = FixedEmbeddingProvider::new("test-embed", |_inputs| vec![vec![0.1, 0.9, 0.0]]);
        let mgr = MemoryManager::new_test()
            .with_embedding_provider(Arc::new(provider))
            .with_ann_settings(true, 1);

        let _id = mgr
            .remember_project(MemoryEntry::new(MemoryCategory::Fact, "alpha"))
            .unwrap();

        let results = mgr
            .recall(
                Some("alpha"),
                3,
                RecallMode::Semantic,
                MemoryScope::Project,
            )
            .unwrap();
        assert!(!results.is_empty());

        let graph_path = mgr.project_memory_path().unwrap();
        let ann_path = ann::ann_index_path(&graph_path);
        assert!(ann_path.exists());
    }

    #[test]
    fn test_rebuild_ann_creates_and_removes_sidecar() {
        let provider = FixedEmbeddingProvider::new("test-embed", |_inputs| vec![vec![0.1, 0.9, 0.0]]);
        let mgr = MemoryManager::new_test()
            .with_embedding_provider(Arc::new(provider))
            .with_ann_settings(true, 1);

        let id = mgr
            .remember_project(MemoryEntry::new(MemoryCategory::Fact, "alpha"))
            .unwrap();
        let graph_path = mgr.project_memory_path().unwrap();
        let ann_path = ann::ann_index_path(&graph_path);
        assert!(!ann_path.exists());

        let stats = mgr.rebuild_ann(MemoryScope::Project).unwrap();
        assert_eq!(stats.project_vectors, 1);
        assert_eq!(stats.global_vectors, 0);
        assert!(ann_path.exists());

        let mut graph = mgr.load_project_graph().unwrap();
        let entry = graph.get_memory_mut(&id).unwrap();
        entry.embedding = None;
        entry.embedding_model = None;
        entry.embedding_version = None;
        graph.metadata.total_embeddings = 0;
        mgr.save_project_graph(&graph).unwrap();

        let stats = mgr.rebuild_ann(MemoryScope::Project).unwrap();
        assert_eq!(stats.project_vectors, 0);
        assert!(!ann_path.exists());
    }

    #[test]
    fn test_export_and_import_roundtrip_persists_project_and_global_graphs() {
        let project_dir = std::env::temp_dir().join(format!("fox-memory-export-project-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&project_dir).unwrap();
        let mgr = MemoryManager::new_test().with_project_dir(project_dir.clone());
        mgr.remember_project(
            MemoryEntry::new(MemoryCategory::Fact, "Project memory").with_tags(vec!["project".into()]),
        )
        .unwrap();
        mgr.remember_global(MemoryEntry::new(MemoryCategory::Preference, "Global memory"))
            .unwrap();

        let export_path = mgr.storage_dir.join("bundle.json");
        let stats = mgr.export_to_path(MemoryScope::All, &export_path).unwrap();
        assert_eq!(stats.project_memories, 1);
        assert_eq!(stats.global_memories, 1);
        assert!(export_path.exists());

        let bundle = storage::read_json::<MemoryExportBundle>(&export_path).unwrap();
        assert_eq!(bundle.bundle_version, 1);
        assert_eq!(bundle.project.as_ref().map(|g| g.memory_count()), Some(1));
        assert_eq!(bundle.global.as_ref().map(|g| g.memory_count()), Some(1));

        let imported_project_dir =
            std::env::temp_dir().join(format!("fox-memory-import-project-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&imported_project_dir).unwrap();
        let imported = MemoryManager::new_test().with_project_dir(imported_project_dir);
        let import_stats = imported.import_from_path(&export_path, false).unwrap();
        assert_eq!(import_stats.project_memories, 1);
        assert_eq!(import_stats.global_memories, 1);

        let project_memories = imported.list(MemoryScope::Project).unwrap();
        let global_memories = imported.list(MemoryScope::Global).unwrap();
        assert_eq!(project_memories.len(), 1);
        assert_eq!(global_memories.len(), 1);
        assert_eq!(project_memories[0].content, "Project memory");
        assert_eq!(project_memories[0].tags, vec!["project".to_string()]);
        assert_eq!(global_memories[0].content, "Global memory");
    }

    #[test]
    fn test_import_bundle_merge_preserves_existing_memories() {
        let source_project_dir =
            std::env::temp_dir().join(format!("fox-memory-merge-source-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&source_project_dir).unwrap();
        let source = MemoryManager::new_test().with_project_dir(source_project_dir);
        source
            .remember_project(MemoryEntry::new(MemoryCategory::Fact, "Imported project memory"))
            .unwrap();
        source
            .remember_global(MemoryEntry::new(MemoryCategory::Preference, "Imported global memory"))
            .unwrap();
        let bundle = source.export_bundle(MemoryScope::All).unwrap();

        let target_project_dir =
            std::env::temp_dir().join(format!("fox-memory-merge-target-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&target_project_dir).unwrap();
        let target = MemoryManager::new_test().with_project_dir(target_project_dir);
        target
            .remember_project(MemoryEntry::new(MemoryCategory::Fact, "Existing project memory"))
            .unwrap();
        target
            .remember_global(MemoryEntry::new(MemoryCategory::Preference, "Existing global memory"))
            .unwrap();

        let stats = target.import_bundle(bundle, true).unwrap();
        assert_eq!(stats.project_memories, 2);
        assert_eq!(stats.global_memories, 2);

        let project_contents: Vec<String> = target
            .list(MemoryScope::Project)
            .unwrap()
            .into_iter()
            .map(|entry| entry.content)
            .collect();
        let global_contents: Vec<String> = target
            .list(MemoryScope::Global)
            .unwrap()
            .into_iter()
            .map(|entry| entry.content)
            .collect();
        assert!(project_contents.iter().any(|content| content == "Existing project memory"));
        assert!(project_contents.iter().any(|content| content == "Imported project memory"));
        assert!(global_contents.iter().any(|content| content == "Existing global memory"));
        assert!(global_contents.iter().any(|content| content == "Imported global memory"));
    }

    #[test]
    fn test_import_bundle_replace_overwrites_existing_scope() {
        let source_project_dir =
            std::env::temp_dir().join(format!("fox-memory-replace-source-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&source_project_dir).unwrap();
        let source = MemoryManager::new_test().with_project_dir(source_project_dir);
        source
            .remember_project(MemoryEntry::new(MemoryCategory::Fact, "Imported only"))
            .unwrap();
        let bundle = source.export_bundle(MemoryScope::Project).unwrap();

        let target_project_dir =
            std::env::temp_dir().join(format!("fox-memory-replace-target-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&target_project_dir).unwrap();
        let target = MemoryManager::new_test().with_project_dir(target_project_dir);
        target
            .remember_project(MemoryEntry::new(MemoryCategory::Fact, "Existing only"))
            .unwrap();

        let stats = target.import_bundle(bundle, false).unwrap();
        assert_eq!(stats.project_memories, 1);

        let project_memories = target.list(MemoryScope::Project).unwrap();
        assert_eq!(project_memories.len(), 1);
        assert_eq!(project_memories[0].content, "Imported only");
    }

    #[test]
    fn test_disable_enable_and_redact_memory_updates_recall_and_audit_log() {
        let provider = FixedEmbeddingProvider::new("test-embed", |_inputs| vec![vec![0.4, 0.6]]);
        let mgr = MemoryManager::new_test().with_embedding_provider(Arc::new(provider));
        let id = mgr
            .remember_project(MemoryEntry::new(MemoryCategory::Preference, "Keep answers concise"))
            .unwrap();

        assert!(mgr.disable_memory(&id).unwrap());
        let disabled = mgr
            .recall(Some("concise"), 5, RecallMode::Keyword, MemoryScope::Project)
            .unwrap();
        assert!(disabled.is_empty());

        assert!(mgr.enable_memory(&id).unwrap());
        let enabled = mgr
            .recall(Some("concise"), 5, RecallMode::Keyword, MemoryScope::Project)
            .unwrap();
        assert_eq!(enabled.len(), 1);

        assert!(mgr.redact_memory(&id, "[redacted]").unwrap());
        let graph = mgr.load_project_graph().unwrap();
        let entry = graph.get_memory(&id).unwrap();
        assert_eq!(entry.content, "[redacted]");
        assert!(entry.embedding.is_some());

        let audit_path = mgr.audit_log_path();
        let audit = std::fs::read_to_string(audit_path).unwrap();
        assert!(audit.contains("\"action\":\"disable\""));
        assert!(audit.contains("\"action\":\"enable\""));
        assert!(audit.contains("\"action\":\"redact\""));
    }

    #[test]
    fn test_refresh_clusters_groups_similar_memories() {
        let provider = FixedEmbeddingProvider::new("test-embed", |inputs| {
            inputs
                .iter()
                .map(|input| {
                    if input.contains("rust") {
                        vec![1.0, 0.0]
                    } else {
                        vec![0.0, 1.0]
                    }
                })
                .collect()
        });
        let mgr = MemoryManager::new_test().with_embedding_provider(Arc::new(provider));
        let rust_a = mgr
            .remember_project(MemoryEntry::new(MemoryCategory::Fact, "rust memory one"))
            .unwrap();
        let rust_b = mgr
            .remember_project(MemoryEntry::new(MemoryCategory::Fact, "rust memory two"))
            .unwrap();
        let _python = mgr
            .remember_project(MemoryEntry::new(MemoryCategory::Fact, "python memory"))
            .unwrap();

        let stats = mgr.refresh_clusters(MemoryScope::Project).unwrap();
        assert_eq!(stats.project_clusters, 1);

        let graph = mgr.load_project_graph().unwrap();
        assert_eq!(graph.cluster_count(), 1);
        let cluster_id = graph.clusters.keys().next().unwrap().clone();
        assert!(graph
            .get_edges(&rust_a)
            .iter()
            .any(|edge| edge.target == cluster_id && matches!(edge.kind, graph::EdgeKind::InCluster)));
        assert!(graph
            .get_edges(&rust_b)
            .iter()
            .any(|edge| edge.target == cluster_id && matches!(edge.kind, graph::EdgeKind::InCluster)));
        assert!(graph.metadata.last_cluster_update.is_some());
    }

    #[test]
    fn test_compact_applies_retention_and_size_limit() {
        let temp_storage =
            std::env::temp_dir().join(format!("fox-memory-compact-{}", uuid::Uuid::new_v4()));
        let project_dir =
            std::env::temp_dir().join(format!("fox-memory-compact-project-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_storage).unwrap();
        std::fs::create_dir_all(&project_dir).unwrap();

        let mut cfg = MemoryConfig::default();
        cfg.retention_days = Some(1);
        let mut mgr = MemoryManager::new(&cfg)
            .with_storage_dir(temp_storage)
            .with_project_dir(project_dir);

        let stale_id = mgr
            .remember_project(MemoryEntry::new(MemoryCategory::Fact, "stale memory"))
            .unwrap();
        let _recent_a = mgr
            .remember_project(MemoryEntry::new(MemoryCategory::Fact, "recent memory a"))
            .unwrap();
        let recent_b = mgr
            .remember_project(MemoryEntry::new(MemoryCategory::Fact, "recent memory b"))
            .unwrap();

        {
            let mut graph = mgr.load_project_graph().unwrap();
            let stale = graph.get_memory_mut(&stale_id).unwrap();
            stale.updated_at = Utc::now() - chrono::Duration::days(3);
            mgr.save_project_graph(&graph).unwrap();
        }
        mgr.cfg.memory_size_limit = Some(1);

        let stats = mgr.compact(MemoryScope::Project, 24 * 30).unwrap();
        assert_eq!(stats.project_removed, 2);

        let remaining = mgr.list(MemoryScope::Project).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, recent_b);
    }

    #[test]
    fn test_rebuild_on_model_change_reembeds_existing_memories() {
        let provider = FixedEmbeddingProvider::new("test-embed", |_inputs| vec![vec![0.8, 0.2]]);
        let temp_storage =
            std::env::temp_dir().join(format!("fox-memory-model-change-{}", uuid::Uuid::new_v4()));
        let project_dir =
            std::env::temp_dir().join(format!("fox-memory-model-change-project-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_storage).unwrap();
        std::fs::create_dir_all(&project_dir).unwrap();

        let mut cfg = MemoryConfig::default();
        cfg.rebuild_on_model_change = true;
        let mgr = MemoryManager::new(&cfg)
            .with_storage_dir(temp_storage)
            .with_project_dir(project_dir)
            .with_embedding_provider(Arc::new(provider));

        let id = mgr
            .remember_project(MemoryEntry::new(MemoryCategory::Fact, "needs refresh"))
            .unwrap();
        {
            let mut graph = mgr.load_project_graph().unwrap();
            let entry = graph.get_memory_mut(&id).unwrap();
            entry.embedding_version = Some("old".to_string());
            graph.metadata.embedding_version = Some("old".to_string());
            mgr.save_project_graph(&graph).unwrap();
        }

        let hits = mgr
            .recall_detailed(
                Some("needs refresh"),
                5,
                RecallMode::Semantic,
                MemoryScope::Project,
            )
            .unwrap();
        assert!(!hits.is_empty());

        let graph = mgr.load_project_graph().unwrap();
        let entry = graph.get_memory(&id).unwrap();
        assert_eq!(entry.embedding_version.as_deref(), Some("test"));
        assert_eq!(graph.metadata.embedding_version.as_deref(), Some("test"));
        assert!(graph.metadata.last_embedding_rebuild_at.is_some());
    }

    #[test]
    fn test_regression_dataset_covers_keyword_semantic_and_cascade_modes() {
        let provider = FixedEmbeddingProvider::new("test-embed", |inputs| {
            inputs
                .iter()
                .map(|input| {
                    if input.contains("short direct rust")
                        || input.contains("keep it concise for rust")
                        || input.contains("rust style sibling")
                    {
                        vec![1.0, 0.0]
                    } else if input.contains("python walkthrough") {
                        vec![0.0, 1.0]
                    } else {
                        vec![0.5, 0.5]
                    }
                })
                .collect()
        });
        let mgr = MemoryManager::new_test().with_embedding_provider(Arc::new(provider));
        let concise_id = mgr
            .remember_project(MemoryEntry::new(
                MemoryCategory::Preference,
                "Prefer short direct rust answers",
            ))
            .unwrap();
        let sibling_id = mgr
            .remember_project(MemoryEntry::new(
                MemoryCategory::Fact,
                "rust style sibling memory",
            ))
            .unwrap();
        mgr.link_memories(&concise_id, &sibling_id, 1.0).unwrap();
        mgr.remember_project(MemoryEntry::new(
            MemoryCategory::Preference,
            "Prefer python walkthrough responses",
        ))
        .unwrap();

        let keyword_hits = mgr
            .recall_detailed(
                Some("short direct rust"),
                5,
                RecallMode::Keyword,
                MemoryScope::Project,
            )
            .unwrap();
        assert_eq!(keyword_hits[0].entry.id, concise_id);

        let semantic_hits = mgr
            .recall_detailed(
                Some("keep it concise for rust"),
                5,
                RecallMode::Semantic,
                MemoryScope::Project,
            )
            .unwrap();
        assert_eq!(semantic_hits[0].entry.id, concise_id);
        assert!(matches!(
            semantic_hits[0].retrieval_source,
            RetrievalSource::Semantic | RetrievalSource::SemanticAnn
        ));

        let cascade_hits = mgr
            .recall_detailed(
                Some("keep it concise for rust"),
                5,
                RecallMode::Cascade,
                MemoryScope::Project,
            )
            .unwrap();
        assert!(cascade_hits.iter().any(|hit| hit.entry.id == sibling_id));
    }

    #[tokio::test]
    async fn test_ingest_transcript_reinforces_duplicates() {
        let provider = FixedEmbeddingProvider::new("test-embed", |inputs| {
            inputs.iter().map(|_| vec![1.0, 0.0]).collect()
        });
        let mgr = MemoryManager::new_test().with_embedding_provider(Arc::new(provider));
        let existing_id = mgr
            .remember_project(MemoryEntry::new(MemoryCategory::Preference, "Prefer concise rust"))
            .unwrap();

        let report = mgr
            .ingest_transcript(
                "User: keep rust concise",
                &StaticExtractor {
                    items: vec![ExtractedMemory {
                        category: "preference".into(),
                        content: "Prefer concise rust".into(),
                        trust: "high".into(),
                    }],
                },
                None,
            )
            .await
            .unwrap();

        assert!(report.created_ids.is_empty());
        assert_eq!(report.reinforced_ids, vec![existing_id.clone()]);
        let graph = mgr.load_project_graph().unwrap();
        let existing = graph.get_memory(&existing_id).unwrap();
        assert!(existing.strength >= 2);
    }

    #[tokio::test]
    async fn test_ingest_transcript_marks_contradictions() {
        let provider = FixedEmbeddingProvider::new("test-embed", |inputs| {
            inputs
                .iter()
                .map(|input| {
                    if input.contains("spaces") {
                        vec![1.0, 0.0]
                    } else if input.contains("tabs") {
                        vec![0.0, 1.0]
                    } else {
                        vec![0.5, 0.5]
                    }
                })
                .collect()
        });
        let mut cfg = MemoryConfig::default();
        cfg.contradiction_policy = ContradictionPolicy::MarkContradictionEdge;
        let mgr = MemoryManager::new(&cfg)
            .with_storage_dir(std::env::temp_dir().join(format!("fox-memory-ingest-{}", uuid::Uuid::new_v4())))
            .with_embedding_provider(Arc::new(provider));
        let old_id = mgr
            .remember_project(MemoryEntry::new(MemoryCategory::Preference, "Use tabs"))
            .unwrap();

        let report = mgr
            .ingest_transcript(
                "User: switch to spaces",
                &StaticExtractor {
                    items: vec![ExtractedMemory {
                        category: "preference".into(),
                        content: "Use spaces".into(),
                        trust: "high".into(),
                    }],
                },
                Some(&StaticChecker {
                    relevant: true,
                    contradictions: vec![("Use spaces".into(), "Use tabs".into())],
                }),
            )
            .await
            .unwrap();

        assert_eq!(report.created_ids.len(), 1);
        assert_eq!(report.contradiction_ids, vec![old_id.clone()]);
        let graph = mgr.load_project_graph().unwrap();
        let edge_count = graph
            .edges
            .iter()
            .filter(|(source, edges)| {
                ((*source == &report.created_ids[0]) || (*source == &old_id))
                    && edges.iter().any(|edge| {
                        edge.target == report.created_ids[0] || edge.target == old_id
                    })
            })
            .count();
        assert!(edge_count > 0);
    }

    #[tokio::test]
    async fn test_ingest_transcript_skips_irrelevant_candidates() {
        let mut cfg = MemoryConfig::default();
        cfg.verify_relevance = true;
        let mgr = MemoryManager::new(&cfg).with_storage_dir(
            std::env::temp_dir().join(format!("fox-memory-irrelevant-{}", uuid::Uuid::new_v4())),
        );

        let report = mgr
            .ingest_transcript(
                "User: random text",
                &StaticExtractor {
                    items: vec![ExtractedMemory {
                        category: "fact".into(),
                        content: "irrelevant memory".into(),
                        trust: "low".into(),
                    }],
                },
                Some(&StaticChecker {
                    relevant: false,
                    contradictions: vec![],
                }),
            )
            .await
            .unwrap();

        assert!(report.created_ids.is_empty());
        assert_eq!(report.skipped_irrelevant, 1);
    }

    // ── Session scope + promotion ──

    fn session_test_manager() -> MemoryManager {
        let temp = std::env::temp_dir().join(format!("fox-memory-session-{}", uuid::Uuid::new_v4()));
        MemoryManager::new_test()
            .with_storage_dir(temp)
            .with_session_id("test-session-1")
    }

    #[test]
    fn session_memory_is_isolated_from_project() {
        let mgr = session_test_manager();
        let sid = mgr
            .remember_session(MemoryEntry::new(MemoryCategory::Fact, "session-only note"))
            .unwrap();

        // Present in session scope.
        let session_list = mgr.list(MemoryScope::Session).unwrap();
        assert!(session_list.iter().any(|e| e.id == sid));

        // Absent from project scope.
        let project_list = mgr.list(MemoryScope::Project).unwrap();
        assert!(project_list.is_empty());
    }

    #[test]
    fn manual_promote_moves_session_memory_to_project() {
        let mgr = session_test_manager();
        let sid = mgr
            .remember_session(MemoryEntry::new(MemoryCategory::Fact, "valuable finding"))
            .unwrap();

        let new_id = mgr
            .promote_memory(&sid, MemoryScope::Session, MemoryScope::Project)
            .unwrap();

        // Gone from session, present in project.
        let session_list = mgr.list(MemoryScope::Session).unwrap();
        assert!(!session_list.iter().any(|e| e.id == sid));
        let project_list = mgr.list(MemoryScope::Project).unwrap();
        assert!(project_list.iter().any(|e| e.id == new_id));
        // Provenance recorded.
        let promoted = project_list.iter().find(|e| e.id == new_id).unwrap();
        assert_eq!(promoted.source.as_deref(), Some("promoted_from:session"));
    }

    #[test]
    fn promote_into_session_is_rejected() {
        let mgr = session_test_manager();
        let sid = mgr
            .remember_project(MemoryEntry::new(MemoryCategory::Fact, "x"))
            .unwrap();
        let err = mgr.promote_memory(&sid, MemoryScope::Project, MemoryScope::Session);
        assert!(err.is_err());
    }

    #[test]
    fn auto_promote_triggers_at_strength_threshold() {
        let temp = std::env::temp_dir().join(format!("fox-memory-autopromo-{}", uuid::Uuid::new_v4()));
        let mut cfg = MemoryConfig::default();
        cfg.auto_promote_enabled = true;
        cfg.auto_promote_strength_threshold = 3;
        cfg.auto_promote_target = AutoExtractScope::Project;
        let mgr = MemoryManager::new(&cfg)
            .with_storage_dir(temp)
            .with_session_id("test-session-auto");

        let sid = mgr
            .remember_session(MemoryEntry::new(MemoryCategory::Fact, "repeated finding"))
            .unwrap();

        // strength starts at 1. Reinforce once → 2 (no promote yet).
        mgr.reinforce_memory(MemoryScope::Session, &sid).unwrap();
        assert!(mgr.list(MemoryScope::Session).unwrap().iter().any(|e| e.id == sid));
        assert!(mgr.list(MemoryScope::Project).unwrap().is_empty());

        // Reinforce again → 3 → auto-promoted to project.
        mgr.reinforce_memory(MemoryScope::Session, &sid).unwrap();
        assert!(!mgr.list(MemoryScope::Session).unwrap().iter().any(|e| e.id == sid));
        let project = mgr.list(MemoryScope::Project).unwrap();
        assert_eq!(project.len(), 1);
        assert_eq!(project[0].source.as_deref(), Some("promoted_from:session"));
    }
}
