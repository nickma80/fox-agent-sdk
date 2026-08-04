//! Graph-based memory storage with tags, links, and wiki metadata.
//! MemoryGraph v2 — HashMap-based for clean JSON serialization.

use crate::memory::ranking::top_k_by_score;
use crate::memory::types::MemoryEntry;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

/// Current graph format version.
pub const GRAPH_VERSION: u32 = 2;

/// Edge relationship types between nodes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EdgeKind {
    HasTag,
    RelatesTo {
        #[serde(default = "default_weight")]
        weight: f32,
    },
    Supersedes,
    Contradicts,
    DerivedFrom,
}

fn default_weight() -> f32 {
    1.0
}

impl EdgeKind {
    pub fn traversal_weight(&self) -> f32 {
        match self {
            EdgeKind::HasTag => 0.8,
            EdgeKind::RelatesTo { weight } => *weight,
            EdgeKind::Supersedes => 0.9,
            EdgeKind::Contradicts => 0.3,
            EdgeKind::DerivedFrom => 0.7,
        }
    }
}

/// An edge in the memory graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub target: String,
    #[serde(flatten)]
    pub kind: EdgeKind,
}

impl Edge {
    pub fn new(target: impl Into<String>, kind: EdgeKind) -> Self {
        Self {
            target: target.into(),
            kind,
        }
    }
}

/// A tag node in the graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagEntry {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub count: u32,
    pub created_at: DateTime<Utc>,
}

impl TagEntry {
    pub fn new(name: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            id: format!("tag:{name}"),
            name,
            description: None,
            count: 0,
            created_at: Utc::now(),
        }
    }
}

/// Graph metadata for tracking statistics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GraphMetadata {
    #[serde(default)]
    pub retrieval_count: u64,
    #[serde(default)]
    pub link_discovery_count: u64,
}

/// The memory graph — HashMap-based for clean JSON serialization. v2.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryGraph {
    pub graph_version: u32,
    pub memories: HashMap<String, MemoryEntry>,
    #[serde(default)]
    pub tags: HashMap<String, TagEntry>,
    #[serde(default)]
    pub edges: HashMap<String, Vec<Edge>>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub reverse_edges: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub metadata: GraphMetadata,
}

impl Default for MemoryGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryGraph {
    pub fn new() -> Self {
        Self {
            graph_version: GRAPH_VERSION,
            memories: HashMap::new(),
            tags: HashMap::new(),
            edges: HashMap::new(),
            reverse_edges: HashMap::new(),
            metadata: GraphMetadata::default(),
        }
    }

    // ── Counts ──

    pub fn memory_count(&self) -> usize {
        self.memories.len()
    }
    pub fn node_count(&self) -> usize {
        self.memories.len() + self.tags.len()
    }
    pub fn edge_count(&self) -> usize {
        self.edges.values().map(|v| v.len()).sum()
    }

    // ── Memory CRUD ──

    pub fn add_memory(&mut self, mut entry: MemoryEntry) -> String {
        entry.refresh_search_text();
        let id = entry.id.clone();
        for tag_name in &entry.tags {
            self.ensure_tag(tag_name);
            let tid = format!("tag:{tag_name}");
            self.add_edge_internal(&id, &tid, EdgeKind::HasTag);
            if let Some(tag) = self.tags.get_mut(&tid) {
                tag.count += 1;
            }
        }
        if let Some(ref sup_id) = entry.superseded_by {
            self.add_edge_internal(sup_id, &id, EdgeKind::Supersedes);
        }
        self.memories.insert(id.clone(), entry);
        id
    }

    pub fn get_memory(&self, id: &str) -> Option<&MemoryEntry> {
        self.memories.get(id)
    }

    pub fn get_memory_mut(&mut self, id: &str) -> Option<&mut MemoryEntry> {
        self.memories.get_mut(id)
    }

    pub fn remove_memory(&mut self, id: &str) -> Option<MemoryEntry> {
        if let Some(edges) = self.edges.remove(id) {
            for edge in &edges {
                if let Some(reverse) = self.reverse_edges.get_mut(&edge.target) {
                    reverse.retain(|s| s != id);
                }
                if matches!(edge.kind, EdgeKind::HasTag)
                    && let Some(tag) = self.tags.get_mut(&edge.target)
                {
                    tag.count = tag.count.saturating_sub(1);
                }
            }
        }
        if let Some(sources) = self.reverse_edges.remove(id) {
            for src in sources {
                if let Some(edges) = self.edges.get_mut(&src) {
                    edges.retain(|e| e.target != id);
                }
            }
        }
        self.memories.remove(id)
    }

    pub fn all_memories(&self) -> impl Iterator<Item = &MemoryEntry> {
        self.memories.values()
    }

    pub fn active_memories(&self) -> impl Iterator<Item = &MemoryEntry> {
        self.memories.values().filter(|m| m.active)
    }

    // ── Tags ──

    pub fn ensure_tag(&mut self, name: &str) -> &TagEntry {
        let tid = format!("tag:{name}");
        self.tags.entry(tid).or_insert_with(|| TagEntry::new(name))
    }

    pub fn tag_memory(&mut self, memory_id: &str, tag_name: &str) {
        self.ensure_tag(tag_name);
        let tid = format!("tag:{tag_name}");
        if let Some(edges) = self.edges.get(memory_id)
            && edges
                .iter()
                .any(|e| e.target == tid && matches!(e.kind, EdgeKind::HasTag))
        {
            return;
        }
        self.add_edge_internal(memory_id, &tid, EdgeKind::HasTag);
        if let Some(tag) = self.tags.get_mut(&tid) {
            tag.count += 1;
        }
        if let Some(mem) = self.memories.get_mut(memory_id)
            && !mem.tags.contains(&tag_name.to_string())
        {
            mem.tags.push(tag_name.to_string());
            mem.refresh_search_text();
        }
    }

