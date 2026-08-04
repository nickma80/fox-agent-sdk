//! Memory system for cross-session learning.
//!
//! Provides persistent memory across sessions, organized by:
//! - Project (per working directory)
//! - Global (user-level preferences)
//!
//! Storage uses MemoryGraph format with JSON files,
//! LRU caching, and automatic backup recovery.

pub mod graph;
pub mod index;
pub mod prompt;
pub mod ranking;
pub mod relevance;
pub mod storage;
pub mod types;
pub mod wiki;

#[allow(unused_imports)]
pub use graph::{Edge, EdgeKind, GRAPH_VERSION, GraphMetadata, MemoryGraph, TagEntry};
#[allow(unused_imports)]
pub use index::{IndexEntry, MemoryIndex, slugify};
#[allow(unused_imports)]
pub use relevance::{ExtractedMemory, MemoryExtractor, MemoryRelevanceChecker};
#[allow(unused_imports)]
pub use storage::{
    GCResult, MemoryGraphCache, cache_graph, cached_graph, default_storage_dir, gc_memory_files,
    invalidate_cache, project_hash, read_json, write_json,
};
#[allow(unused_imports)]
pub use types::{
    MemoryCategory, MemoryEntry, MemoryScope, NarrativeRecord, RecallMode, Reinforcement,
    TrustLevel, memory_matches_search, memory_score, normalize_memory_search_text,
    normalize_search_text,
};
#[allow(unused_imports)]
pub use wiki::{
    EnrichedMemory, QueryExpansion, RankedCandidate, WikiAssistant, parse_enrich_output,
    parse_query_expansion, parse_rerank_output,
};

