//! Memory system for cross-session learning.
//!
//! Provides persistent memory across sessions, organized by:
//! - Project (per working directory)
//! - Global (user-level preferences)
//!
//! Storage uses MemoryGraph v2 format with JSON files,
//! LRU caching, and automatic backup recovery.

pub mod graph;
pub mod prompt;
pub mod ranking;
pub mod relevance;
pub mod storage;
pub mod types;

pub use graph::{ClusterEntry, Edge, EdgeKind, GRAPH_VERSION, GraphMetadata, MemoryGraph, TagEntry};
pub use relevance::{ExtractedMemory, MemoryExtractor, MemoryRelevanceChecker};
pub use storage::{GCResult, MemoryGraphCache, cache_graph, cached_graph, default_storage_dir, gc_memory_files, invalidate_cache, project_hash, read_json, write_json};
pub use types::{
    MemoryCategory, MemoryEntry, MemoryScope, RecallMode, Reinforcement, TrustLevel,
    memory_matches_search, memory_score, normalize_memory_search_text, normalize_search_text,
};

use crate::config::MemoryConfig;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Events emitted by the memory pipeline.
#[derive(Clone, Debug)]
pub enum MemoryStateEvent {
    InjectionComputed { count: u32, memory_ids: Vec<String>, prompt_chars: usize },
    InjectionConsumed { count: u32, memory_ids: Vec<String>, prompt_chars: usize },
    Enabled,
    Disabled,
}

/// Memory manager for cross-session learning.
#[derive(Clone)]
pub struct MemoryManager {
    storage_dir: PathBuf,
    project_dir: Option<PathBuf>,
    test_mode: bool,
}

impl MemoryManager {
    /// Create a new MemoryManager from config.
    pub fn new(config: &MemoryConfig) -> Self {
        let storage_dir = config.storage_dir.clone()
            .unwrap_or_else(default_storage_dir);
        Self {
            storage_dir,
            project_dir: None,
            test_mode: false,
        }
    }

    /// Set the project directory (for scoping project memories).
    pub fn with_project_dir(mut self, dir: PathBuf) -> Self {
        self.project_dir = Some(dir);
        self
    }