    pub fn untag_memory(&mut self, memory_id: &str, tag_name: &str) {
        let tid = format!("tag:{tag_name}");
        if let Some(edges) = self.edges.get_mut(memory_id) {
            edges.retain(|e| !(e.target == tid && matches!(e.kind, EdgeKind::HasTag)));
        }
        if let Some(sources) = self.reverse_edges.get_mut(&tid) {
            sources.retain(|s| s != memory_id);
        }
        if let Some(tag) = self.tags.get_mut(&tid) {
            tag.count = tag.count.saturating_sub(1);
        }
        if let Some(mem) = self.memories.get_mut(memory_id) {
            mem.tags.retain(|t| t != tag_name);
            mem.refresh_search_text();
        }
    }

    pub fn get_memories_by_tag(&self, tag_name: &str) -> Vec<&MemoryEntry> {
        let tid = format!("tag:{tag_name}");
        self.reverse_edges
            .get(&tid)
            .map(|sources| {
                sources
                    .iter()
                    .filter_map(|id| self.memories.get(id))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn all_tags(&self) -> impl Iterator<Item = &TagEntry> {
        self.tags.values()
    }

    // ── Edges ──

    fn add_edge_internal(&mut self, from: &str, to: &str, kind: EdgeKind) {
        self.edges
            .entry(from.into())
            .or_default()
            .push(Edge::new(to, kind));
        self.reverse_edges
            .entry(to.into())
            .or_default()
            .push(from.into());
    }

    pub fn add_edge(&mut self, from: &str, to: &str, kind: EdgeKind) {
        if let Some(edges) = self.edges.get(from)
            && edges.iter().any(|e| e.target == to && e.kind == kind)
        {
            return;
        }
        self.add_edge_internal(from, to, kind);
    }

    pub fn remove_edge(&mut self, from: &str, to: &str, kind: &EdgeKind) {
        if let Some(edges) = self.edges.get_mut(from) {
            edges.retain(|e| !(e.target == to && &e.kind == kind));
        }
        if let Some(sources) = self.reverse_edges.get_mut(to) {
            sources.retain(|s| s != from);
        }
    }

    pub fn get_edges(&self, id: &str) -> &[Edge] {
        self.edges.get(id).map(|v| v.as_slice()).unwrap_or(&[])
    }

    pub fn get_incoming(&self, id: &str) -> Vec<&str> {
        self.reverse_edges
            .get(id)
            .map(|v| v.iter().map(|s| s.as_str()).collect())
            .unwrap_or_default()
    }

    pub fn link_memories(&mut self, from: &str, to: &str, weight: f32) {
        self.add_edge(from, to, EdgeKind::RelatesTo { weight });
        self.metadata.link_discovery_count += 1;
    }

    pub fn supersede(&mut self, newer_id: &str, older_id: &str) {
        self.add_edge(newer_id, older_id, EdgeKind::Supersedes);
        if let Some(older) = self.memories.get_mut(older_id) {
            older.active = false;
            older.superseded_by = Some(newer_id.into());
        }
    }

    pub fn mark_contradiction(&mut self, id_a: &str, id_b: &str) {
        self.add_edge(id_a, id_b, EdgeKind::Contradicts);
        self.add_edge(id_b, id_a, EdgeKind::Contradicts);
    }

    // ── Cascade (BFS) Retrieval ──

    /// BFS cascade retrieval starting from seed memories.
    /// Traverses through tags and edges to find related memories.
    pub fn cascade_retrieve(
        &self,
        seed_ids: &[String],
        seed_scores: &[f32],
        max_depth: usize,
        max_results: usize,
    ) -> Vec<(String, f32)> {
        let mut visited: HashSet<String> = HashSet::new();
        let mut results: HashMap<String, f32> = HashMap::new();
        let mut queue: VecDeque<(String, f32, usize)> = VecDeque::new();
        for (id, score) in seed_ids.iter().zip(seed_scores.iter()) {
            if self.memories.contains_key(id) {
                queue.push_back((id.clone(), *score, 0));
                results.insert(id.clone(), *score);
            }
        }
        while let Some((node_id, score, depth)) = queue.pop_front() {
            if !visited.insert(node_id.clone()) {
                continue;
            }
            if depth >= max_depth {
                continue;
            }
            for edge in self.get_edges(&node_id).to_vec() {
                let target = &edge.target;
                if visited.contains(target) {
                    continue;
                }
                let edge_weight = edge.kind.traversal_weight();
                let decay = 0.7_f32.powi(depth as i32 + 1);
                let new_score = score * edge_weight * decay;

                if target.starts_with("tag:") {
                    for src_id in self.get_incoming(target) {
                        let src_id = src_id.to_string();
                        if !visited.contains(&src_id) && self.memories.contains_key(&src_id) {
                            let existing = results.get(&src_id).copied().unwrap_or(0.0);
                            if new_score > existing {
                                results.insert(src_id.clone(), new_score);
                                queue.push_back((src_id, new_score, depth + 1));
                            }
                        }
                    }
                } else if self.memories.contains_key(target) {
                    let existing = results.get(target).copied().unwrap_or(0.0);
                    if new_score > existing {
                        results.insert(target.clone(), new_score);
                        queue.push_back((target.clone(), new_score, depth + 1));
                    }
                }
            }
        }
        top_k_by_score(results, max_results)
    }
}
