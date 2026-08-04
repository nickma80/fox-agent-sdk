//! Memory system bindings for cross-session learning.
//!
//! Exposes MemoryManager, MemoryConfig, MemoryEntry, and recall/search APIs
//! to Python developers.

use fox_agent_core::{
    MemoryCategory, MemoryConfig, MemoryEntry, MemoryManager as CoreMemoryManager, MemoryScope,
    RecallMode,
};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::path::PathBuf;

// ── PyMemoryConfig ──

/// Python binding for MemoryConfig.
///
/// Controls all aspects of the memory system: enabling, LLM-wiki recall,
/// auto-extraction, injection limits, deduplication, and retention.
#[pyclass(name = "MemoryConfig", module = "fox_agent_sdk._core")]
#[derive(Clone)]
pub struct PyMemoryConfig {
    inner: MemoryConfig,
}

#[pymethods]
impl PyMemoryConfig {
    /// Create with sensible defaults (memory disabled).
    #[new]
    #[pyo3(signature = (
        *,
        enabled = false,
        auto_extract = false,
        auto_extract_scope = "Project",
        auto_extract_message_window = 6,
        auto_extract_max_items_per_turn = 4,
        wiki_enabled = true,
        max_results = 10,
        injection_max_chars = 1500,
        injection_max_per_category = 3,
        verify_relevance = false,
        retention_days = 0,
        memory_size_limit = 10000,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        enabled: bool,
        auto_extract: bool,
        auto_extract_scope: &str,
        auto_extract_message_window: usize,
        auto_extract_max_items_per_turn: usize,
        wiki_enabled: bool,
        max_results: usize,
        injection_max_chars: usize,
        injection_max_per_category: usize,
        verify_relevance: bool,
        retention_days: u32,
        memory_size_limit: usize,
    ) -> PyResult<Self> {
        let scope = match auto_extract_scope {
            "Session" => fox_agent_core::AutoExtractScope::Session,
            "Project" => fox_agent_core::AutoExtractScope::Project,
            "Global" => fox_agent_core::AutoExtractScope::Global,
            other => {
                return Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "unknown auto_extract_scope '{}', expected 'Session', 'Project', or 'Global'",
                    other
                )));
            }
        };

        Ok(Self {
            inner: MemoryConfig {
                enabled,
                auto_extract,
                auto_extract_scope: scope,
                auto_extract_message_window,
                auto_extract_max_items_per_turn,
                wiki_enabled,
                max_results,
                injection_max_chars,
                injection_max_per_category,
                verify_relevance,
                retention_days: if retention_days > 0 {
                    Some(retention_days as u64)
                } else {
                    None
                },
                memory_size_limit: if memory_size_limit > 0 {
                    Some(memory_size_limit)
                } else {
                    None
                },
                ..Default::default()
            },
        })
    }

    #[getter]
    fn enabled(&self) -> bool {
        self.inner.enabled
    }

    fn __repr__(&self) -> String {
        format!(
            "MemoryConfig(enabled={}, auto_extract={}, max_results={})",
            self.inner.enabled, self.inner.auto_extract, self.inner.max_results
        )
    }
}

impl PyMemoryConfig {
    pub fn into_inner(self) -> MemoryConfig {
        self.inner
    }
}

// ── PyMemoryScope ──

/// Scope constants for memory operations.
#[pyclass(name = "MemoryScope", module = "fox_agent_sdk._core")]
#[derive(Clone)]
pub struct PyMemoryScope;

#[pymethods]
impl PyMemoryScope {
    #[classattr]
    const SESSION: &'static str = "session";
    #[classattr]
    const PROJECT: &'static str = "project";
    #[classattr]
    const GLOBAL: &'static str = "global";
    #[classattr]
    const ALL: &'static str = "all";
}

fn parse_scope(s: &str) -> PyResult<MemoryScope> {
    match s {
        "session" => Ok(MemoryScope::Session),
        "project" => Ok(MemoryScope::Project),
        "global" => Ok(MemoryScope::Global),
        "all" => Ok(MemoryScope::All),
        other => Err(pyo3::exceptions::PyValueError::new_err(format!(
            "unknown scope '{}', expected 'session', 'project', 'global', or 'all'",
            other
        ))),
    }
}