    /// Create in test mode (uses temp directory).
    pub fn new_test() -> Self {
        let temp = std::env::temp_dir().join(format!("fox-memory-test-{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&temp);
        Self {
            storage_dir: temp,
            project_dir: None,
            test_mode: true,
        }
    }

    pub fn is_test_mode(&self) -> bool { self.test_mode }

    pub fn with_storage_dir(mut self, dir: PathBuf) -> Self {
        self.storage_dir = dir;
        self
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

    // ── CRUD: remember ──

    /// Store a memory in the project scope.
    pub fn remember_project(&self, entry: MemoryEntry) -> Result<String, String> {
        let mut graph = self.load_project_graph()?;
        let id = graph.add_memory(entry);
        self.save_project_graph(&graph)?;
        Ok(id)
    }

    /// Store a memory in the global scope.
    pub fn remember_global(&self, entry: MemoryEntry) -> Result<String, String> {
        let mut graph = self.load_global_graph()?;
        let id = graph.add_memory(entry);
        self.save_global_graph(&graph)?;
        Ok(id)
    }

    /// Store a memory in the appropriate scope.
    pub fn remember(&self, entry: MemoryEntry, scope: MemoryScope) -> Result<String, String> {
        match scope {
            MemoryScope::Project | MemoryScope::All => self.remember_project(entry),
            MemoryScope::Global => self.remember_global(entry),
        }
    }

    // ── CRUD: recall ──

    /// Recall memories. Mode controls retrieval strategy.
    pub fn recall(&self, query: Option<&str>, limit: usize, mode: RecallMode, scope: MemoryScope) -> Result<Vec<(MemoryEntry, f32)>, String> {
        match mode {
            RecallMode::Recent => self.recall_recent(limit, scope),
            RecallMode::Keyword => {
                let q = query.unwrap_or("");
                if q.is_empty() { return Ok(Vec::new()); }
                self.recall_keyword(q, limit, scope)
            }
            RecallMode::Semantic => {
                // Without embedding feature, fall back to keyword
                let q = query.unwrap_or("");
                if q.is_empty() { return Ok(Vec::new()); }
                self.recall_keyword(q, limit, scope)
            }
            RecallMode::Cascade => {
                let q = query.unwrap_or("");
                if q.is_empty() { return Ok(Vec::new()); }
                // Fall back to keyword + cascade
                self.recall_cascade(q, limit, scope)
            }
        }
    }

    fn recall_recent(&self, limit: usize, scope: MemoryScope) -> Result<Vec<(MemoryEntry, f32)>, String> {
        let all = self.collect_memories(scope)?;
        let scored: Vec<(MemoryEntry, f32)> = all.into_iter()
            .filter(|e| e.active)
            .map(|e| {
                let score = memory_score(&e) as f32;
                (e, score)
            })
            .collect();
        Ok(ranking::top_k_by_score(scored, limit))
    }

    fn recall_keyword(&self, query: &str, limit: usize, scope: MemoryScope) -> Result<Vec<(MemoryEntry, f32)>, String> {
        let nq = normalize_search_text(query);
        if nq.is_empty() { return Ok(Vec::new()); }
        let all = self.collect_memories(scope)?;
        let matches: Vec<(MemoryEntry, f32)> = all.into_iter()
            .filter(|e| e.active && memory_matches_search(e, &nq))
            .map(|e| {
                let score = memory_score(&e) as f32;
                (e, score)
            })
            .collect();
        Ok(ranking::top_k_by_score(matches, limit))
    }

    fn recall_cascade(&self, query: &str, limit: usize, scope: MemoryScope) -> Result<Vec<(MemoryEntry, f32)>, String> {
        // Start with keyword search
        let hits = self.recall_keyword(query, limit * 2, scope)?;
        if hits.is_empty() { return Ok(Vec::new()); }

        let seed_ids: Vec<String> = hits.iter().map(|(e, _)| e.id.clone()).collect();
        let seed_scores: Vec<f32> = hits.iter().map(|(_, s)| *s).collect();

        // Cascade through both graphs
        let mut merged: HashMap<String, f32> = HashMap::new();
        for (e, s) in &hits { merged.insert(e.id.clone(), *s); }

        if scope.includes_project() {
            if let Ok(graph) = self.load_project_graph() {
                let cascaded = graph.cascade_retrieve(&seed_ids, &seed_scores, 2, limit * 2);
                for (id, s) in cascaded {
                    let existing = merged.get(&id).copied().unwrap_or(0.0);
                    if s > existing { merged.insert(id, s); }
                }
            }
        }
        if scope.includes_global() {
            if let Ok(graph) = self.load_global_graph() {
                let cascaded = graph.cascade_retrieve(&seed_ids, &seed_scores, 2, limit * 2);
                for (id, s) in cascaded {
                    let existing = merged.get(&id).copied().unwrap_or(0.0);
                    if s > existing { merged.insert(id, s); }
                }
            }
        }

        // Look up entries for ids
        let mut results = Vec::new();
        for (id, score) in merged {
            if let Ok(graph) = self.load_project_graph() {
                if let Some(e) = graph.get_memory(&id) {
                    results.push((e.clone(), score));
                    continue;
                }
            }
            if let Ok(graph) = self.load_global_graph() {
                if let Some(e) = graph.get_memory(&id) {
                    results.push((e.clone(), score));
                }
            }
        }

        Ok(ranking::top_k_by_score(results, limit))
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
            return self.save_project_graph(&project).map(|_| true);
        }
        // Try global
        let mut global = self.load_global_graph()?;
        if global.remove_memory(id).is_some() {
            return self.save_global_graph(&global).map(|_| true);
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

    // ── Helpers ──

    fn collect_memories(&self, scope: MemoryScope) -> Result<Vec<MemoryEntry>, String> {
        let mut all = Vec::new();
        if scope.includes_project() {
            if let Ok(graph) = self.load_project_graph() {
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
