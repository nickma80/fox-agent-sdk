use crate::config::MemoryConfig;
use crate::memory::graph::MemoryGraph;
use crate::memory::types::MemoryEntry;
use bincode;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use vectorlite::index::hnsw::HNSWIndex;
use vectorlite::{SimilarityMetric, Vector, VectorIndex};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AnnSnapshot {
    built_at: DateTime<Utc>,
    dim: usize,
    embedding_model: Option<String>,
    embedding_version: Option<String>,
    index: HNSWIndex,
    id_to_memory_id: HashMap<u64, String>,
}

#[derive(Debug)]
pub struct AnnSearchHit {
    pub memory_id: String,
    #[expect(dead_code)]
    pub approx_score: f64,
}

#[derive(Debug)]
pub struct AnnStats {
    pub vectors_indexed: usize,
    #[expect(dead_code)]
    pub dim: usize,
    #[expect(dead_code)]
    pub built_at: DateTime<Utc>,
}

static ANN_CACHE: OnceLock<Mutex<HashMap<PathBuf, AnnSnapshot>>> = OnceLock::new();

fn ann_cache() -> &'static Mutex<HashMap<PathBuf, AnnSnapshot>> {
    ANN_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn ann_index_path(graph_path: &Path) -> PathBuf {
    graph_path.with_extension("ann.bin")
}

pub fn invalidate_ann_index(graph_path: &Path) {
    let ann_path = ann_index_path(graph_path);
    let _ = std::fs::remove_file(&ann_path);
    let _ = std::fs::remove_file(tmp_path(&ann_path));
    if let Ok(mut cache) = ann_cache().lock() {
        cache.remove(&ann_path);
    }
}

pub fn rebuild_ann_index(
    graph_path: &Path,
    graph: &MemoryGraph,
    expected_embedding_model: Option<&str>,
    expected_embedding_version: Option<&str>,
) -> Result<AnnStats, String> {
    let ann_path = ann_index_path(graph_path);
    let snapshot = build_snapshot(graph, expected_embedding_model, expected_embedding_version)?;
    persist_snapshot(&ann_path, &snapshot)?;
    if let Ok(mut cache) = ann_cache().lock() {
        cache.insert(ann_path.clone(), snapshot.clone());
    }
    Ok(AnnStats {
        vectors_indexed: snapshot.id_to_memory_id.len(),
        dim: snapshot.dim,
        built_at: snapshot.built_at,
    })
}

pub fn ann_search_candidates(
    cfg: &MemoryConfig,
    graph_path: &Path,
    graph: &MemoryGraph,
    query_embedding: &[f32],
    k: usize,
    expected_embedding_model: Option<&str>,
    expected_embedding_version: Option<&str>,
) -> Result<Vec<AnnSearchHit>, String> {
    if !cfg.ann_enabled {
        return Ok(Vec::new());
    }
    let Some(dim) = query_embedding.first().map(|_| query_embedding.len()) else {
        return Ok(Vec::new());
    };
    if dim == 0 {
        return Ok(Vec::new());
    }

    let ann_path = ann_index_path(graph_path);
    let mut snapshot = load_snapshot_cached(&ann_path)?;
    if snapshot.is_none() {
        snapshot = Some(build_snapshot(
            graph,
            expected_embedding_model,
            expected_embedding_version,
        )?);
        if let Some(s) = &snapshot {
            persist_snapshot(&ann_path, s)?;
            if let Ok(mut cache) = ann_cache().lock() {
                cache.insert(ann_path.clone(), s.clone());
            }
        }
    }
    let Some(snapshot) = snapshot else {
        return Ok(Vec::new());
    };
    if snapshot.dim != dim {
        return Ok(Vec::new());
    }
    if snapshot.id_to_memory_id.len() < cfg.ann_min_vectors {
        return Ok(Vec::new());
    }

    let query: Vec<f64> = query_embedding.iter().map(|v| *v as f64).collect();
    let results = snapshot
        .index
        .search(&query, k.max(1), SimilarityMetric::Cosine);

    let mut hits = Vec::with_capacity(results.len());
    for r in results {
        if let Some(memory_id) = snapshot.id_to_memory_id.get(&r.id) {
            hits.push(AnnSearchHit {
                memory_id: memory_id.clone(),
                approx_score: r.score,
            });
        }
    }
    Ok(hits)
}

fn load_snapshot_cached(path: &Path) -> Result<Option<AnnSnapshot>, String> {
    if let Ok(cache) = ann_cache().lock() {
        if let Some(snapshot) = cache.get(path).cloned() {
            return Ok(Some(snapshot));
        }
    }
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(path).map_err(|e| format!("failed to read `{}`: {e}", path.display()))?;
    let snapshot: AnnSnapshot = bincode::deserialize(&bytes)
        .map_err(|e| format!("failed to decode ANN index `{}`: {e}", path.display()))?;
    if let Ok(mut cache) = ann_cache().lock() {
        cache.insert(path.to_path_buf(), snapshot.clone());
    }
    Ok(Some(snapshot))
}

fn persist_snapshot(path: &Path, snapshot: &AnnSnapshot) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("failed to create dir `{}`: {e}", parent.display()))?;
    }
    let tmp = tmp_path(path);
    let bytes = bincode::serialize(snapshot).map_err(|e| format!("failed to encode ANN index: {e}"))?;
    std::fs::write(&tmp, &bytes).map_err(|e| format!("failed to write `{}`: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("failed to rename `{}`: {e}", path.display()))?;
    Ok(())
}

fn tmp_path(path: &Path) -> PathBuf {
    let filename = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "index.ann.bin".to_string());
    path.with_file_name(format!("{filename}.tmp"))
}

fn build_snapshot(
    graph: &MemoryGraph,
    expected_embedding_model: Option<&str>,
    expected_embedding_version: Option<&str>,
) -> Result<AnnSnapshot, String> {
    let mut vectors: Vec<(&MemoryEntry, Vec<f64>)> = Vec::new();
    for entry in graph.memories.values() {
        if !entry.active {
            continue;
        }
        let Some(embedding) = entry.embedding.as_ref() else {
            continue;
        };
        if let Some(model) = expected_embedding_model {
            if entry.embedding_model.as_deref() != Some(model) {
                continue;
            }
        }
        if let Some(version) = expected_embedding_version {
            if entry.embedding_version.as_deref() != Some(version) {
                continue;
            }
        }
        let values: Vec<f64> = embedding.iter().map(|v| *v as f64).collect();
        vectors.push((entry, values));
    }

    let dim = vectors.first().map(|(_, v)| v.len()).unwrap_or(0);
    if dim == 0 {
        return Err("cannot build ANN index with zero dimension vectors".to_string());
    }
    vectors.retain(|(_, v)| v.len() == dim);

    let mut index = HNSWIndex::new(dim);
    let mut id_to_memory_id = HashMap::new();
    let mut next_id: u64 = 1;
    for (entry, values) in vectors {
        let id = next_id;
        next_id += 1;
        index
            .add(Vector { id, values, text: String::new(), metadata: None })
            .map_err(|e| format!("failed to add ANN vector: {e}"))?;
        id_to_memory_id.insert(id, entry.id.clone());
    }

    Ok(AnnSnapshot {
        built_at: Utc::now(),
        dim,
        embedding_model: expected_embedding_model.map(|s| s.to_string()),
        embedding_version: expected_embedding_version.map(|s| s.to_string()),
        index,
        id_to_memory_id,
    })
}
