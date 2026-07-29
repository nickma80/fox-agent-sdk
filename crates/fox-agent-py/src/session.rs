//! Session persistence bindings.
//!
//! Exposes FileSessionStore for saving/loading session snapshots.

use fox_agent_core::{FileSessionStore, SessionSnapshot, SessionStore};
use pyo3::prelude::*;
use std::path::PathBuf;
use std::sync::Arc;

// ── PySessionSnapshot (read-only view) ──

/// Read-only snapshot of agent session state.
#[pyclass(name = "SessionSnapshot", module = "fox_agent_sdk._core")]
#[derive(Clone)]
pub struct PySessionSnapshot {
    #[pyo3(get)]
    session_id: String,
    #[pyo3(get)]
    title: Option<String>,
    #[pyo3(get)]
    model: Option<String>,
    #[pyo3(get)]
    status: String,
    #[pyo3(get)]
    working_dir: Option<String>,
    #[pyo3(get)]
    message_count: usize,
    #[pyo3(get)]
    next_turn_id: u64,
    #[pyo3(get)]
    updated_at: u64,
    #[pyo3(get)]
    created_at: u64,
}

impl PySessionSnapshot {
    pub(crate) fn from_snapshot(s: &SessionSnapshot) -> Self {
        Self {
            session_id: s.session_id.clone(),
            title: s.title.clone(),
            model: s.model.clone(),
            status: format!("{:?}", s.status).to_lowercase(),
            working_dir: s.working_dir.as_ref().map(|p| p.to_string_lossy().to_string()),
            message_count: s.messages.len(),
            next_turn_id: s.next_turn_id,
            updated_at: s.updated_at,
            created_at: s.created_at,
        }
    }
}

#[pymethods]
impl PySessionSnapshot {
    fn __repr__(&self) -> String {
        format!(
            "SessionSnapshot(id='{}', status={}, messages={}, turn={})",
            &self.session_id[..8.min(self.session_id.len())],
            self.status,
            self.message_count,
            self.next_turn_id
        )
    }
}

// ── PyFileSessionStore ──

/// File-backed session store.
///
/// Persists sessions as JSON files in a directory.
#[pyclass(name = "FileSessionStore", module = "fox_agent_sdk._core")]
#[derive(Clone)]
pub struct PyFileSessionStore {
    inner: Arc<FileSessionStore>,
}

#[pymethods]
impl PyFileSessionStore {
    /// Create a new file session store rooted at `dir`.
    #[new]
    fn new(dir: String) -> Self {
        Self {
            inner: Arc::new(FileSessionStore::new(PathBuf::from(dir))),
        }
    }

    /// List all stored session IDs.
    fn list_sessions(&self) -> PyResult<Vec<String>> {
        self.inner.list_sessions().map_err(|e| {
            pyo3::exceptions::PyRuntimeError::new_err(format!("failed to list sessions: {}", e))
        })
    }

    /// Load a session snapshot by ID.
    fn load_session(&self, session_id: String) -> PyResult<Option<PySessionSnapshot>> {
        self.inner.load_session(&session_id).map_err(|e| {
            pyo3::exceptions::PyRuntimeError::new_err(format!("failed to load session: {}", e))
        }).map(|opt| opt.as_ref().map(PySessionSnapshot::from_snapshot))
    }

    /// Delete a session by ID.
    fn delete_session(&self, session_id: String) -> PyResult<()> {
        self.inner.delete_session(&session_id).map_err(|e| {
            pyo3::exceptions::PyRuntimeError::new_err(format!("failed to delete session: {}", e))
        })
    }

    fn __repr__(&self) -> String {
        "FileSessionStore()".to_string()
    }
}

impl PyFileSessionStore {
    pub fn into_arc(self) -> Arc<dyn SessionStore> {
        self.inner as Arc<dyn SessionStore>
    }
}