// ── PyRecallMode ──

/// Recall mode constants.
#[pyclass(name = "RecallMode", module = "fox_agent_sdk._core")]
#[derive(Clone)]
pub struct PyRecallMode;

#[pymethods]
impl PyRecallMode {
    #[classattr]
    const RECENT: &'static str = "recent";
    #[classattr]
    const KEYWORD: &'static str = "keyword";
    #[classattr]
    const WIKI: &'static str = "wiki";
}

fn parse_mode(s: &str) -> PyResult<RecallMode> {
    match s {
        "recent" => Ok(RecallMode::Recent),
        "keyword" => Ok(RecallMode::Keyword),
        "wiki" => Ok(RecallMode::Wiki),
        other => Err(pyo3::exceptions::PyValueError::new_err(format!(
            "unknown mode '{}', expected 'recent', 'keyword', or 'wiki'",
            other
        ))),
    }
}

// ── MemoryManager ──

/// Python binding for the core MemoryManager.
///
/// Provides CRUD + recall + search for cross-session learning.
#[pyclass(name = "MemoryManager", module = "fox_agent_sdk._core")]
#[derive(Clone)]
pub struct PyMemoryManager {
    inner: CoreMemoryManager,
}

#[pymethods]
impl PyMemoryManager {
    /// Create a new MemoryManager with the given config.
    #[new]
    fn new(config: PyMemoryConfig) -> Self {
        Self {
            inner: CoreMemoryManager::new(&config.into_inner()),
        }
    }

    /// Set the storage directory.
    fn with_storage_dir(&self, dir: String) -> Self {
        Self {
            inner: self.inner.clone().with_storage_dir(PathBuf::from(dir)),
        }
    }

    /// Set the project directory.
    fn with_project_dir(&self, dir: String) -> Self {
        Self {
            inner: self.inner.clone().with_project_dir(PathBuf::from(dir)),
        }
    }

    /// Set the session ID for session-scoped isolation.
    fn with_session_id(&self, id: String) -> Self {
        Self {
            inner: self.inner.clone().with_session_id(id),
        }
    }

    /// Whether wiki (LLM-based) recall is enabled.
    fn wiki_enabled(&self) -> bool {
        self.inner.wiki_enabled()
    }

    /// Store a memory entry.
    ///
    /// Returns the memory ID on success.
    #[pyo3(signature = (content, *, category = "fact", scope = "project"))]
    fn remember(&self, content: String, category: &str, scope: &str) -> PyResult<String> {
        let cat = match category {
            "fact" => MemoryCategory::Fact,
            "preference" => MemoryCategory::Preference,
            "entity" => MemoryCategory::Entity,
            "correction" => MemoryCategory::Correction,
            "narrative" => MemoryCategory::Narrative,
            other => MemoryCategory::Custom(other.to_string()),
        };
        let scope = parse_scope(scope)?;

        let entry = MemoryEntry::new(cat, content);
        self.inner.remember(entry, scope).map_err(|e| {
            pyo3::exceptions::PyRuntimeError::new_err(format!("failed to store memory: {}", e))
        })
    }

    /// Recall memories relevant to a query.
    #[pyo3(signature = (query, *, limit = 10, mode = "keyword", scope = "all"))]
    fn recall(
        &self,
        query: Option<String>,
        limit: usize,
        mode: &str,
        scope: &str,
        py: Python<'_>,
    ) -> PyResult<Py<PyDict>> {
        let mode = parse_mode(mode)?;
        let scope = parse_scope(scope)?;

        let results = self
            .inner
            .recall_detailed(query.as_deref(), limit, mode, scope)
            .map_err(|e| {
                pyo3::exceptions::PyRuntimeError::new_err(format!("recall failed: {}", e))
            })?;

        let dict = PyDict::new(py);
        let entries = pyo3::types::PyList::empty(py);
        for hit in &results {
            let entry_dict = PyDict::new(py);
            entry_dict.set_item("id", &hit.entry.id)?;
            entry_dict.set_item("content", &hit.entry.content)?;
            entry_dict.set_item("category", hit.entry.category.to_string())?;
            entry_dict.set_item("score", hit.score_breakdown.final_score)?;
            entry_dict.set_item("trust", format!("{:?}", hit.entry.trust))?;
            if !hit.entry.tags.is_empty() {
                entry_dict.set_item("tags", hit.entry.tags.clone())?;
            }
            entries.append(entry_dict)?;
        }
        let recall_count = entries.len();
        dict.set_item("results", entries)?;
        dict.set_item("count", recall_count)?;

        Ok(dict.into())
    }

