use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock, RwLock};

use crate::{
    interrupt::InterruptSnapshot, message::Message, model::ModelRuntimeState, PermissionRequest,
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Active,
    Paused,
    Closed,
    Crashed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnvSnapshot {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PendingToolCallSnapshot {
    pub call_id: String,
    pub name: String,
    pub input: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionSnapshot {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_key: Option<String>,
    pub status: SessionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<PathBuf>,
    #[serde(default)]
    pub messages: Vec<Message>,
    /// Complete un-compacted transcript — never truncated.
    /// Persisted separately so session restore and UI display
    /// always see the full conversation history.
    #[serde(default)]
    pub full_messages: Vec<Message>,
    #[serde(default)]
    pub env_snapshots: Vec<EnvSnapshot>,
    #[serde(default)]
    pub model_runtime_state: ModelRuntimeState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_permission: Option<PermissionRequest>,
    #[serde(default)]
    pub pending_tool_calls: Vec<PendingToolCallSnapshot>,
    #[serde(default)]
    pub interrupt_state: InterruptSnapshot,
    pub next_turn_id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    pub updated_at: u64,
    /// Wall-clock creation timestamp (seconds since Unix epoch).
    /// Set once when the session is first created, never changes.
    #[serde(default)]
    pub created_at: u64,
}

pub trait SessionStore: Send + Sync {
    fn save_session(&self, snapshot: &SessionSnapshot) -> Result<(), String>;
    fn load_session(&self, session_id: &str) -> Result<Option<SessionSnapshot>, String>;
    fn delete_session(&self, session_id: &str) -> Result<(), String>;
    fn list_sessions(&self) -> Result<Vec<String>, String>;
}

#[derive(Default)]
pub struct InMemorySessionStore {
    sessions: RwLock<HashMap<String, SessionSnapshot>>,
}

impl SessionStore for InMemorySessionStore {
    fn save_session(&self, snapshot: &SessionSnapshot) -> Result<(), String> {
        self.sessions
            .write()
            .map_err(|_| "session store lock poisoned".to_string())?
            .insert(snapshot.session_id.clone(), snapshot.clone());
        Ok(())
    }

    fn load_session(&self, session_id: &str) -> Result<Option<SessionSnapshot>, String> {
        Ok(self
            .sessions
            .read()
            .map_err(|_| "session store lock poisoned".to_string())?
            .get(session_id)
            .cloned())
    }

    fn delete_session(&self, session_id: &str) -> Result<(), String> {
        self.sessions
            .write()
            .map_err(|_| "session store lock poisoned".to_string())?
            .remove(session_id);
        Ok(())
    }

    fn list_sessions(&self) -> Result<Vec<String>, String> {
        let mut ids: Vec<String> = self
            .sessions
            .read()
            .map_err(|_| "session store lock poisoned".to_string())?
            .keys()
            .cloned()
            .collect();
        ids.sort();
        Ok(ids)
    }
}

pub struct FileSessionStore {
    root_dir: PathBuf,
}

impl FileSessionStore {
    pub fn new(root_dir: impl Into<PathBuf>) -> Self {
        Self {
            root_dir: root_dir.into(),
        }
    }

    fn session_path(&self, session_id: &str) -> PathBuf {
        self.root_dir.join(format!("{session_id}.json"))
    }
}

impl SessionStore for FileSessionStore {
    fn save_session(&self, snapshot: &SessionSnapshot) -> Result<(), String> {
        let path = self.session_path(&snapshot.session_id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create session dir {}: {e}", parent.display()))?;
        }
        let payload = serde_json::to_string_pretty(snapshot)
            .map_err(|e| format!("failed to serialize session snapshot: {e}"))?;
        fs::write(&path, payload)
            .map_err(|e| format!("failed to write session snapshot {}: {e}", path.display()))
    }

    fn load_session(&self, session_id: &str) -> Result<Option<SessionSnapshot>, String> {
        let path = self.session_path(session_id);
        if !path.exists() {
            return Ok(None);
        }
        let payload = fs::read_to_string(&path)
            .map_err(|e| format!("failed to read session snapshot {}: {e}", path.display()))?;
        let snapshot = serde_json::from_str(&payload)
            .map_err(|e| format!("failed to parse session snapshot {}: {e}", path.display()))?;
        Ok(Some(snapshot))
    }

    fn delete_session(&self, session_id: &str) -> Result<(), String> {
        let path = self.session_path(session_id);
        if path.exists() {
            fs::remove_file(&path)
                .map_err(|e| format!("failed to delete session snapshot {}: {e}", path.display()))?;
        }
        Ok(())
    }

    fn list_sessions(&self) -> Result<Vec<String>, String> {
        let dir = self.root_dir.join("sessions");
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut ids = Vec::new();
        for entry in fs::read_dir(&dir)
            .map_err(|e| format!("failed to read session dir {}: {e}", dir.display()))?
        {
            let entry = entry.map_err(|e| format!("failed to inspect session dir entry: {e}"))?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
                ids.push(stem.to_string());
            }
        }
        ids.sort();
        Ok(ids)
    }
}

fn session_store_cell() -> &'static RwLock<Arc<dyn SessionStore>> {
    static STORE: OnceLock<RwLock<Arc<dyn SessionStore>>> = OnceLock::new();
    STORE.get_or_init(|| RwLock::new(Arc::new(InMemorySessionStore::default())))
}

pub fn default_session_store() -> Arc<dyn SessionStore> {
    session_store_cell()
        .read()
        .ok()
        .map(|guard| Arc::clone(&*guard))
        .unwrap_or_else(|| Arc::new(InMemorySessionStore::default()))
}

pub fn set_default_session_store(store: Arc<dyn SessionStore>) {
    if let Ok(mut guard) = session_store_cell().write() {
        *guard = store;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::now_secs;

    #[test]
    fn file_session_store_roundtrip_persists_snapshot() {
        let root = std::env::temp_dir().join(format!("fox-session-store-{}", uuid::Uuid::new_v4()));
        let store = FileSessionStore::new(root.clone());
        let snapshot = SessionSnapshot {
            session_id: "s1".to_string(),
            parent_id: None,
            title: Some("test".to_string()),
            model: Some("mock-1".to_string()),
            provider_key: None,
            status: SessionStatus::Active,
            working_dir: None,
            messages: vec![Message::user("hello")],
            full_messages: vec![Message::user("hello")],
            env_snapshots: Vec::new(),
            model_runtime_state: ModelRuntimeState::default(),
            pending_permission: None,
            pending_tool_calls: Vec::new(),
            interrupt_state: InterruptSnapshot::default(),
            next_turn_id: 2,
            metadata: None,
            updated_at: now_secs(),
            created_at: now_secs(),
        };
        store.save_session(&snapshot).unwrap();

        let loaded = store.load_session("s1").unwrap().unwrap();
        assert_eq!(loaded.session_id, "s1");
        assert_eq!(loaded.messages.len(), 1);

        let _ = std::fs::remove_dir_all(root);
    }
}
