//! Graph-based memory storage with tags, clusters, and semantic links.
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
    InCluster,
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
            EdgeKind::InCluster => 0.6,
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
        Self { target: target.into(), kind }
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

/// A cluster node (auto-discovered grouping via embeddings).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterEntry {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub centroid: Vec<f32>,
    pub member_count: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ClusterEntry {
    pub fn new(id: impl Into<String>) -> Self {
        let id = id.into();
        let now = Utc::now();
        Self {
            id: format!("cluster:{id}"),
            name: None,
            centroid: Vec::new(),
            member_count: 0,
            created_at: now,
            updated_at: now,
        }
    }
}

/// Graph metadata for tracking statistics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GraphMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_cluster_update: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_embedding_rebuild_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_version: Option<String>,
    #[serde(default)]
    pub total_embeddings: u64,
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
    pub clusters: HashMap<String, ClusterEntry>,
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
            clusters: HashMap::new(),
            edges: HashMap::new(),
            reverse_edges: HashMap::new(),
            metadata: GraphMetadata::default(),
        }
    }

    // ── Counts ──

    pub fn memory_count(&self) -> usize { self.memories.len() }
    pub fn node_count(&self) -> usize { self.memories.len() + self.tags.len() + self.clusters.len() }
    pub fn edge_count(&self) -> usize { self.edges.values().map(|v| v.len()).sum() }

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

    pub fn cluster_count(&self) -> usize {
        self.clusters.len()
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
            && edges.iter().any(|e| e.target == tid && matches!(e.kind, EdgeKind::HasTag))
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
        self.reverse_edges.get(&tid).map(|sources| {
            sources.iter().filter_map(|id| self.memories.get(id)).collect()
        }).unwrap_or_default()
    }

    pub fn all_tags(&self) -> impl Iterator<Item = &TagEntry> { self.tags.values() }

    // ── Edges ──

    fn add_edge_internal(&mut self, from: &str, to: &str, kind: EdgeKind) {
        self.edges.entry(from.into()).or_default().push(Edge::new(to, kind));
        self.reverse_edges.entry(to.into()).or_default().push(from.into());
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
        self.reverse_edges.get(id).map(|v| v.iter().map(|s| s.as_str()).collect()).unwrap_or_default()
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

    pub fn refresh_clusters(
        &mut self,
        similarity_threshold: f32,
        min_members: usize,
    ) -> usize {
        self.clear_clusters();

        let mut candidates: Vec<(String, Vec<f32>)> = self
            .active_memories()
            .filter_map(|entry| {
                entry
                    .embedding
                    .as_ref()
                    .filter(|embedding| !embedding.is_empty())
                    .map(|embedding| (entry.id.clone(), embedding.clone()))
            })
            .collect();
        candidates.sort_by(|a, b| a.0.cmp(&b.0));

        let mut groups: Vec<TempCluster> = Vec::new();
        for (memory_id, embedding) in candidates {
            let mut best_idx = None;
            let mut best_score = 0.0f32;
            for (idx, group) in groups.iter().enumerate() {
                let Some(score) = cosine_similarity(&group.centroid, &embedding) else {
                    continue;
                };
                if score >= similarity_threshold && score > best_score {
                    best_score = score;
                    best_idx = Some(idx);
                }
            }

            if let Some(idx) = best_idx {
                groups[idx].add_member(memory_id, embedding);
            } else {
                groups.push(TempCluster::new(memory_id, embedding));
            }
        }

        let now = Utc::now();
        let mut retained = 0usize;
        for (idx, group) in groups.into_iter().enumerate() {
            if group.members.len() < min_members.max(1) || group.centroid.is_empty() {
                continue;
            }
            retained += 1;
            let cluster_id = format!("cluster:auto-{}", idx + 1);
            self.clusters.insert(
                cluster_id.clone(),
                ClusterEntry {
                    id: cluster_id.clone(),
                    name: Some(format!("Auto Cluster {}", idx + 1)),
                    centroid: group.centroid.clone(),
                    member_count: group.members.len() as u32,
                    created_at: now,
                    updated_at: now,
                },
            );
            for member_id in group.members {
                self.add_edge(&member_id, &cluster_id, EdgeKind::InCluster);
            }
        }
        self.metadata.last_cluster_update = Some(now);
        retained
    }

    pub fn clear_clusters(&mut self) {
        let cluster_ids: HashSet<String> = self.clusters.keys().cloned().collect();
        if cluster_ids.is_empty() {
            self.metadata.last_cluster_update = Some(Utc::now());
            return;
        }

        self.clusters.clear();
        self.reverse_edges
            .retain(|target, _| !cluster_ids.contains(target));

        let mut empty_sources = Vec::new();
        for (source, edges) in &mut self.edges {
            edges.retain(|edge| {
                !(matches!(edge.kind, EdgeKind::InCluster) || cluster_ids.contains(&edge.target))
            });
            if edges.is_empty() {
                empty_sources.push(source.clone());
            }
        }
        for source in empty_sources {
            self.edges.remove(&source);
        }
        self.metadata.last_cluster_update = Some(Utc::now());
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

                if target.starts_with("tag:") || target.starts_with("cluster:") {
                    let propagated = if target.starts_with("cluster:") {
                        (new_score * 1.1).min(1.0)
                    } else {
                        new_score
                    };
                    for src_id in self.get_incoming(target) {
                        let src_id = src_id.to_string();
                        if !visited.contains(&src_id) && self.memories.contains_key(&src_id) {
                            let existing = results.get(&src_id).copied().unwrap_or(0.0);
                            if propagated > existing {
                                results.insert(src_id.clone(), propagated);
                                queue.push_back((src_id, propagated, depth + 1));
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

struct TempCluster {
    members: Vec<String>,
    centroid: Vec<f32>,
}

impl TempCluster {
    fn new(memory_id: String, embedding: Vec<f32>) -> Self {
        Self {
            members: vec![memory_id],
            centroid: embedding,
        }
    }

    fn add_member(&mut self, memory_id: String, embedding: Vec<f32>) {
        let existing_members = self.members.len() as f32;
        for (idx, value) in embedding.iter().enumerate() {
            if let Some(centroid) = self.centroid.get_mut(idx) {
                *centroid = ((*centroid * existing_members) + *value) / (existing_members + 1.0);
            }
        }
        self.members.push(memory_id);
    }
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