    /// Simple keyword search across memories.
    #[pyo3(signature = (text, *, scope = "all"))]
    fn search(&self, text: String, scope: &str, py: Python<'_>) -> PyResult<Py<PyDict>> {
        let scope = parse_scope(scope)?;

        let results = self.inner.search(&text, scope).map_err(|e| {
            pyo3::exceptions::PyRuntimeError::new_err(format!("search failed: {}", e))
        })?;

        let dict = PyDict::new(py);
        let entries = pyo3::types::PyList::empty(py);
        for entry in &results {
            let entry_dict = PyDict::new(py);
            entry_dict.set_item("id", &entry.id)?;
            entry_dict.set_item("content", &entry.content)?;
            entry_dict.set_item("category", entry.category.to_string())?;
            entries.append(entry_dict)?;
        }
        let search_count = entries.len();
        dict.set_item("results", entries)?;
        dict.set_item("count", search_count)?;

        Ok(dict.into())
    }

    /// List all memories in a scope.
    #[pyo3(signature = (scope = "all"))]
    fn list(&self, scope: &str, py: Python<'_>) -> PyResult<Py<PyDict>> {
        let scope = parse_scope(scope)?;

        let results = self.inner.list(scope).map_err(|e| {
            pyo3::exceptions::PyRuntimeError::new_err(format!("list failed: {}", e))
        })?;

        let dict = PyDict::new(py);
        let entries = pyo3::types::PyList::empty(py);
        for entry in &results {
            let entry_dict = PyDict::new(py);
            entry_dict.set_item("id", &entry.id)?;
            entry_dict.set_item("content", &entry.content)?;
            entry_dict.set_item("category", entry.category.to_string())?;
            entry_dict.set_item("active", entry.active)?;
            entries.append(entry_dict)?;
        }
        let list_count = entries.len();
        dict.set_item("results", entries)?;
        dict.set_item("count", list_count)?;

        Ok(dict.into())
    }

    /// Forget (deactivate) a memory by ID.
    fn forget(&self, id: String) -> PyResult<bool> {
        self.inner
            .forget(&id)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("forget failed: {}", e)))
    }

    /// Rebuild the wiki index for a scope (PRD §8.1).
    ///
    /// The index is a derived projection (id/title/summary/tags/aliases)
    /// rebuilt lazily from the current graph(s).
    #[pyo3(signature = (scope = "all"))]
    fn rebuild_index(&self, scope: &str, py: Python<'_>) -> PyResult<Py<PyDict>> {
        let scope = parse_scope(scope)?;
        let index = self.inner.rebuild_index(scope).map_err(|e| {
            pyo3::exceptions::PyRuntimeError::new_err(format!("rebuild_index failed: {}", e))
        })?;
        let dict = PyDict::new(py);
        dict.set_item("entries", index.entries.len())?;
        Ok(dict.into())
    }

    /// Batch-enrich `enriched=false` memories via the wiki assistant (PRD §8.1).
    ///
    /// `limit == 0` means unlimited.  Returns the number of entries enriched;
    /// requires the manager to have a wiki assistant attached (otherwise 0).
    #[pyo3(signature = (scope = "all", limit = 0))]
    fn enrich<'py>(
        &self,
        scope: &str,
        limit: usize,
        py: Python<'py>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let scope = parse_scope(scope)?;
        let inner = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py::<_, Py<PyAny>>(py, async move {
            let count = inner.backfill_enrich(scope, limit).await.map_err(|e| {
                pyo3::exceptions::PyRuntimeError::new_err(format!("enrich failed: {}", e))
            })?;
            Python::with_gil(|py| {
                let dict = PyDict::new(py);
                dict.set_item("count", count)?;
                Ok(dict.into_any().unbind())
            })
        })
    }

    fn __repr__(&self) -> String {
        format!("MemoryManager(wiki={})", self.inner.wiki_enabled())
    }
}
