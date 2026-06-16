//! File persistence layer for MemoryGraph with backup recovery and LRU caching.

use crate::memory::graph::MemoryGraph;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

/// Default base directory for memory storage.
pub fn default_storage_dir() -> PathBuf {
    dirs_or_default().join("memory")
}

fn dirs_or_default() -> PathBuf {
    if let Ok(dir) = std::env::var("FOX_AGENT_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(data_dir) = dirs::data_dir() {
        return data_dir.join("fox-agent");
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".fox-agent")
}

/// Compute a short hash for a project path (for filenames).
pub fn project_hash(project_dir: &Path) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    project_dir.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

// ── JSON read/write with backup recovery ──

/// Read JSON from path. If the file is corrupt, try `.bak` backup.
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, String> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => return Err(format!("failed to read `{}`: {e}", path.display())),
    };
    match serde_json::from_str::<T>(&content) {
        Ok(v) => Ok(v),
        Err(primary_err) => {
            // Try backup
            let backup = path.with_extension("json.bak");
            if backup.exists() {
                let backup_content = std::fs::read_to_string(&backup)
                    .map_err(|e| format!("corrupt primary + backup read failed: {e}"))?;
                match serde_json::from_str::<T>(&backup_content) {
                    Ok(v) => {
                        tracing::warn!(
                            "Recovered memory from backup: {} (primary error: {})",
                            backup.display(),
                            primary_err
                        );
                        return Ok(v);
                    }
                    Err(e) => {
                        return Err(format!(
                            "memory file corrupt (primary: {}, backup: {})",
                            primary_err, e
                        ));
                    }
                }
            }
            Err(format!("memory file corrupt: {}", primary_err))
        }
    }
}

/// Write JSON to path atomically (write to temp, then rename).
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    // Create parent directory
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("failed to create dir `{}`: {e}", parent.display()))?;
    }

    // Create backup of existing file
    if path.exists() {
        let backup = path.with_extension("json.bak");
        let _ = std::fs::copy(path, &backup);
    }

    let json_str = serde_json::to_string_pretty(value).map_err(|e| format!("serialization failed: {e}"))?;

    // Write to temp file, then rename (atomic on most filesystems)
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &json_str).map_err(|e| format!("failed to write `{}`: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("failed to rename `{}`: {e}", path.display()))?;

    Ok(())
}

// ── LRU Cache ──

struct CacheEntry {
    graph: MemoryGraph,
    loaded_at: SystemTime,
}

/// Simple LRU cache for MemoryGraph objects.
pub struct MemoryGraphCache {
    max_entries: usize,
    entries: HashMap<PathBuf, CacheEntry>,
    access_order: Vec<PathBuf>,
}

impl MemoryGraphCache {
    pub fn new(max_entries: usize) -> Self {
        Self {
            max_entries,
            entries: HashMap::new(),
            access_order: Vec::with_capacity(max_entries),
        }
    }

    pub fn get(&mut self, path: &Path) -> Option<&MemoryGraph> {
        if self.entries.contains_key(path) {
            self.touch(path);
            self.entries.get(path).map(|e| &e.graph)
        } else {
            None
        }
    }

    pub fn insert(&mut self, path: PathBuf, graph: MemoryGraph) {
        if self.entries.contains_key(&path) {
            self.entries.get_mut(&path).unwrap().graph = graph;
            self.touch(&path);
            return;
        }
        if self.entries.len() >= self.max_entries {
            if let Some(lru) = self.access_order.first().cloned() {
                self.entries.remove(&lru);
                self.access_order.retain(|p| *p != lru);
            }
        }
        self.entries.insert(path.clone(), CacheEntry {
            graph,
            loaded_at: SystemTime::now(),
        });
        self.access_order.push(path);
    }

    pub fn invalidate(&mut self, path: &Path) {
        self.entries.remove(path);
        self.access_order.retain(|p| p != path);
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.access_order.clear();
    }

    fn touch(&mut self, path: &Path) {
        if let Some(pos) = self.access_order.iter().position(|p| p == path) {
            let p = self.access_order.remove(pos);
            self.access_order.push(p);
        }
    }
}

/// Global graph cache (lazy).
static GRAPH_CACHE: std::sync::OnceLock<Mutex<MemoryGraphCache>> = std::sync::OnceLock::new();

fn graph_cache() -> &'static Mutex<MemoryGraphCache> {
    GRAPH_CACHE.get_or_init(|| Mutex::new(MemoryGraphCache::new(32)))
}

/// Try to get cached graph.
pub fn cached_graph(path: &Path) -> Option<MemoryGraph> {
    graph_cache().lock().ok().and_then(|mut cache| {
        cache.get(path).cloned()
    })
}

/// Update cached graph.
pub fn cache_graph(path: PathBuf, graph: &MemoryGraph) {
    if let Ok(mut cache) = graph_cache().lock() {
        cache.insert(path, graph.clone());
    }
}

/// Invalidate cache entry.
pub fn invalidate_cache(path: &Path) {
    if let Ok(mut cache) = graph_cache().lock() {
        cache.invalidate(path);
    }
}

// ── GC ──

/// Result of a GC operation.
#[derive(Debug)]
pub struct GCResult {
    pub removed_files: usize,
    pub total_scanned: usize,
}

/// Clean up old memory files.
pub fn gc_memory_files(storage_dir: &Path, max_age_hours: u64) -> Result<GCResult, String> {
    // GC global.json + project files
    let now = SystemTime::now();
    let max_age = Duration::from_secs(max_age_hours * 3600);
    let mut removed = 0;
    let mut scanned = 0;

    // GC global
    let global = storage_dir.join("global.json");
    if global.exists() {
        scanned += 1;
        if let Ok(meta) = std::fs::metadata(&global) {
            if let Ok(modified) = meta.modified() {
                if now.duration_since(modified).unwrap_or(Duration::ZERO) > max_age {
                    let backup = global.with_extension("json.bak");
                    let _ = std::fs::remove_file(&global);
                    let _ = std::fs::remove_file(&backup);
                    removed += 1;
                }
            }
        }
    }

    // GC project files
    let projects_dir = storage_dir.join("projects");
    if projects_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&projects_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().map(|e| e == "json").unwrap_or(false) {
                    scanned += 1;
                    if let Ok(meta) = std::fs::metadata(&path) {
                        if let Ok(modified) = meta.modified() {
                            if now.duration_since(modified).unwrap_or(Duration::ZERO) > max_age {
                                let backup = path.with_extension("json.bak");
                                let _ = std::fs::remove_file(&path);
                                let _ = std::fs::remove_file(&backup);
                                invalidate_cache(&path);
                                removed += 1;
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(GCResult { removed_files: removed, total_scanned: scanned })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_write_read_roundtrip() {
        let dir = std::env::temp_dir().join(format!("memory-test-{}", uuid::Uuid::new_v4()));
        let path = dir.join("test.json");

        let mut g = MemoryGraph::new();
        let entry = crate::memory::types::MemoryEntry::new(
            crate::memory::types::MemoryCategory::Fact,
            "hello world",
        );
        g.add_memory(entry);

        write_json(&path, &g).unwrap();
        let loaded: MemoryGraph = read_json(&path).unwrap();
        assert_eq!(loaded.memory_count(), 1);
        assert_eq!(loaded.graph_version, 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_cache_hit() {
        let mut cache = MemoryGraphCache::new(4);
        let p = PathBuf::from("/fake/path.json");
        let g = MemoryGraph::new();

        assert!(cache.get(&p).is_none());
        cache.insert(p.clone(), g);
        assert!(cache.get(&p).is_some());
    }
}