use crate::config::{AutoExtractScope, ContradictionPolicy, MemoryConfig};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Events emitted by the memory pipeline.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum MemoryStateEvent {
    InjectionComputed {
        count: u32,
        memory_ids: Vec<String>,
        prompt_chars: usize,
    },
    InjectionConsumed {
        count: u32,
        memory_ids: Vec<String>,
        prompt_chars: usize,
    },
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
    /// Optional wiki assistant for LLM-backed enrich / dedupe / rerank (§4.2/§4.3).
    /// None keeps all operations synchronous and lexical-only.
    wiki_assistant: Option<Arc<dyn WikiAssistant>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// How a memory was retrieved — used for diagnostics and scoring transparency.
pub enum RetrievalSource {
    /// Returned by recency-only scan (no query).
    Recent,
    /// Matched via keyword term overlap on search_text.
    Keyword,
    /// Seed hit from keyword/wiki phase in a cascade search.
    CascadeSeed,
    /// Surfaced by graph traversal from a seed hit in a cascade search.
    CascadeGraph,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// Decomposed scoring for a single recall hit — enables explainable retrieval.
pub struct ScoreBreakdown {
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
/// Statistics from a wiki export operation (§3.3).
pub struct WikiExportStats {
    /// Absolute path to the generated `index.md`.
    pub index_path: String,
    /// Absolute path to the generated `pages/` directory.
    pub pages_dir: String,
    /// Number of page files written.
    pub pages_written: usize,
    /// Total memories scanned (including inactive).
    pub memories: usize,
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
            wiki_assistant: None,
        }
    }

    /// Set the project directory (for scoping project memories).
    pub fn with_project_dir(mut self, dir: PathBuf) -> Self {
        self.project_dir = Some(dir);
        self
    }

    /// Set the session ID for Session-scoped memory isolation.
    ///
    /// Session memories are stored in `{storage}/session_scoped/{session_id}.json`
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
            wiki_assistant: None,
        }
    }

    pub fn is_test_mode(&self) -> bool {
        self.test_mode
    }

    pub fn with_storage_dir(mut self, dir: PathBuf) -> Self {
        self.storage_dir = dir;
        self
    }

    /// Attach a wiki assistant for LLM-backed enrich / dedupe / rerank.
    ///
    /// When set (and `enrich_on_write` is on), `remember_*` schedules a
    /// background enrich; `ingest_transcript` may use `are_same` for dedupe.
    pub fn with_wiki_assistant(mut self, assistant: Arc<dyn WikiAssistant>) -> Self {
        self.wiki_assistant = Some(assistant);
        self
    }

    pub fn wiki_enabled(&self) -> bool {
        self.cfg.wiki_enabled
    }

    // ── Index (lazy, on-demand) ──

    /// Rebuild the in-memory [`MemoryIndex`] for a scope from its graph(s).
    ///
    /// The index is a derived projection — every write invalidates it implicitly
    /// because the next call rebuilds from the current graph (Phase 3 "惰性重建").
    /// Phase 5 adds `{graph}.index.json` persistence on top of this builder.
    pub fn rebuild_index(&self, scope: MemoryScope) -> Result<MemoryIndex, String> {
        let mut entries: Vec<IndexEntry> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (_, graph) in self.load_scope_graphs(scope) {
            for entry in graph.all_memories() {
                if !entry.active || !seen.insert(entry.id.clone()) {
                    continue;
                }
                entries.push(IndexEntry {
                    id: entry.id.clone(),
                    title: entry.title.clone(),
                    summary: entry.summary.clone(),
                    tags: entry.tags.clone(),
                    aliases: entry.aliases.clone(),
                });
            }
        }
        Ok(MemoryIndex {
            entries,
            updated_at: chrono::Utc::now(),
        })
    }

    /// Load the graphs covered by `scope` as (scope, graph) pairs.
    fn load_scope_graphs(&self, scope: MemoryScope) -> Vec<(MemoryScope, MemoryGraph)> {
        let mut out = Vec::new();
        if scope.includes_session()
            && let Ok(g) = self.load_session_graph()
        {
            out.push((MemoryScope::Session, g));
        }
        if scope.includes_project()
            && let Ok(g) = self.load_project_graph()
        {
            out.push((MemoryScope::Project, g));
        }
        if scope.includes_global()
            && let Ok(g) = self.load_global_graph()
        {
            out.push((MemoryScope::Global, g));
        }
        out
    }

    /// Persist the per-graph index projections to `{graph}.index.json` (§3.3).
    ///
    /// Each covered scope writes its own local index (only that graph's
    /// entries); the returned [`MemoryIndex`] is the cross-scope combined
    /// projection.  Writes are explicit — the in-memory path stays lazy
    /// ([`Self::rebuild_index`] always reads the current graph).
    pub fn persist_index(&self, scope: MemoryScope) -> Result<MemoryIndex, String> {
        let combined = self.rebuild_index(scope)?;
        for (s, graph) in self.load_scope_graphs(scope) {
            let local = MemoryIndex::from_graph(&graph);
            let path = self.index_file_path(s)?;
            storage::write_json(&path, &local)?;
        }
        Ok(combined)
    }

    /// Load the index for a scope, always rebuilt from the current graph(s)
    /// (lazy — no stale snapshot is ever served).
    pub fn load_index(&self, scope: MemoryScope) -> Result<MemoryIndex, String> {
        self.rebuild_index(scope)
    }

    /// Compact llms.txt-style index digest for prompt injection (§7.1 step 3).
    pub fn index_to_prompt(&self, scope: MemoryScope, budget_chars: usize) -> Option<String> {
        self.rebuild_index(scope).ok()?.to_prompt(budget_chars)
    }

    /// Batch-enrich all active `enriched=false` memories in scope (PRD §8.1).
    ///
    /// `limit == 0` means unlimited.  Returns the number of successfully
    /// enriched entries; per-entry failures are logged and skipped.
    pub async fn backfill_enrich(&self, scope: MemoryScope, limit: usize) -> Result<usize, String> {
        let Some(assistant) = self.wiki_assistant.clone() else {
            return Ok(0);
        };
        let cap = if limit == 0 { usize::MAX } else { limit };
        let mut enriched_count = 0usize;
        for (s, graph) in self.load_scope_graphs(scope) {
            let ids: Vec<String> = graph
                .all_memories()
                .filter(|m| m.active && !m.enriched)
                .map(|m| m.id.clone())
                .take(cap)
                .collect();
            for id in ids {
                match self.run_enrich(&id, s, assistant.clone()).await {
                    Ok(()) => enriched_count += 1,
                    Err(e) => {
                        tracing::warn!(memory_id = %id, error = %e, "backfill enrich failed");
                    }
                }
            }
        }
        Ok(enriched_count)
    }

    /// Export the wiki projection (index.md + pages/<slug>.md) into `dir` (§3.3).
    ///
    /// Pages render title/tags/aliases frontmatter + raw content; slugs are
    /// unique per entry (duplicate titles get `-2`, `-3`, … suffixes) and the
    /// generated index.md links point to the actual page files.
    pub fn export_wiki(&self, scope: MemoryScope, dir: &Path) -> Result<WikiExportStats, String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("failed to create wiki dir: {e}"))?;

        let memories = self.collect_memories(scope)?;
        let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut active: Vec<&MemoryEntry> = memories
            .iter()
            .filter(|e| e.active && seen_ids.insert(e.id.clone()))
            .collect();
        // 确定性排序：先创建的条目获得基础 slug（HashMap 迭代顺序是随机的）。
        active.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });

        // 为每个 active 记忆分配唯一 slug（标题 slugify，重复标题追加 -2/-3…）。
        let mut slug_for: HashMap<String, String> = HashMap::new();
        let mut used: HashMap<String, usize> = HashMap::new();
        for entry in &active {
            let base = entry
                .title
                .as_deref()
                .map(slugify)
                .unwrap_or_else(|| slugify(&entry.id));
            let n = used.entry(base.clone()).or_insert(0);
            let slug = if *n == 0 {
                base.clone()
            } else {
                format!("{base}-{}", *n + 1)
            };
            *n += 1;
            slug_for.insert(entry.id.clone(), slug);
        }

        let pages_dir = dir.join("pages");
        std::fs::create_dir_all(&pages_dir)
            .map_err(|e| format!("failed to create pages dir: {e}"))?;
        for entry in &active {
            let slug = &slug_for[&entry.id];
            let page = render_wiki_page(entry);
            std::fs::write(pages_dir.join(format!("{slug}.md")), page)
                .map_err(|e| format!("failed to write page {slug}.md: {e}"))?;
        }

        let index = self.rebuild_index(scope)?;
        let mut md = String::from("# Memory Index\n\n");
        md.push_str(&format!(
            "Updated: {}\n\n",
            index.updated_at.format("%Y-%m-%d %H:%M UTC")
        ));
        for e in &index.entries {
            let title = e.title.clone().unwrap_or_else(|| e.id.clone());
            let slug = slug_for
                .get(&e.id)
                .cloned()
                .unwrap_or_else(|| slugify(&title));
            let link = format!("[{}]({})", title, MemoryIndex::page_path(&slug));
            match e.summary.as_deref() {
                Some(s) if !s.is_empty() => md.push_str(&format!("- {link} — {s}\n")),
                _ => md.push_str(&format!("- {link}\n")),
            }
        }
        std::fs::write(dir.join("index.md"), md)
            .map_err(|e| format!("failed to write index.md: {e}"))?;

        Ok(WikiExportStats {
            index_path: dir.join("index.md").to_string_lossy().into_owned(),
            pages_dir: pages_dir.to_string_lossy().into_owned(),
            pages_written: active.len(),
            memories: memories.len(),
        })
    }

    /// Path to the persisted index projection for a concrete scope.
    fn index_file_path(&self, scope: MemoryScope) -> Result<PathBuf, String> {
        let graph_path = match scope {
            MemoryScope::Project => self.project_memory_path()?,
            MemoryScope::Global => self.global_memory_path(),
            MemoryScope::Session => self.session_memory_path()?,
            MemoryScope::All => return Err("index file requires a concrete scope".to_string()),
        };
        Ok(graph_path.with_extension("index.json"))
    }

    // ── Write fast-path hooks (Phase 3) ──

    /// Post-write hook: schedules a background enrich when configured.
    fn after_write(&self, scope: MemoryScope, id: &str) {
        if self.cfg.enrich_on_write {
            self.spawn_enrich(id.to_string(), scope);
        }
    }

    /// Spawn a background enrich task for a freshly-written memory (§4.2.2).
    ///
    /// No-ops when no assistant is attached (fully synchronous memory system).
    fn spawn_enrich(&self, id: String, scope: MemoryScope) {
        let Some(assistant) = self.wiki_assistant.clone() else {
            return;
        };
        let mgr = self.clone();
        tokio::spawn(async move {
            if let Err(e) = mgr.run_enrich(&id, scope, assistant).await {
                tracing::warn!(memory_id = %id, error = %e, "background enrich failed");
            }
        });
    }

    /// Apply assistant enrichment to a single memory: title/summary/tags/aliases
    /// (+ `[[links]]` when `link_discovery_enabled`) and mark `enriched`.
    async fn run_enrich(
        &self,
        id: &str,
        scope: MemoryScope,
        assistant: Arc<dyn WikiAssistant>,
    ) -> Result<(), String> {
        let mut graph = match scope {
            MemoryScope::Session => self.load_session_graph()?,
            MemoryScope::Global => self.load_global_graph()?,
            MemoryScope::Project | MemoryScope::All => self.load_project_graph()?,
        };
        let entry = graph
            .get_memory(id)
            .cloned()
            .ok_or_else(|| format!("memory {id} not found for enrich"))?;
        if entry.enriched {
            return Ok(()); // idempotent — already enriched
        }

        let index = MemoryIndex::from_graph(&graph);
        let enriched = assistant.enrich(&entry, &index.all_titles()).await?;

        if let Some(entry_mut) = graph.get_memory_mut(id) {
            if !enriched.title.is_empty() {
                entry_mut.title = Some(enriched.title);
            }
            if !enriched.summary.is_empty() {
                entry_mut.summary = Some(enriched.summary);
            }
            for tag in enriched.tags {
                if !entry_mut.tags.iter().any(|t| t == &tag) {
                    entry_mut.tags.push(tag);
                }
            }
            for alias in enriched.aliases {
                if !entry_mut.aliases.iter().any(|a| a == &alias) {
                    entry_mut.aliases.push(alias);
                }
            }
            entry_mut.enriched = true;
            entry_mut.refresh_search_text();
        }
        if self.cfg.link_discovery_enabled {
            for link_id in &enriched.link_ids {
                if link_id != id {
                    graph.link_memories(id, link_id, 0.8);
                }
            }
        }
        match scope {
            MemoryScope::Session => self.save_session_graph(&graph)?,
            MemoryScope::Global => self.save_global_graph(&graph)?,
            MemoryScope::Project | MemoryScope::All => self.save_project_graph(&graph)?,
        }
        Ok(())
    }

    // ── Path helpers ──

    fn project_memory_path(&self) -> Result<PathBuf, String> {
        let project_dir = self
            .project_dir
            .clone()
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
        let dir = self.storage_dir.clone();
        let _ = std::fs::create_dir_all(&dir);
        dir.join("global.json")
    }

    fn session_memory_path(&self) -> Result<PathBuf, String> {
        let sid = self
            .session_id
            .as_ref()
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
        if !self.test_mode
            && let Some(cached) = storage::cached_graph(path)
        {
            return Ok(cached);
        }

        if !path.exists() {
            return Ok(MemoryGraph::new());
        }

        let graph = match storage::read_json::<MemoryGraph>(path) {
            Ok(g) => g,
            // PRD §12.2：主文件与 `.json.bak` 备份均损坏时优雅降级为空图，
            // 不 panic；作用域级调用方（load_scope_graphs）以 Ok 过滤。
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "memory graph corrupt; degraded to empty graph"
                );
                MemoryGraph::new()
            }
        };
        if !self.test_mode {
            storage::cache_graph(path.to_path_buf(), &graph);
        }
        Ok(graph)
    }

    fn save_graph(&self, path: &Path, graph: &MemoryGraph) -> Result<(), String> {
        storage::write_json(path, graph)?;
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
        let entry = self.prepare_entry_for_storage(entry);
        let id = graph.add_memory(entry);
        self.apply_governance_policies(&mut graph);
        self.save_project_graph(&graph)?;
        self.after_write(MemoryScope::Project, &id);
        Ok(id)
    }

    /// Store a memory in the global scope.
    pub fn remember_global(&self, entry: MemoryEntry) -> Result<String, String> {
        let mut graph = self.load_global_graph()?;
        let entry = self.prepare_entry_for_storage(entry);
        let id = graph.add_memory(entry);
        self.apply_governance_policies(&mut graph);
        self.save_global_graph(&graph)?;
        self.after_write(MemoryScope::Global, &id);
        Ok(id)
    }

    /// Store a memory in the session-local scope.
    ///
    /// Requires `with_session_id()` to have been called.
    pub fn remember_session(&self, entry: MemoryEntry) -> Result<String, String> {
        let mut graph = self.load_session_graph()?;
        let entry = self.prepare_entry_for_storage(entry);
        let id = graph.add_memory(entry);
        self.apply_governance_policies(&mut graph);
        self.save_session_graph(&graph)?;
        self.after_write(MemoryScope::Session, &id);
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
            .ok_or_else(|| {
                format!(
                    "promote: memory '{id}' not found in {} scope",
                    scope_name(from)
                )
            })?
            .clone();

        // Record provenance so the promotion is auditable.
        entry.source = Some(format!("promoted_from:{}", scope_name(from)));
        entry.updated_at = chrono::Utc::now();

        // Write the copy into the target scope.
        let mut target_graph = self.load_write_scope_graph(to)?;
        let new_id = target_graph.add_memory(entry);
        self.apply_governance_policies(&mut target_graph);
        self.save_write_scope_graph(to, &target_graph)?;

        // Remove from the source scope only after the target write succeeds.
        source_graph.remove_memory(id);
        self.save_write_scope_graph(from, &source_graph)?;

        Ok(new_id)
    }

    // ── CRUD: narratives ──

    /// Store a narrative record in session scope.
    pub fn remember_narrative(
        &self,
        record: &NarrativeRecord,
        session_id: &str,
    ) -> Result<String, String> {
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
    pub fn recall(
        &self,
        query: Option<&str>,
        limit: usize,
        mode: RecallMode,
        scope: MemoryScope,
    ) -> Result<Vec<(MemoryEntry, f32)>, String> {
        Ok(self
            .recall_detailed(query, limit, mode, scope)?
            .into_iter()
            .map(|hit| (hit.entry, hit.score))
            .collect())
    }

    pub fn recall_detailed(
        &self,
        query: Option<&str>,
        limit: usize,
        mode: RecallMode,
        scope: MemoryScope,
    ) -> Result<Vec<RecallHit>, String> {
        match mode {
            RecallMode::Recent => self.recall_recent(limit, scope),
            RecallMode::Keyword => {
                let q = query.unwrap_or("");
                if q.is_empty() {
                    return Ok(Vec::new());
                }
                self.recall_keyword(q, limit, scope)
            }
            RecallMode::Wiki => {
                let q = query.unwrap_or("");
                if q.is_empty() {
                    return Ok(Vec::new());
                }
                // Wiki 模式（§5.2）：词汇预筛 + 图 BFS 扩散的同步路径（无 LLM）。
                // 带 assistant 的 LLM 查询扩展/重排请调用 `recall_wiki_async`。
                self.recall_wiki(q, limit, scope)
            }
        }
    }

    fn recall_recent(&self, limit: usize, scope: MemoryScope) -> Result<Vec<RecallHit>, String> {
        let all = self.collect_memories(scope)?;
        let scored: Vec<RecallHit> = all
            .into_iter()
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

    fn recall_keyword(
        &self,
        query: &str,
        limit: usize,
        scope: MemoryScope,
    ) -> Result<Vec<RecallHit>, String> {
        let nq = normalize_search_text(query);
        if nq.is_empty() {
            return Ok(Vec::new());
        }
        let all = self.collect_memories(scope)?;
        let matches: Vec<RecallHit> = all
            .into_iter()
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

    /// Wiki recall（§5.2）：查询扩展（纯词汇回退）→ 加权词汇预筛 → 图 BFS 扩散。
    ///
    /// 同步路径（无 LLM），`recall_detailed` 的 `Wiki` 分支使用；需要 LLM 查询
    /// 扩展 / 重排时请调用 [`Self::recall_wiki_async`]。
    fn recall_wiki(
        &self,
        query: &str,
        limit: usize,
        scope: MemoryScope,
    ) -> Result<Vec<RecallHit>, String> {
        let expansion = QueryExpansion::from_query(query);
        let candidates = self.wiki_prefilter(&expansion, limit, scope)?;
        self.recall_wiki_inner(&candidates, None, limit, scope)
    }

    /// Wiki recall 异步版本（§5.2）：LLM 查询扩展、加权词汇预筛、可选 LLM 重排，
    /// 最后做图 BFS 扩散。未装配 assistant、功能开关关闭或 LLM 调用失败时自动
    /// 退化为纯词汇路径。
    pub async fn recall_wiki_async(
        &self,
        query: &str,
        limit: usize,
        scope: MemoryScope,
    ) -> Result<Vec<RecallHit>, String> {
        let lexical = QueryExpansion::from_query(query);
        let expansion = if self.cfg.query_expansion_enabled {
            match &self.wiki_assistant {
                Some(assistant) => assistant
                    .expand_query(query)
                    .await
                    .unwrap_or_else(|_| lexical.clone()),
                None => lexical.clone(),
            }
        } else {
            lexical.clone()
        };

        let candidates = self.wiki_prefilter(&expansion, limit, scope)?;
        if candidates.is_empty() {
            return Ok(Vec::new());
        }

        let reranked = if self.cfg.rerank_enabled {
            match &self.wiki_assistant {
                Some(assistant) => {
                    let entries: Vec<MemoryEntry> =
                        candidates.iter().map(|(entry, _)| entry.clone()).collect();
                    assistant.rerank(query, &entries).await.ok()
                }
                None => None,
            }
        } else {
            None
        };

        self.recall_wiki_inner(&candidates, reranked.as_deref(), limit, scope)
    }

    /// §5.2 ② 加权词汇预筛：title(3.0)/aliases(2.0)/tags(1.5)/content(1.0) 加权，
    /// 仅保留命中项，按分数降序并截断到 `limit × rerank_candidate_multiplier`。
    fn wiki_prefilter(
        &self,
        expansion: &QueryExpansion,
        limit: usize,
        scope: MemoryScope,
    ) -> Result<Vec<(MemoryEntry, f32)>, String> {
        let mut scored: Vec<(MemoryEntry, f32)> = self
            .collect_memories(scope)?
            .into_iter()
            .filter(|entry| entry.active)
            .filter_map(|entry| {
                let score = lexical_prefilter_score(&entry, expansion);
                if score > 0.0 {
                    Some((entry, score))
                } else {
                    None
                }
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let max_candidates = limit.saturating_mul(self.cfg.rerank_candidate_multiplier.max(1));
        scored.truncate(max_candidates);
        Ok(scored)
    }

    /// §5.2 ③④ 从预筛候选中挑选种子（重排序或词法序）→ 图 BFS 扩散 → top-k。
    fn recall_wiki_inner(
        &self,
        candidates: &[(MemoryEntry, f32)],
        reranked: Option<&[RankedCandidate]>,
        limit: usize,
        scope: MemoryScope,
    ) -> Result<Vec<RecallHit>, String> {
        let seed_limit = limit.saturating_mul(2).max(1);
        let seed_hits: Vec<RecallHit> = match reranked {
            Some(ranked) => {
                let mut hits = Vec::new();
                for rc in ranked {
                    // 重排输出按候选序号（1-based）引用预筛结果。
                    let Some(idx) = rc.id.parse::<usize>().ok().and_then(|i| i.checked_sub(1))
                    else {
                        continue;
                    };
                    let Some((entry, lexical)) = candidates.get(idx).cloned() else {
                        continue;
                    };
                    hits.push(wiki_seed_hit(entry, Some(rc.score), lexical));
                    if hits.len() >= seed_limit {
                        break;
                    }
                }
                // 重排输出无法映射时回退到词法序种子。
                if hits.is_empty() {
                    candidates
                        .iter()
                        .take(seed_limit)
                        .map(|(entry, lexical)| wiki_seed_hit(entry.clone(), None, *lexical))
                        .collect()
                } else {
                    hits
                }
            }
            None => candidates
                .iter()
                .take(seed_limit)
                .map(|(entry, lexical)| wiki_seed_hit(entry.clone(), None, *lexical))
                .collect(),
        };

        if seed_hits.is_empty() {
            return Ok(Vec::new());
        }

        let mut merged: HashMap<String, RecallHit> = seed_hits
            .into_iter()
            .map(|hit| (hit.entry.id.clone(), hit))
            .collect();
        self.expand_cascade(&mut merged, limit, scope)?;
        Ok(top_k_hits(merged.into_values().collect(), limit))
    }

    /// 从种子命中做图链接 BFS 扩散（`recall_wiki` 内部种子扩散阶段共用）。
    fn expand_cascade(
        &self,
        merged: &mut HashMap<String, RecallHit>,
        limit: usize,
        scope: MemoryScope,
    ) -> Result<(), String> {
        let (seed_ids, seed_scores): (Vec<String>, Vec<f32>) = merged
            .iter()
            .map(|(id, hit)| (id.clone(), hit.score))
            .unzip();
        let entry_map: HashMap<String, MemoryEntry> = self
            .collect_memories(scope)?
            .into_iter()
            .map(|entry| (entry.id.clone(), entry))
            .collect();
        let depth = self.cfg.max_graph_depth.max(1);
        let breadth = limit.saturating_mul(3).max(1);
        for (_, graph) in self.load_scope_graphs(scope) {
            let cascaded = graph.cascade_retrieve(&seed_ids, &seed_scores, depth, breadth);
            apply_cascade_results(merged, &entry_map, cascaded);
        }
        Ok(())
    }

    // ── CRUD: search ──

    /// Search memories by text (exact substring match on search_text).
    pub fn search(&self, text: &str, scope: MemoryScope) -> Result<Vec<MemoryEntry>, String> {
        let nq = normalize_search_text(text);
        if nq.is_empty() {
            return Ok(Vec::new());
        }
        let all = self.collect_memories(scope)?;
        Ok(all
            .into_iter()
            .filter(|e| memory_matches_search(e, &nq))
            .collect())
    }

    // ── CRUD: list / forget ──

    /// List all memories, newest first.
    pub fn list(&self, scope: MemoryScope) -> Result<Vec<MemoryEntry>, String> {
        let mut all = self.collect_memories(scope)?;
        all.sort_by_key(|b| std::cmp::Reverse(b.updated_at));
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
            entry.refresh_search_text();
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
            entry.refresh_search_text();
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
        let entries: Vec<MemoryEntry> = results
            .into_iter()
            .filter(|(id, _)| id != memory_id)
            .filter_map(|(id, _)| graph.get_memory(&id).cloned())
            .collect();
        Ok(entries)
    }

    pub fn graph_stats(&self) -> Result<(usize, usize, usize), String> {
        let project = self.load_project_graph()?;
        let global = self.load_global_graph()?;
        let memories = project.memory_count() + global.memory_count();
        let tags = project.tags.len() + global.tags.len();
        let edges = project.edge_count() + global.edge_count();
        Ok((memories, tags, edges))
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
            project_memories: bundle
                .project
                .as_ref()
                .map(|g| g.memory_count())
                .unwrap_or(0),
            session_memories: bundle
                .session
                .as_ref()
                .map(|g| g.memory_count())
                .unwrap_or(0),
            global_memories: bundle
                .global
                .as_ref()
                .map(|g| g.memory_count())
                .unwrap_or(0),
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

    pub fn import_bundle(
        &self,
        bundle: MemoryExportBundle,
        merge: bool,
    ) -> Result<ImportStats, String> {
        let mut stats = ImportStats::default();
        if let Some(project) = bundle.project {
            let mut graph = if merge {
                self.load_project_graph()?
            } else {
                MemoryGraph::new()
            };
            if merge {
                merge_graph(&mut graph, project);
            } else {
                graph = project;
            }
            normalize_graph_after_import(&mut graph);
            self.apply_governance_policies(&mut graph);
            stats.project_memories = graph.memory_count();
            self.save_project_graph(&graph)?;
        }
        if let Some(session) = bundle.session
            && self.session_id.is_some()
        {
            let mut graph = if merge {
                self.load_session_graph()?
            } else {
                MemoryGraph::new()
            };
            if merge {
                merge_graph(&mut graph, session);
            } else {
                graph = session;
            }
            normalize_graph_after_import(&mut graph);
            self.apply_governance_policies(&mut graph);
            stats.session_memories = graph.memory_count();
            self.save_session_graph(&graph)?;
        }
        if let Some(global) = bundle.global {
            let mut graph = if merge {
                self.load_global_graph()?
            } else {
                MemoryGraph::new()
            };
            if merge {
                merge_graph(&mut graph, global);
            } else {
                graph = global;
            }
            normalize_graph_after_import(&mut graph);
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

            if self.cfg.verify_relevance
                && let Some(checker) = relevance_checker
            {
                let (relevant, _) = checker
                    .check_relevance(&candidate.content, transcript)
                    .await?;
                if !relevant {
                    report.skipped_irrelevant += 1;
                    continue;
                }
            }

            if let Some((dup_scope, dup_id)) =
                self.find_duplicate_for_ingestion(&candidate, existing_scope)?
            {
                self.reinforce_memory(dup_scope, &dup_id)?;
                report.reinforced_ids.push(dup_id);
                report.skipped_duplicates += 1;
                continue;
            }

            // §4.3: optional LLM dedupe — `are_same` on the best partial-overlap
            // candidate when the lexical threshold missed but the assistant is set.
            if let Some((dup_scope, dup_id)) = self
                .find_llm_duplicate_for_ingestion(&candidate, existing_scope)
                .await?
            {
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
        if scope.includes_project()
            && let Ok(graph) = self.load_project_graph()
        {
            all.extend(graph.all_memories().cloned());
        }
        if scope.includes_session()
            && let Ok(graph) = self.load_session_graph()
        {
            all.extend(graph.all_memories().cloned());
        }
        if scope.includes_global()
            && let Ok(graph) = self.load_global_graph()
        {
            all.extend(graph.all_memories().cloned());
        }
        Ok(all)
    }

    fn prepare_entry_for_storage(&self, mut entry: MemoryEntry) -> MemoryEntry {
        entry.refresh_search_text();
        entry
    }

    fn find_duplicate_for_ingestion(
        &self,
        candidate: &MemoryEntry,
        scope: MemoryScope,
    ) -> Result<Option<(MemoryScope, String)>, String> {
        if scope.includes_project() {
            let graph = self.load_project_graph()?;
            if let Some(id) =
                find_duplicate_in_graph(&graph, candidate, self.cfg.dedupe_min_overlap_ratio)
            {
                return Ok(Some((MemoryScope::Project, id)));
            }
        }
        if scope.includes_global() {
            let graph = self.load_global_graph()?;
            if let Some(id) =
                find_duplicate_in_graph(&graph, candidate, self.cfg.dedupe_min_overlap_ratio)
            {
                return Ok(Some((MemoryScope::Global, id)));
            }
        }
        Ok(None)
    }

    /// §4.3 optional LLM dedupe: pick the best same-category partial-overlap
    /// candidate (lexical ratio below threshold) and ask `are_same`.
    ///
    /// Bounded to at most one LLM call per candidate; no-op without an assistant
    /// or when no overlapping candidate exists.
    async fn find_llm_duplicate_for_ingestion(
        &self,
        candidate: &MemoryEntry,
        scope: MemoryScope,
    ) -> Result<Option<(MemoryScope, String)>, String> {
        let Some(assistant) = self.wiki_assistant.clone() else {
            return Ok(None);
        };
        let mut best_overlap = 0.0f32;
        let mut best: Option<(MemoryScope, MemoryEntry)> = None;
        for (s, graph) in self.load_scope_graphs(scope) {
            for entry in graph.all_memories() {
                if !entry.active || entry.category != candidate.category {
                    continue;
                }
                let overlap = title_alias_tag_overlap(entry, candidate);
                if overlap > best_overlap {
                    best_overlap = overlap;
                    best = Some((s, entry.clone()));
                }
            }
        }
        if best_overlap <= 0.0 {
            return Ok(None);
        }
        if let Some((dup_scope, existing)) = best
            && assistant
                .are_same(&candidate.content, &existing.content)
                .await?
        {
            return Ok(Some((dup_scope, existing.id)));
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
            if !has_text_overlap(&existing.content, &candidate.content) {
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
                        let target =
                            auto_extract_scope_to_memory_scope(self.cfg.auto_promote_target);
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

    fn save_write_scope_graph(
        &self,
        scope: MemoryScope,
        graph: &MemoryGraph,
    ) -> Result<(), String> {
        match scope {
            MemoryScope::Session => self.save_session_graph(graph),
            MemoryScope::Project | MemoryScope::All => self.save_project_graph(graph),
            MemoryScope::Global => self.save_global_graph(graph),
        }
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
        updated
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

    fn apply_governance_policies(&self, graph: &mut MemoryGraph) -> usize {
        let removed_by_retention = self.apply_retention_policy(graph);
        let removed_by_size = self.apply_size_limit(graph);
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
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    hits.truncate(limit);
    hits
}

/// §5.2 ② 加权词法预筛分：title 3.0 / aliases 2.0 / tags 1.5 / summary 1.0 /
/// content 1.0，归一化到 [0,1]。content 兜底保证未 enrich（无 wiki 元数据）的
/// 条目也能被召回。
fn lexical_prefilter_score(entry: &MemoryEntry, expansion: &QueryExpansion) -> f32 {
    let terms = expansion.all_search_terms();
    if terms.is_empty() {
        return 0.0;
    }
    let title_text = entry.title.as_deref().unwrap_or("");
    let summary_text = entry.summary.as_deref().unwrap_or("");
    let mut score = 0.0f32;
    for term in &terms {
        let t = term.to_lowercase();
        let in_title = !title_text.is_empty() && title_text.to_lowercase().contains(&t);
        let in_alias = entry.aliases.iter().any(|a| a.to_lowercase().contains(&t));
        let in_tag = entry.tags.iter().any(|tag| tag.to_lowercase().contains(&t));
        let in_summary = !summary_text.is_empty() && summary_text.to_lowercase().contains(&t);
        let in_content = entry.content.to_lowercase().contains(&t);
        if in_title {
            score += 3.0;
        } else if in_alias {
            score += 2.0;
        } else if in_tag {
            score += 1.5;
        } else if in_summary || in_content {
            score += 1.0;
        }
    }
    (score / (3.0 * terms.len() as f32)).min(1.0)
}

/// 构造一个 wiki 种子命中（§5.2 ③）：词汇分 + 可选重排分 + recency + trust。
fn wiki_seed_hit(entry: MemoryEntry, rerank_score: Option<f32>, lexical: f32) -> RecallHit {
    let recency = normalize_memory_score(&entry);
    let trust = normalize_trust_score(&entry);
    let final_score = match rerank_score {
        Some(rerank) => lexical * 0.4 + rerank * 0.4 + recency * 0.15 + trust * 0.05,
        None => lexical * 0.7 + recency * 0.2 + trust * 0.1,
    };
    RecallHit {
        entry,
        score: final_score,
        score_breakdown: ScoreBreakdown {
            keyword_score: Some(lexical),
            recency_score: recency,
            trust_score: trust,
            final_score,
            ..Default::default()
        },
        retrieval_source: RetrievalSource::CascadeSeed,
    }
}

/// Render a single wiki page (§3.3): YAML frontmatter (title/tags/aliases) +
/// raw content.
fn render_wiki_page(entry: &MemoryEntry) -> String {
    let mut out = String::from("---\n");
    let title = entry.title.as_deref().unwrap_or(&entry.id);
    out.push_str(&format!("title: \"{}\"\n", title.replace('"', "\\\"")));
    if !entry.tags.is_empty() {
        out.push_str(&format!(
            "tags: [{}]\n",
            entry
                .tags
                .iter()
                .map(|t| format!("\"{}\"", t.replace('"', "\\\"")))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !entry.aliases.is_empty() {
        out.push_str(&format!(
            "aliases: [{}]\n",
            entry
                .aliases
                .iter()
                .map(|a| format!("\"{}\"", a.replace('"', "\\\"")))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    out.push_str("---\n\n");
    out.push_str(entry.content.trim());
    out.push('\n');
    out
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
    for (source, edges) in incoming.edges {
        let existing = target.edges.entry(source).or_default();
        for edge in edges {
            if !existing
                .iter()
                .any(|current| current.target == edge.target && current.kind == edge.kind)
            {
                existing.push(edge);
            }
        }
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
            graph
                .reverse_edges
                .entry(edge.target.clone())
                .or_default()
                .push(source.clone());
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
    if existing.searchable_text() == candidate.searchable_text() {
        return true;
    }
    title_alias_tag_overlap(existing, candidate) >= threshold
}

/// 联合 title/alias/tag/内容 词重叠比例（§4.3）。
fn title_alias_tag_overlap(existing: &MemoryEntry, candidate: &MemoryEntry) -> f32 {
    text_overlap_ratio(&existing.searchable_text(), &candidate.searchable_text())
}

/// Lexical term-overlap ratio (0.0–1.0) over the union of both texts' terms.
fn text_overlap_ratio(lhs: &str, rhs: &str) -> f32 {
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
        return 0.0;
    }
    let shared = lhs_terms.intersection(&rhs_terms).count();
    let union = lhs_terms.union(&rhs_terms).count();
    if union == 0 {
        return 0.0;
    }
    shared as f32 / union as f32
}

fn has_text_overlap(lhs: &str, rhs: &str) -> bool {
    text_overlap_ratio(lhs, rhs) > 0.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct StaticExtractor {
        items: Vec<ExtractedMemory>,
    }

    #[async_trait]
    impl MemoryExtractor for StaticExtractor {
        async fn extract(
            &self,
            _transcript: &str,
            _existing: &[String],
        ) -> Result<Vec<ExtractedMemory>, String> {
            Ok(self.items.clone())
        }
    }

    struct StaticChecker {
        relevant: bool,
        contradictions: Vec<(String, String)>,
    }

    #[async_trait]
    impl MemoryRelevanceChecker for StaticChecker {
        async fn check_relevance(
            &self,
            _memory: &str,
            _context: &str,
        ) -> Result<(bool, String), String> {
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

        let results = mgr
            .recall(Some("Rust"), 10, RecallMode::Keyword, MemoryScope::Project)
            .unwrap();
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
        let (memories, _, _) = mgr.graph_stats().unwrap();
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
    fn test_recall_detailed_exposes_source_and_breakdown() {
        let mgr = MemoryManager::new_test();
        mgr.remember_project(MemoryEntry::new(
            MemoryCategory::Preference,
            "Prefer small direct rust style answers",
        ))
        .unwrap();

        let hits = mgr
            .recall_detailed(
                Some("rust style"),
                5,
                RecallMode::Keyword,
                MemoryScope::Project,
            )
            .unwrap();
        assert!(!hits.is_empty());
        assert!(matches!(hits[0].retrieval_source, RetrievalSource::Keyword));
        assert!(hits[0].score_breakdown.keyword_score.is_some());
        assert!(hits[0].score_breakdown.final_score > 0.0);
    }

    #[test]
    fn test_cascade_recall_surfaces_graph_hits() {
        let mgr = MemoryManager::new_test();
        let seed_id = mgr
            .remember_project(MemoryEntry::new(MemoryCategory::Fact, "rust seed"))
            .unwrap();
        let linked_id = mgr
            .remember_project(MemoryEntry::new(
                MemoryCategory::Fact,
                "related graph memory",
            ))
            .unwrap();
        mgr.link_memories(&seed_id, &linked_id, 1.0).unwrap();

        let hits = mgr
            .recall_detailed(Some("rust"), 5, RecallMode::Wiki, MemoryScope::Project)
            .unwrap();
        assert!(hits.iter().any(|hit| hit.entry.id == linked_id));
        let linked = hits.iter().find(|hit| hit.entry.id == linked_id).unwrap();
        assert!(matches!(
            linked.retrieval_source,
            RetrievalSource::CascadeGraph
        ));
        assert!(linked.score_breakdown.graph_score.is_some());
    }

    #[test]
    fn test_export_and_import_roundtrip_persists_project_and_global_graphs() {
        let project_dir = std::env::temp_dir().join(format!(
            "fox-memory-export-project-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&project_dir).unwrap();
        let mgr = MemoryManager::new_test().with_project_dir(project_dir.clone());
        mgr.remember_project(
            MemoryEntry::new(MemoryCategory::Fact, "Project memory")
                .with_tags(vec!["project".into()]),
        )
        .unwrap();
        mgr.remember_global(MemoryEntry::new(
            MemoryCategory::Preference,
            "Global memory",
        ))
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

        let imported_project_dir = std::env::temp_dir().join(format!(
            "fox-memory-import-project-{}",
            uuid::Uuid::new_v4()
        ));
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
            .remember_project(MemoryEntry::new(
                MemoryCategory::Fact,
                "Imported project memory",
            ))
            .unwrap();
        source
            .remember_global(MemoryEntry::new(
                MemoryCategory::Preference,
                "Imported global memory",
            ))
            .unwrap();
        let bundle = source.export_bundle(MemoryScope::All).unwrap();

        let target_project_dir =
            std::env::temp_dir().join(format!("fox-memory-merge-target-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&target_project_dir).unwrap();
        let target = MemoryManager::new_test().with_project_dir(target_project_dir);
        target
            .remember_project(MemoryEntry::new(
                MemoryCategory::Fact,
                "Existing project memory",
            ))
            .unwrap();
        target
            .remember_global(MemoryEntry::new(
                MemoryCategory::Preference,
                "Existing global memory",
            ))
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
        assert!(
            project_contents
                .iter()
                .any(|content| content == "Existing project memory")
        );
        assert!(
            project_contents
                .iter()
                .any(|content| content == "Imported project memory")
        );
        assert!(
            global_contents
                .iter()
                .any(|content| content == "Existing global memory")
        );
        assert!(
            global_contents
                .iter()
                .any(|content| content == "Imported global memory")
        );
    }

    #[test]
    fn test_import_bundle_replace_overwrites_existing_scope() {
        let source_project_dir = std::env::temp_dir().join(format!(
            "fox-memory-replace-source-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&source_project_dir).unwrap();
        let source = MemoryManager::new_test().with_project_dir(source_project_dir);
        source
            .remember_project(MemoryEntry::new(MemoryCategory::Fact, "Imported only"))
            .unwrap();
        let bundle = source.export_bundle(MemoryScope::Project).unwrap();

        let target_project_dir = std::env::temp_dir().join(format!(
            "fox-memory-replace-target-{}",
            uuid::Uuid::new_v4()
        ));
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
        let mgr = MemoryManager::new_test();
        let id = mgr
            .remember_project(MemoryEntry::new(
                MemoryCategory::Preference,
                "Keep answers concise",
            ))
            .unwrap();

        assert!(mgr.disable_memory(&id).unwrap());
        let disabled = mgr
            .recall(
                Some("concise"),
                5,
                RecallMode::Keyword,
                MemoryScope::Project,
            )
            .unwrap();
        assert!(disabled.is_empty());

        assert!(mgr.enable_memory(&id).unwrap());
        let enabled = mgr
            .recall(
                Some("concise"),
                5,
                RecallMode::Keyword,
                MemoryScope::Project,
            )
            .unwrap();
        assert_eq!(enabled.len(), 1);

        assert!(mgr.redact_memory(&id, "[redacted]").unwrap());
        let graph = mgr.load_project_graph().unwrap();
        let entry = graph.get_memory(&id).unwrap();
        assert_eq!(entry.content, "[redacted]");

        let audit_path = mgr.audit_log_path();
        let audit = std::fs::read_to_string(audit_path).unwrap();
        assert!(audit.contains("\"action\":\"disable\""));
        assert!(audit.contains("\"action\":\"enable\""));
        assert!(audit.contains("\"action\":\"redact\""));
    }

    #[test]
    fn test_compact_applies_retention_and_size_limit() {
        let temp_storage =
            std::env::temp_dir().join(format!("fox-memory-compact-{}", uuid::Uuid::new_v4()));
        let project_dir = std::env::temp_dir().join(format!(
            "fox-memory-compact-project-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&temp_storage).unwrap();
        std::fs::create_dir_all(&project_dir).unwrap();

        let cfg = MemoryConfig {
            retention_days: Some(1),
            ..Default::default()
        };
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
    fn test_regression_dataset_covers_keyword_and_wiki_modes() {
        let mgr = MemoryManager::new_test();
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

        let wiki_hits = mgr
            .recall_detailed(
                Some("short direct rust"),
                5,
                RecallMode::Wiki,
                MemoryScope::Project,
            )
            .unwrap();
        assert!(wiki_hits.iter().any(|hit| hit.entry.id == sibling_id));
    }

    #[tokio::test]
    async fn test_ingest_transcript_reinforces_duplicates() {
        let mgr = MemoryManager::new_test();
        let existing_id = mgr
            .remember_project(MemoryEntry::new(
                MemoryCategory::Preference,
                "Prefer concise rust",
            ))
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
        let cfg = MemoryConfig {
            contradiction_policy: ContradictionPolicy::MarkContradictionEdge,
            ..Default::default()
        };
        let mgr = MemoryManager::new(&cfg).with_storage_dir(
            std::env::temp_dir().join(format!("fox-memory-ingest-{}", uuid::Uuid::new_v4())),
        );
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
                    && edges
                        .iter()
                        .any(|edge| edge.target == report.created_ids[0] || edge.target == old_id)
            })
            .count();
        assert!(edge_count > 0);
    }

    #[tokio::test]
    async fn test_ingest_transcript_skips_irrelevant_candidates() {
        let cfg = MemoryConfig {
            verify_relevance: true,
            ..Default::default()
        };
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
        let temp =
            std::env::temp_dir().join(format!("fox-memory-session-{}", uuid::Uuid::new_v4()));
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
        let temp =
            std::env::temp_dir().join(format!("fox-memory-autopromo-{}", uuid::Uuid::new_v4()));
        let cfg = MemoryConfig {
            auto_promote_enabled: true,
            auto_promote_strength_threshold: 3,
            auto_promote_target: AutoExtractScope::Project,
            ..Default::default()
        };
        let mgr = MemoryManager::new(&cfg)
            .with_storage_dir(temp)
            .with_session_id("test-session-auto");

        let sid = mgr
            .remember_session(MemoryEntry::new(MemoryCategory::Fact, "repeated finding"))
            .unwrap();

        // strength starts at 1. Reinforce once → 2 (no promote yet).
        mgr.reinforce_memory(MemoryScope::Session, &sid).unwrap();
        assert!(
            mgr.list(MemoryScope::Session)
                .unwrap()
                .iter()
                .any(|e| e.id == sid)
        );
        assert!(mgr.list(MemoryScope::Project).unwrap().is_empty());

        // Reinforce again → 3 → auto-promoted to project.
        mgr.reinforce_memory(MemoryScope::Session, &sid).unwrap();
        assert!(
            !mgr.list(MemoryScope::Session)
                .unwrap()
                .iter()
                .any(|e| e.id == sid)
        );
        let project = mgr.list(MemoryScope::Project).unwrap();
        assert_eq!(project.len(), 1);
        assert_eq!(project[0].source.as_deref(), Some("promoted_from:session"));
    }

    // ── Phase 3: 写入管线（惰性索引重建 / 后台 enrich / LLM 判重）──

    struct StaticWikiAssistant {
        enrich_result: EnrichedMemory,
        same_result: bool,
        enrich_calls: AtomicUsize,
    }

    #[async_trait]
    impl WikiAssistant for StaticWikiAssistant {
        async fn expand_query(&self, query: &str) -> Result<QueryExpansion, String> {
            Ok(QueryExpansion::from_query(query))
        }

        async fn rerank(
            &self,
            _query: &str,
            _candidates: &[MemoryEntry],
        ) -> Result<Vec<RankedCandidate>, String> {
            Ok(Vec::new())
        }

        async fn enrich(
            &self,
            _entry: &MemoryEntry,
            _existing_titles: &[String],
        ) -> Result<EnrichedMemory, String> {
            self.enrich_calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.enrich_result.clone())
        }

        async fn are_same(&self, _a: &str, _b: &str) -> Result<bool, String> {
            Ok(self.same_result)
        }
    }

    #[test]
    fn test_rebuild_index_aggregates_active_entries_across_scopes() {
        let mgr = MemoryManager::new_test();
        let mut titled = MemoryEntry::new(MemoryCategory::Fact, "Rust ownership rules");
        titled.title = Some("Rust Ownership".to_string());
        titled.summary = Some("Borrow rules at compile time".to_string());
        titled.aliases = vec!["borrow checker".to_string()];
        titled.tags = vec!["rust".to_string()];
        let id = mgr.remember_project(titled).unwrap();

        mgr.remember_global(MemoryEntry::new(
            MemoryCategory::Preference,
            "Use spaces for indentation",
        ))
        .unwrap();

        // Inactive entries must be excluded from the index projection.
        let inactive_id = mgr
            .remember_project(MemoryEntry::new(MemoryCategory::Fact, "stale fact"))
            .unwrap();
        mgr.set_memory_active(&inactive_id, false).unwrap();

        let index = mgr.rebuild_index(MemoryScope::All).unwrap();
        assert!(index.len() >= 2);
        let entry = index.entries.iter().find(|e| e.id == id).unwrap();
        assert_eq!(entry.title.as_deref(), Some("Rust Ownership"));
        assert_eq!(
            entry.summary.as_deref(),
            Some("Borrow rules at compile time")
        );
        assert!(entry.aliases.iter().any(|a| a == "borrow checker"));
        assert!(entry.tags.iter().any(|t| t == "rust"));
        assert!(!index.entries.iter().any(|e| e.id == inactive_id));
    }

    #[tokio::test]
    async fn test_run_enrich_applies_metadata_and_is_idempotent() {
        let mgr = MemoryManager::new_test();
        let id = mgr
            .remember_project(MemoryEntry::new(
                MemoryCategory::Fact,
                "alpha-beta search algorithm",
            ))
            .unwrap();

        let assistant = Arc::new(StaticWikiAssistant {
            enrich_result: EnrichedMemory {
                title: "Alpha-Beta Search".into(),
                summary: "Tree pruning technique for two-player games".into(),
                tags: vec!["algorithm".into(), "search".into()],
                aliases: vec!["AB pruning".into()],
                link_ids: Vec::new(),
            },
            same_result: false,
            enrich_calls: AtomicUsize::new(0),
        });

        mgr.run_enrich(&id, MemoryScope::Project, assistant.clone())
            .await
            .unwrap();
        assert_eq!(assistant.enrich_calls.load(Ordering::SeqCst), 1);

        let graph = mgr.load_project_graph().unwrap();
        let entry = graph.get_memory(&id).unwrap();
        assert_eq!(entry.title.as_deref(), Some("Alpha-Beta Search"));
        assert_eq!(
            entry.summary.as_deref(),
            Some("Tree pruning technique for two-player games")
        );
        assert!(entry.tags.iter().any(|t| t == "algorithm"));
        assert!(entry.aliases.iter().any(|a| a == "AB pruning"));
        assert!(entry.enriched);

        // Idempotent: already-enriched entries skip the LLM call entirely.
        mgr.run_enrich(&id, MemoryScope::Project, assistant.clone())
            .await
            .unwrap();
        assert_eq!(assistant.enrich_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_run_enrich_links_discovered_memories() {
        let mgr = MemoryManager::new_test();
        let target = mgr
            .remember_project(MemoryEntry::new(
                MemoryCategory::Fact,
                "minimax search algorithm",
            ))
            .unwrap();
        let id = mgr
            .remember_project(MemoryEntry::new(
                MemoryCategory::Fact,
                "alpha-beta search algorithm",
            ))
            .unwrap();

        let assistant = Arc::new(StaticWikiAssistant {
            enrich_result: EnrichedMemory {
                title: "Alpha-Beta Search".into(),
                summary: "Pruning technique".into(),
                tags: Vec::new(),
                aliases: Vec::new(),
                link_ids: vec![target.clone()],
            },
            same_result: false,
            enrich_calls: AtomicUsize::new(0),
        });

        mgr.run_enrich(&id, MemoryScope::Project, assistant)
            .await
            .unwrap();

        let related = mgr.get_related(&id, 5).unwrap();
        assert!(related.iter().any(|m| m.id == target));
    }

    #[tokio::test]
    async fn test_find_llm_duplicate_returns_match_when_assistant_agrees() {
        let mgr = MemoryManager::new_test();
        let existing_id = mgr
            .remember_project(MemoryEntry::new(
                MemoryCategory::Preference,
                "Use spaces for indentation in this project",
            ))
            .unwrap();
        // Attach the assistant only after the write so no background enrich spawns.
        let mgr = mgr.with_wiki_assistant(Arc::new(StaticWikiAssistant {
            enrich_result: EnrichedMemory::default(),
            same_result: true,
            enrich_calls: AtomicUsize::new(0),
        }));

        // Paraphrase: lexical overlap below threshold — only the LLM can catch it.
        let candidate =
            MemoryEntry::new(MemoryCategory::Preference, "Use spaces when indenting code");

        let dup = mgr
            .find_llm_duplicate_for_ingestion(&candidate, MemoryScope::All)
            .await
            .unwrap();
        assert_eq!(dup, Some((MemoryScope::Project, existing_id)));
    }

    #[tokio::test]
    async fn test_find_llm_duplicate_returns_none_when_assistant_disagrees() {
        let mgr = MemoryManager::new_test();
        mgr.remember_project(MemoryEntry::new(
            MemoryCategory::Preference,
            "Use spaces for indentation in this project",
        ))
        .unwrap();
        let mgr = mgr.with_wiki_assistant(Arc::new(StaticWikiAssistant {
            enrich_result: EnrichedMemory::default(),
            same_result: false,
            enrich_calls: AtomicUsize::new(0),
        }));

        let candidate =
            MemoryEntry::new(MemoryCategory::Preference, "Use spaces when indenting code");

        let dup = mgr
            .find_llm_duplicate_for_ingestion(&candidate, MemoryScope::All)
            .await
            .unwrap();
        assert!(dup.is_none());
    }

    #[tokio::test]
    async fn test_ingest_transcript_uses_llm_dedupe_for_paraphrases() {
        let cfg = MemoryConfig {
            enrich_on_write: false,
            ..MemoryConfig::default()
        };
        let mgr = MemoryManager::new(&cfg)
            .with_storage_dir(
                std::env::temp_dir().join(format!("fox-memory-llmdedupe-{}", uuid::Uuid::new_v4())),
            )
            .with_wiki_assistant(Arc::new(StaticWikiAssistant {
                enrich_result: EnrichedMemory::default(),
                same_result: true,
                enrich_calls: AtomicUsize::new(0),
            }));

        let existing_id = mgr
            .remember_project(MemoryEntry::new(
                MemoryCategory::Preference,
                "Use spaces for indentation in this project",
            ))
            .unwrap();

        let report = mgr
            .ingest_transcript(
                "User: use spaces when indenting code",
                &StaticExtractor {
                    items: vec![ExtractedMemory {
                        category: "preference".into(),
                        content: "Use spaces when indenting code".into(),
                        trust: "high".into(),
                    }],
                },
                None,
            )
            .await
            .unwrap();

        assert_eq!(report.extracted_count, 1);
        assert!(report.created_ids.is_empty());
        assert_eq!(report.reinforced_ids, vec![existing_id.clone()]);
        assert_eq!(report.skipped_duplicates, 1);

        // Reinforced in place — no new memory, strength incremented.
        let graph = mgr.load_project_graph().unwrap();
        assert_eq!(graph.memory_count(), 1);
        assert!(graph.get_memory(&existing_id).unwrap().strength >= 2);
    }

    // ── Phase 4: 检索管线（recall_wiki / recall_wiki_async）──

    struct RecallMockAssistant {
        expansion: QueryExpansion,
        rerank_result: Vec<RankedCandidate>,
    }

    #[async_trait]
    impl WikiAssistant for RecallMockAssistant {
        async fn expand_query(&self, _query: &str) -> Result<QueryExpansion, String> {
            Ok(self.expansion.clone())
        }

        async fn rerank(
            &self,
            _query: &str,
            _candidates: &[MemoryEntry],
        ) -> Result<Vec<RankedCandidate>, String> {
            Ok(self.rerank_result.clone())
        }

        async fn enrich(
            &self,
            _entry: &MemoryEntry,
            _existing_titles: &[String],
        ) -> Result<EnrichedMemory, String> {
            Ok(EnrichedMemory::default())
        }

        async fn are_same(&self, _a: &str, _b: &str) -> Result<bool, String> {
            Ok(false)
        }
    }

    #[test]
    fn test_recall_wiki_sync_prefilter_and_cascade() {
        let mgr = MemoryManager::new_test();
        let seed_id = mgr
            .remember_project(MemoryEntry::new(
                MemoryCategory::Fact,
                "Rust ownership rules",
            ))
            .unwrap();
        let target_id = mgr
            .remember_project(MemoryEntry::new(
                MemoryCategory::Fact,
                "borrow checker concepts",
            ))
            .unwrap();
        mgr.link_memories(&seed_id, &target_id, 0.8).unwrap();

        // 未 enrich 的条目也能靠 content 命中（§5.2 content 兜底）。
        let hits = mgr
            .recall_detailed(Some("rust"), 5, RecallMode::Wiki, MemoryScope::Project)
            .unwrap();
        assert!(hits.iter().any(|h| h.entry.id == seed_id));
        // 图扩散召回无词汇重叠的邻居。
        let target = hits
            .iter()
            .find(|h| h.entry.id == target_id)
            .expect("cascade hit");
        assert_eq!(target.retrieval_source, RetrievalSource::CascadeGraph);
        assert!(hits[0].score_breakdown.keyword_score.is_some());
    }

    #[test]
    fn test_wiki_recall_matches_alias_without_literal_overlap() {
        let mgr = MemoryManager::new_test();
        // §10.1「无字面重叠但别名命中」：查询词与正文零重叠，仅命中 aliases 字段。
        let mut aliased = MemoryEntry::new(MemoryCategory::Entity, "about game tree search notes");
        aliased.aliases = vec!["alpha-beta pruning".into()];
        let aliased_id = mgr.remember_project(aliased).unwrap();
        // 无关条目（不命中）。
        mgr.remember_project(MemoryEntry::new(
            MemoryCategory::Fact,
            "cooking pasta recipes",
        ))
        .unwrap();

        // 同步 Wiki 路径（无 assistant）：词汇预筛命中别名（权重 2.0）。
        let hits = mgr
            .recall_detailed(Some("pruning"), 5, RecallMode::Wiki, MemoryScope::Project)
            .unwrap();
        let hit = hits
            .iter()
            .find(|h| h.entry.id == aliased_id)
            .expect("alias hit");
        assert_eq!(hit.retrieval_source, RetrievalSource::CascadeSeed);
        let lexical = hit.score_breakdown.keyword_score.expect("lexical score");
        assert!(lexical > 0.0 && lexical <= 1.0);
        assert!(!hits.iter().any(|h| h.entry.content.contains("pasta")));
    }

    #[tokio::test]
    async fn test_enrich_populates_title_summary_aliases_links() {
        let mgr = MemoryManager::new_test();
        let a = mgr
            .remember_project(MemoryEntry::new(
                MemoryCategory::Fact,
                "rust borrow checker",
            ))
            .unwrap();
        let b = mgr
            .remember_project(MemoryEntry::new(
                MemoryCategory::Fact,
                "rust ownership rules",
            ))
            .unwrap();
        let target = mgr
            .remember_project(MemoryEntry::new(
                MemoryCategory::Preference,
                "prefer immutable bindings",
            ))
            .unwrap();

        let assistant = Arc::new(StaticWikiAssistant {
            enrich_result: EnrichedMemory {
                title: "Rust Style Guide".into(),
                summary: "Prefer immutable, borrow-safe Rust".into(),
                tags: vec!["rust".into(), "style".into()],
                aliases: vec!["rust code style".into()],
                link_ids: vec![a.clone(), b.clone()],
            },
            same_result: false,
            enrich_calls: AtomicUsize::new(0),
        });

        // 对指定条目执行 enrich（写路径后台增强的同步等价调用）。
        mgr.run_enrich(&target, MemoryScope::Project, assistant.clone())
            .await
            .unwrap();
        assert_eq!(assistant.enrich_calls.load(Ordering::SeqCst), 1);
        let graph = mgr.load_project_graph().unwrap();
        let entry = graph.get_memory(&target).unwrap();
        assert!(entry.enriched);
        assert_eq!(entry.title.as_deref(), Some("Rust Style Guide"));
        assert_eq!(
            entry.summary.as_deref(),
            Some("Prefer immutable, borrow-safe Rust")
        );
        assert!(entry.tags.contains(&"rust".to_string()));
        assert!(entry.aliases.contains(&"rust code style".to_string()));
        // link_ids → RelatesTo 边。
        let related = mgr.get_related(&target, 5).unwrap();
        assert!(related.iter().any(|m| m.id == a));
        assert!(related.iter().any(|m| m.id == b));
    }

    #[tokio::test]
    async fn test_recall_wiki_async_uses_expansion_aliases_and_title_weighting() {
        let mgr = MemoryManager::new_test();
        // title 命中（权重 3.0）
        let mut titled = MemoryEntry::new(MemoryCategory::Entity, "unrelated body text");
        titled.title = Some("Minimax Search".into());
        let titled_id = mgr.remember_project(titled).unwrap();
        // alias 命中（权重 2.0）
        let mut aliased = MemoryEntry::new(MemoryCategory::Entity, "about game search trees");
        aliased.aliases = vec!["ab pruning".into()];
        let aliased_id = mgr.remember_project(aliased).unwrap();
        // 仅 content 命中（权重 1.0）
        let content_id = mgr
            .remember_project(MemoryEntry::new(
                MemoryCategory::Entity,
                "minimax strategy notes",
            ))
            .unwrap();
        // 完全不命中
        let pasta_id = mgr
            .remember_project(MemoryEntry::new(
                MemoryCategory::Fact,
                "cooking pasta recipes",
            ))
            .unwrap();

        let assistant = Arc::new(RecallMockAssistant {
            expansion: QueryExpansion {
                terms: vec!["minimax".into()],
                aliases: vec!["pruning".into()],
                entities: vec![],
                tags: vec![],
                natural_query: "minimax pruning".into(),
            },
            rerank_result: vec![],
        });
        let mgr = mgr.with_wiki_assistant(assistant);

        let hits = mgr
            .recall_wiki_async("minimax pruning", 10, MemoryScope::Project)
            .await
            .unwrap();
        let titled = hits
            .iter()
            .find(|h| h.entry.id == titled_id)
            .expect("title hit");
        let aliased = hits
            .iter()
            .find(|h| h.entry.id == aliased_id)
            .expect("alias hit");
        let content = hits
            .iter()
            .find(|h| h.entry.id == content_id)
            .expect("content hit");
        assert!(!hits.iter().any(|h| h.entry.id == pasta_id));

        // 权重 title(3.0) > alias(2.0) > content(1.0)。
        assert!(
            titled.score_breakdown.keyword_score.unwrap()
                > aliased.score_breakdown.keyword_score.unwrap()
        );
        assert!(
            aliased.score_breakdown.keyword_score.unwrap()
                > content.score_breakdown.keyword_score.unwrap()
        );
    }

    #[tokio::test]
    async fn test_recall_wiki_async_rerank_reorders_seeds() {
        let mgr = MemoryManager::new_test();
        // 词法上 A 命中更多 term，但 LLM 重排认为 B 更相关。
        let a_id = mgr
            .remember_project(MemoryEntry::new(
                MemoryCategory::Fact,
                "rust error handling with result",
            ))
            .unwrap();
        let b_id = mgr
            .remember_project(MemoryEntry::new(
                MemoryCategory::Fact,
                "rust panics are always bad",
            ))
            .unwrap();

        let assistant = Arc::new(RecallMockAssistant {
            expansion: QueryExpansion {
                terms: vec!["rust".into(), "error".into()],
                aliases: vec![],
                entities: vec![],
                tags: vec![],
                natural_query: "rust errors".into(),
            },
            rerank_result: vec![
                RankedCandidate {
                    id: "2".into(),
                    score: 0.9,
                    reason: "exact match".into(),
                },
                RankedCandidate {
                    id: "1".into(),
                    score: 0.3,
                    reason: "partial".into(),
                },
            ],
        });
        let mgr = mgr.with_wiki_assistant(assistant);

        let hits = mgr
            .recall_wiki_async("rust errors", 5, MemoryScope::Project)
            .await
            .unwrap();
        assert_eq!(
            hits.first().map(|h| h.entry.id.as_str()),
            Some(b_id.as_str())
        );
        assert!(hits.iter().any(|h| h.entry.id == a_id));
        // 重排分也反映在种子分项中（keyword_score 保留词汇分）。
        assert!(hits[0].score_breakdown.keyword_score.is_some());
    }

    #[tokio::test]
    async fn test_recall_wiki_async_falls_back_without_assistant() {
        let mgr = MemoryManager::new_test();
        mgr.remember_project(MemoryEntry::new(
            MemoryCategory::Fact,
            "rust ownership rules",
        ))
        .unwrap();
        mgr.remember_project(MemoryEntry::new(MemoryCategory::Fact, "python walkthrough"))
            .unwrap();

        // 无 assistant 时退化为纯词汇路径，结果非空且都是种子命中。
        let hits = mgr
            .recall_wiki_async("rust", 5, MemoryScope::Project)
            .await
            .unwrap();
        assert!(!hits.is_empty());
        assert!(
            hits.iter()
                .all(|h| h.retrieval_source == RetrievalSource::CascadeSeed)
        );
        assert!(hits.iter().all(|h| h.entry.content.contains("rust")));
    }

    // ── Phase 5: 索引持久化 / 批量补增强 / wiki 导出 ──

    #[test]
    fn test_persist_index_writes_graph_index_json() {
        let mgr = MemoryManager::new_test();
        let mut titled = MemoryEntry::new(MemoryCategory::Fact, "Rust ownership rules");
        titled.title = Some("Rust Ownership".into());
        titled.summary = Some("Borrow rules at compile time".into());
        mgr.remember_project(titled).unwrap();
        mgr.remember_global(MemoryEntry::new(MemoryCategory::Preference, "Use spaces"))
            .unwrap();

        let combined = mgr.persist_index(MemoryScope::All).unwrap();
        assert!(combined.len() >= 2);

        // 每个 graph 旁的 {graph}.index.json 只包含该图自己的条目。
        let project_index_path = mgr
            .project_memory_path()
            .unwrap()
            .with_extension("index.json");
        let project_index: MemoryIndex =
            serde_json::from_str(&std::fs::read_to_string(&project_index_path).unwrap()).unwrap();
        assert_eq!(project_index.len(), 1);
        assert_eq!(
            project_index.entries[0].title.as_deref(),
            Some("Rust Ownership")
        );

        let global_index_path = mgr.global_memory_path().with_extension("index.json");
        let global_index: MemoryIndex =
            serde_json::from_str(&std::fs::read_to_string(&global_index_path).unwrap()).unwrap();
        assert_eq!(global_index.len(), 1);

        // 持久化索引可反序列化，且 load_index 仍返回最新图投影。
        let loaded = mgr.load_index(MemoryScope::All).unwrap();
        assert_eq!(loaded.len(), combined.len());
    }

    #[test]
    fn test_export_wiki_writes_index_and_pages_with_unique_slugs() {
        let mgr = MemoryManager::new_test();
        let base = chrono::Utc::now();
        let mut a = MemoryEntry::new(MemoryCategory::Fact, "ownership content");
        a.created_at = base;
        a.title = Some("Rust Errors".into());
        a.summary = Some("Handle errors with Result".into());
        a.tags = vec!["rust".into()];
        a.aliases = vec!["errors".into()];
        let id_a = mgr.remember_project(a).unwrap();

        // 重复标题 → slug 追加 -2（created_at 决定谁获得基础 slug）。
        let mut b = MemoryEntry::new(MemoryCategory::Fact, "second content");
        b.created_at = base + chrono::Duration::seconds(1);
        b.title = Some("Rust Errors".into());
        let id_b = mgr.remember_project(b).unwrap();

        // 无 title 条目 → 用 id 作 slug。
        let mut c = MemoryEntry::new(MemoryCategory::Fact, "bare content no title");
        c.created_at = base + chrono::Duration::seconds(2);
        mgr.remember_project(c).unwrap();

        let dir = std::env::temp_dir().join(format!("fox-memory-wiki-{}", uuid::Uuid::new_v4()));
        let stats = mgr.export_wiki(MemoryScope::Project, &dir).unwrap();
        assert_eq!(stats.pages_written, 3);
        assert_eq!(stats.memories, 3);

        let index_md = std::fs::read_to_string(dir.join("index.md")).unwrap();
        assert!(index_md.contains("[Rust Errors](pages/rust-errors.md)"));
        assert!(index_md.contains("[Rust Errors](pages/rust-errors-2.md)"));
        assert!(index_md.contains("— Handle errors with Result"));

        // pages/ 文件与 frontmatter。
        assert!(dir.join("pages/rust-errors.md").exists());
        assert!(dir.join("pages/rust-errors-2.md").exists());
        let page = std::fs::read_to_string(dir.join("pages/rust-errors.md")).unwrap();
        assert!(page.contains("title: \"Rust Errors\""));
        assert!(page.contains("tags: [\"rust\"]"));
        assert!(page.contains("aliases: [\"errors\"]"));
        assert!(page.contains("ownership content"));
        let _ = (id_a, id_b);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_backfill_enrich_enhances_unenriched_entries() {
        let mgr = MemoryManager::new_test();
        let id1 = mgr
            .remember_project(MemoryEntry::new(MemoryCategory::Fact, "alpha-beta search"))
            .unwrap();
        let id2 = mgr
            .remember_project(MemoryEntry::new(MemoryCategory::Fact, "minimax search"))
            .unwrap();

        let assistant = Arc::new(StaticWikiAssistant {
            enrich_result: EnrichedMemory {
                title: "Search Algorithm".into(),
                summary: "Tree search technique".into(),
                tags: vec!["algorithm".into()],
                aliases: Vec::new(),
                link_ids: Vec::new(),
            },
            same_result: false,
            enrich_calls: AtomicUsize::new(0),
        });
        // 写路径之后才装配 assistant，避免 remember 触发后台 enrich。
        let mgr = mgr.with_wiki_assistant(assistant.clone());

        let enriched = mgr.backfill_enrich(MemoryScope::Project, 0).await.unwrap();
        assert_eq!(enriched, 2);
        assert_eq!(assistant.enrich_calls.load(Ordering::SeqCst), 2);

        let graph = mgr.load_project_graph().unwrap();
        assert!(graph.get_memory(&id1).unwrap().enriched);
        assert!(graph.get_memory(&id2).unwrap().enriched);
        assert_eq!(
            graph.get_memory(&id1).unwrap().title.as_deref(),
            Some("Search Algorithm")
        );

        // 幂等：二次 backfill 不再触发 LLM。
        let again = mgr.backfill_enrich(MemoryScope::Project, 0).await.unwrap();
        assert_eq!(again, 0);
        assert_eq!(assistant.enrich_calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn test_index_to_prompt_respects_budget() {
        let mgr = MemoryManager::new_test();
        let mut titled = MemoryEntry::new(MemoryCategory::Fact, "content");
        titled.title = Some("Rust Errors".into());
        titled.summary = Some("Handle errors with Result".into());
        mgr.remember_project(titled).unwrap();

        let prompt = mgr.index_to_prompt(MemoryScope::Project, 10_000);
        let prompt = prompt.expect("index prompt should render");
        assert!(prompt.contains("Rust Errors"));
        assert!(prompt.contains("Handle errors with Result"));

        // 预算过小 → None。
        assert!(mgr.index_to_prompt(MemoryScope::Project, 4).is_none());
    }
}
